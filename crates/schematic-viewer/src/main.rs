#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

//! Copperpour — Live Schematic Viewer
//!
//! Watches a .kicad_sch file (and, for hierarchical designs, every sheet
//! reachable from it), renders each to SVG via kicad-cli, and displays them
//! in a native window with pan/zoom, a sheet selector, and auto-refresh.
//!
//! Usage: schematic-viewer [--kicad-cli <path>] [path/to/file.kicad_sch]

use copperpour_viewer::*;
use notify::{EventKind, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::Receiver;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

// ─── State ──────────────────────────────────────────────────────────────────

struct ViewerState {
    root_path: Mutex<Option<PathBuf>>,
    active_file: Mutex<Option<PathBuf>>,
    sheets: Mutex<Vec<SheetEntry>>,
    /// Rendered SVG per sheet, keyed by `SheetEntry::file`. All sheets are
    /// rendered eagerly on open/refresh so switching sheets is instant.
    svg_cache: Mutex<HashMap<String, String>>,
    /// Directories the live watcher currently covers — diffed against a
    /// fresh `compute_watch_dirs` on every change so sheets added in a new
    /// folder get picked up without reopening the viewer.
    watched_dirs: Mutex<HashSet<PathBuf>>,
    kicad_cli: Mutex<String>,
    /// File passed on the command line, handed to the frontend when it asks.
    startup_file: Mutex<Option<String>>,
    /// The active file watcher. Replacing it drops (and stops) the previous
    /// one, so only the currently-open design is ever watched.
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
}

// ─── Tauri Commands ─────────────────────────────────────────────────────────

/// The file passed on the command line, if any. The frontend calls this once
/// its scripts are ready — no timing games with `window.eval`.
#[tauri::command]
fn get_startup_file(state: tauri::State<'_, ViewerState>) -> Option<String> {
    state.startup_file.lock().unwrap().take()
}

#[derive(serde::Serialize)]
struct OpenResult {
    svg: String,
    sheets: Vec<SheetEntry>,
    active_file: String,
    render_errors: Vec<String>,
}

/// Payload for the `sheets-updated` event, emitted by the file watcher when
/// a re-walk finds the tree changed. Carries `active_file` alongside the new
/// list so the frontend's selector can stay in sync even when the
/// previously active sheet vanished and the backend fell back to root.
#[derive(serde::Serialize)]
struct SheetsUpdated {
    sheets: Vec<SheetEntry>,
    active_file: String,
}

#[tauri::command]
fn open_schematic(
    app: AppHandle,
    state: tauri::State<'_, ViewerState>,
    path: String,
) -> Result<OpenResult, String> {
    let root = PathBuf::from(&path);
    if !root.exists() {
        return Err(format!("File not found: {}", path));
    }

    let cli = state.kicad_cli.lock().unwrap().clone();
    let sheets = walk_sheet_tree(&root);
    let (cache, errors) = render_all(&cli, &root, &sheets);

    let active_key = root.to_string_lossy().to_string();
    let svg = cache
        .get(&active_key)
        .cloned()
        .ok_or_else(|| format!("Failed to render root schematic: {}", errors.join("; ")))?;

    if let Some(window) = app.get_webview_window("main") {
        let name = root.file_name().unwrap_or_default().to_string_lossy();
        let _ = window.set_title(&format!("{} — Schematic Viewer", name));
    }

    let watch_dirs = compute_watch_dirs(&sheets);
    let watcher = build_watcher(app.clone(), cli, root.clone())?;

    *state.root_path.lock().unwrap() = Some(root.clone());
    *state.active_file.lock().unwrap() = Some(root);
    *state.sheets.lock().unwrap() = sheets.clone();
    *state.svg_cache.lock().unwrap() = cache;
    *state.watched_dirs.lock().unwrap() = watch_dirs;
    *state.watcher.lock().unwrap() = Some(watcher);

    Ok(OpenResult {
        svg,
        sheets,
        active_file: active_key,
        render_errors: errors,
    })
}

/// Switch which sheet is displayed, without re-rendering — all sheets were
/// already rendered eagerly by `open_schematic`/the watcher, so this is
/// just a cache lookup.
#[tauri::command]
fn select_sheet(state: tauri::State<'_, ViewerState>, file: String) -> Result<String, String> {
    let known = state.sheets.lock().unwrap().iter().any(|e| e.file == file);
    if !known {
        return Err(format!(
            "'{}' is not a known sheet in the current design",
            file
        ));
    }
    let svg = state
        .svg_cache
        .lock()
        .unwrap()
        .get(&file)
        .cloned()
        .ok_or_else(|| "This sheet failed to render — see the earlier error".to_string())?;
    *state.active_file.lock().unwrap() = Some(PathBuf::from(&file));
    Ok(svg)
}

#[tauri::command]
fn refresh(state: tauri::State<'_, ViewerState>) -> Result<String, String> {
    let active = state
        .active_file
        .lock()
        .unwrap()
        .clone()
        .ok_or("No schematic loaded")?;
    let cli = state.kicad_cli.lock().unwrap().clone();
    // Same snapshot isolation as render_all, scoped to the active sheet's
    // subtree (children must be present for kicad-cli to load the sheet).
    let entries = walk_sheet_tree(&active);
    let batch_dir = render_batch_dir();
    let snapshot = snapshot_tree(&active, &entries, &batch_dir.join("src"));
    let key = active.to_string_lossy().to_string();
    let src = snapshot
        .get(&key)
        .cloned()
        .unwrap_or_else(|| active.clone());
    let result = render_to_svg_in(&cli, &src, &batch_dir.join("out"));
    let _ = std::fs::remove_dir_all(&batch_dir);
    let svg = result?;
    state.svg_cache.lock().unwrap().insert(key, svg.clone());
    Ok(svg)
}

/// Opens the currently *active* sheet in KiCAD — not necessarily the root —
/// so "Open in KiCAD" matches whatever the user is actually looking at.
#[tauri::command]
fn open_in_kicad(state: tauri::State<'_, ViewerState>) -> Result<(), String> {
    let root = state
        .root_path
        .lock()
        .unwrap()
        .clone()
        .ok_or("No schematic loaded")?;
    let active = state
        .active_file
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| root.clone());

    // kicad.exe (the full GUI app, as opposed to kicad-cli) expects a
    // .kicad_pro project file — passing a bare .kicad_sch produces "does
    // not appear to be a KiCad project file". Prefer the project file
    // (standard KiCad convention: same file stem as the root schematic,
    // same directory) when it exists; otherwise fall back to opening the
    // sheet directly, which KiCad can still do for a standalone schematic
    // with no project file. Note: this opens the *project*, not
    // necessarily jumping straight to whichever sub-sheet is active in the
    // viewer — there's no reliable way to make kicad.exe do that from the
    // command line.
    let project = root.with_extension("kicad_pro");
    let target = if project.exists() { project } else { active };

    // KiCAD marks an open project/file with a `~<name>.lck` file in the
    // same directory (confirmed against a real KiCAD 10 session). Spawning
    // kicad.exe against an already-locked project produces a warning dialog
    // that, if declined, still leaves a new blank KiCAD window running —
    // checking first avoids that stray window entirely. Known tradeoff: a
    // stale lock left behind by a crashed KiCAD would incorrectly block a
    // legitimate open; not attempting to validate lock freshness here.
    if kicad_lock_path(&target).exists() {
        let name = target.file_name().unwrap_or_default().to_string_lossy();
        return Err(format!(
            "'{}' is already open in another KiCAD window — switch to that window instead of opening a new one.",
            name
        ));
    }

    Command::new(resolve_kicad_binary())
        .arg(&target)
        .spawn()
        .map_err(|e| format!("Failed to launch KiCAD: {}", e))?;
    Ok(())
}

// ─── File Watcher ───────────────────────────────────────────────────────────

/// Build a watcher covering every sheet's parent directory. On any relevant
/// change, re-walks the whole tree (not just re-renders one file) — an edit
/// to the root could add or remove a sheet, so "what changed" always means
/// "re-discover the tree, then re-render everything in it." Hierarchical
/// designs are small in practice, so eagerly re-rendering all sheets on
/// every change is simpler and safer than tracking per-sheet staleness.
fn build_watcher(
    app: AppHandle,
    cli: String,
    root: PathBuf,
) -> Result<notify::RecommendedWatcher, String> {
    let initial_dirs = compute_watch_dirs(&walk_sheet_tree(&root));

    // All slow work happens on a dedicated worker thread, never in the
    // notify callback below — see spawn_render_worker for why that split is
    // load-bearing, not just tidy.
    let (tx, rx) = std::sync::mpsc::channel();
    spawn_render_worker(app, cli, root, rx);

    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            let Ok(event) = res else { return };

            match event.kind {
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {}
                _ => return,
            }

            let changed: Vec<PathBuf> = event
                .paths
                .iter()
                .filter(|p| is_relevant_sch_change(p))
                .cloned()
                .collect();
            if changed.is_empty() {
                return;
            }

            let _ = tx.send(changed);
        })
        .map_err(|e| format!("Failed to create file watcher: {}", e))?;

    for dir in &initial_dirs {
        watcher
            .watch(dir, RecursiveMode::NonRecursive)
            .map_err(|e| format!("Failed to watch {}: {}", dir.display(), e))?;
    }

    Ok(watcher)
}

/// The render worker owns everything slow: it collapses each burst of file
/// events into a single re-walk + re-render once the burst goes quiet.
///
/// Keeping this off the notify callback is load-bearing on Windows: while
/// the callback runs, directory changes queue in a small fixed OS buffer
/// (ReadDirectoryChangesW). A multi-second render inside the callback
/// overflows that buffer and silently drops every event that arrives
/// meanwhile — the watcher then looks permanently "deaf" even though
/// nothing errored. The callback must return in microseconds; it forwards
/// each event's relevant paths through `rx` and nothing more.
///
/// The worker exits when `rx` disconnects, which happens exactly when the
/// watcher holding the sender is dropped (a new design was opened, or the
/// viewer is shutting down).
fn spawn_render_worker(app: AppHandle, cli: String, root: PathBuf, rx: Receiver<Vec<PathBuf>>) {
    std::thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            // Surface "the watcher noticed" immediately, before the quiet
            // wait and the render — both take a while on a big design.
            // Without this there is no way for the user to tell "the edit
            // was never seen" apart from "seen, but the render failed".
            let _ = app.emit("change-detected", ());

            let mut changed = HashSet::new();
            note_changed(&mut changed, first);
            if !drain_until_quiet(&rx, QUIET_WINDOW, MAX_BURST_WAIT, &mut changed) {
                return; // watcher dropped mid-burst — this design was closed
            }

            let state = app.state::<ViewerState>();

            // Stale guard: if the user opened a different design while this
            // burst was in flight, drop it.
            {
                let current_root = state.root_path.lock().unwrap().clone();
                if current_root.as_deref() != Some(root.as_path()) {
                    return;
                }
            }

            let new_sheets = walk_sheet_tree(&root);

            // Re-render only what this burst touched (plus anything not yet
            // cached); untouched sheets keep their existing SVGs. On a big
            // design this turns save-to-screen latency from "render the
            // whole board" into "render the sheets you edited".
            let old_cache = state.svg_cache.lock().unwrap().clone();
            let need = files_needing_render(&new_sheets, &changed, &old_cache);
            let (fresh, errors) = render_some(&cli, &root, &new_sheets, Some(&need));

            // Merge: freshly rendered wins; sheets still in the tree keep
            // their old SVG; sheets that left the tree drop out. A sheet
            // that needed a render but failed stays absent, matching
            // render_all's contract.
            let mut cache: HashMap<String, String> = HashMap::new();
            for e in &new_sheets {
                if let Some(svg) = fresh.get(&e.file) {
                    cache.insert(e.file.clone(), svg.clone());
                } else if !need.contains(&e.file) {
                    if let Some(svg) = old_cache.get(&e.file) {
                        cache.insert(e.file.clone(), svg.clone());
                    }
                }
            }

            // If the previously active sheet vanished from the tree (its
            // sheet was deleted), fall back to the root.
            let (active_key, active_switched) = {
                let mut active = state.active_file.lock().unwrap();
                let prev = active.as_ref().map(|a| a.to_string_lossy().to_string());
                let still_valid = active
                    .as_ref()
                    .map(|a| new_sheets.iter().any(|e| e.file == a.to_string_lossy()))
                    .unwrap_or(false);
                if !still_valid {
                    *active = Some(root.clone());
                }
                let key = active.as_ref().unwrap().to_string_lossy().to_string();
                let switched = prev.as_deref() != Some(key.as_str());
                (key, switched)
            };

            // Re-derive the watch set and diff it against what's currently
            // watched, so a sheet added in a brand-new folder gets covered
            // without requiring the user to reopen the viewer.
            let new_dirs = compute_watch_dirs(&new_sheets);
            if let Some(w) = state.watcher.lock().unwrap().as_mut() {
                let mut watched = state.watched_dirs.lock().unwrap();
                for dir in new_dirs.difference(&watched) {
                    let _ = w.watch(dir, RecursiveMode::NonRecursive);
                }
                for dir in watched.difference(&new_dirs) {
                    let _ = w.unwatch(dir);
                }
                *watched = new_dirs;
            }

            *state.sheets.lock().unwrap() = new_sheets.clone();
            *state.svg_cache.lock().unwrap() = cache.clone();

            let _ = app.emit(
                "sheets-updated",
                &SheetsUpdated {
                    sheets: new_sheets,
                    active_file: active_key.clone(),
                },
            );
            // Push the active sheet only when its content actually changed
            // (it was re-rendered) or the view must switch (fallback to
            // root after a delete); re-pushing an identical SVG would
            // pointlessly redraw and flash "updated" for edits that only
            // touched other sheets.
            if active_switched || need.contains(&active_key) {
                if let Some(svg) = cache.get(&active_key) {
                    let _ = app.emit("schematic-updated", svg);
                }
            }
            for e in &errors {
                let _ = app.emit("viewer-error", format!("Render failed: {}", e));
            }
        }
    });
}

// ─── Main ───────────────────────────────────────────────────────────────────

fn main() {
    // Minimal arg parsing: [--kicad-cli <path>] [schematic-file]
    let mut kicad_cli_override: Option<String> = None;
    let mut file_arg: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--kicad-cli" {
            kicad_cli_override = args.next();
        } else if !a.starts_with('-') {
            file_arg = Some(a);
        }
    }

    let state = ViewerState {
        root_path: Mutex::new(None),
        active_file: Mutex::new(None),
        sheets: Mutex::new(Vec::new()),
        svg_cache: Mutex::new(HashMap::new()),
        watched_dirs: Mutex::new(HashSet::new()),
        kicad_cli: Mutex::new(resolve_kicad_cli(kicad_cli_override)),
        startup_file: Mutex::new(file_arg),
        watcher: Mutex::new(None),
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_startup_file,
            open_schematic,
            select_sheet,
            refresh,
            open_in_kicad
        ])
        .build(tauri::generate_context!())
        .expect("error while building schematic viewer")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                // Best-effort cleanup of this instance's rendered SVGs
                let _ = std::fs::remove_dir_all(render_temp_dir());
            }
        });
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #[test]
    fn tauri_identifier_is_copperpours() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            config["identifier"],
            "com.github.copperpour.copperpour.viewer"
        );
    }
}
