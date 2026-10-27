//! Repository chores that are not part of the shipped binary.
//!
//! `cargo xtask fix-doc-counts` rewrites every tool and toolset count in the
//! documentation from `router::registry`, which is the only source of truth.
//!
//! Why this exists: `doc_tool_counts` requires roughly seven documents to
//! quote the catalogue totals, and `tool-directory.md` to carry a per-toolset
//! heading. Every PR that adds a tool therefore edits the same handful of
//! lines, so any two such PRs conflict by construction — and a release that
//! moves the counts conflicts with the whole open queue at once. v0.10.0 did
//! exactly that to eleven open pull requests. The guard stays; the
//! hand-editing does not.
//!
//! `cargo xtask collect-api-migrations <version>` folds the unreleased notes in
//! `docs/api-migrations/` into `docs/API_MIGRATIONS.md`; see `api_migrations`.

mod api_migrations;

use konnect_core::router::{meta_tools, registry};
use std::path::{Path, PathBuf};

const SKIP: &[&str] = &["target", "node_modules", ".git", ".claude", "dist", "build"];
const NL: char = '\n';

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("fix-doc-counts") => fix_doc_counts(),
        Some("collect-api-migrations") => api_migrations::collect(args.get(2).map(String::as_str)),
        other => {
            eprintln!("unknown task: {other:?}");
            eprintln!("usage: cargo xtask fix-doc-counts");
            eprintln!("       cargo xtask collect-api-migrations vMAJOR.MINOR.PATCH");
            std::process::exit(2);
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the repo root")
        .to_path_buf()
}

fn text_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !SKIP.contains(&name.as_ref()) {
                    stack.push(path);
                }
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("md") | Some("json")
            ) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Numbers written immediately before `word`, read exactly as the guard reads
/// them. Returns `(byte offset of the first digit, value)`.
fn counts_before(line: &str, word: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (at, _) in line.match_indices(word) {
        let before = line[..at].trim_end();
        let digits: String = before
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect();
        if digits.is_empty() {
            continue;
        }
        let start = before.len() - digits.len();
        if let Ok(n) = digits.chars().rev().collect::<String>().parse::<usize>() {
            out.push((start, n));
        }
    }
    out
}

fn fix_doc_counts() {
    let toolsets = registry::ALL_TOOLSETS.len();
    let registered: usize = registry::ALL_TOOLSETS.iter().map(|t| t.tool_count).sum();
    let meta = meta_tools::meta_tool_descriptions().len();
    let total = registered + meta;
    let root = repo_root();
    let files = text_files(&root);

    // Which stale value means "registered" and which means "total" is never
    // guessed: a catalogue pair always differs by exactly the meta-tool count,
    // so the pair identifies itself. If the documents do not present such a
    // pair, refuse rather than write the wrong number into every file.
    let mut stale: Vec<usize> = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines() {
            // Detect over the same three spellings the rewrite handles.
            // Scanning only "N tools" would make a half-applied run look
            // finished: the "N registered" and "N total" forms would survive
            // and the next invocation would find nothing stale to key on.
            for word in ["tools", "registered", "total"] {
                for (_, n) in counts_before(line, word) {
                    if n >= 100 && n != registered && n != total && !stale.contains(&n) {
                        stale.push(n);
                    }
                }
            }
        }
    }
    stale.sort_unstable();

    let (old_registered, old_total) = match stale.as_slice() {
        [] => (registered, total),
        [a, b] if b - a == meta => (*a, *b),
        [a] if *a + meta == total => (*a, total),
        [a] if *a == total.saturating_sub(0) => (registered, *a),
        [a] if a.checked_sub(meta) == Some(registered) => (registered, *a),
        other => {
            eprintln!(
                "refusing to guess: catalogue totals {other:?} do not form a \
                 registered/total pair differing by {meta}. Fix by hand, then re-run."
            );
            std::process::exit(1);
        }
    };

    let mut changed: Vec<String> = Vec::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let is_directory = path.file_name().is_some_and(|f| f == "tool-directory.md");
        let mut touched = false;
        let mut out: Vec<String> = Vec::new();

        for raw in text.lines() {
            let mut line = raw.to_string();

            // Catalogue totals. The guard spells them three ways — "N tools",
            // "N registered", "N total" — so all three are rewritten, and only
            // ever when the number is one of the two known previous values.
            // That restriction is what makes this safe to run over every
            // document in the repository: an unrelated 217 is left alone.
            for word in ["tools", "registered", "total"] {
                let mut hits = counts_before(&line, word);
                hits.reverse();
                for (start, n) in hits {
                    if n < 100 {
                        continue;
                    }
                    let replacement = if n == old_registered && registered != n {
                        Some(registered)
                    } else if n == old_total && total != n {
                        Some(total)
                    } else {
                        None
                    };
                    if let Some(v) = replacement {
                        line.replace_range(start..start + n.to_string().len(), &v.to_string());
                        touched = true;
                    }
                }
            }

            // Toolset counts, any width.
            let mut hits = counts_before(&line, "toolsets");
            hits.reverse();
            for (start, n) in hits {
                if n != toolsets {
                    line.replace_range(start..start + n.to_string().len(), &toolsets.to_string());
                    touched = true;
                }
            }

            if is_directory {
                if let Some(fixed) = rewrite_section_heading(&line) {
                    line = fixed;
                    touched = true;
                }
            }

            out.push(line);
        }

        if touched {
            // Preserve whatever line endings the file already had. Rewriting a
            // CRLF document as LF churns every line in a Windows contributor's
            // working tree for a one-digit change — and this repository has
            // already shipped one line-ending regression (#352).
            let crlf = text.contains("\r\n");
            let sep = if crlf { "\r\n" } else { "\n" };
            let mut joined = out.join(sep);
            if text.ends_with(NL) {
                joined.push_str(sep);
            }
            std::fs::write(path, joined).expect("write");
            changed.push(
                path.strip_prefix(&root)
                    .unwrap_or(path)
                    .display()
                    .to_string()
                    .replace('\\', "/"),
            );
        }
    }

    println!("registry: {toolsets} toolsets, {registered} registered, {total} total");
    if changed.is_empty() {
        println!("documentation already current; nothing to do");
    } else {
        println!("rewrote {} file(s):", changed.len());
        for c in &changed {
            println!("  {c}");
        }
    }
}

/// A `### \`toolset\` · N tools` heading whose count disagrees with the
/// registry. Returns the corrected line, or `None` when it already agrees or
/// the line is not a toolset heading.
fn rewrite_section_heading(line: &str) -> Option<String> {
    let rest = line.strip_prefix("### `")?;
    let (name, tail) = rest.split_once('`')?;
    let meta = registry::ALL_TOOLSETS.iter().find(|ts| ts.name == name)?;
    let claimed = tail
        .split_whitespace()
        .find_map(|w| w.parse::<usize>().ok())?;
    if claimed == meta.tool_count {
        return None;
    }
    let needle = claimed.to_string();
    let at = tail.find(&needle)?;
    Some(format!(
        "### `{name}`{}{}{}",
        &tail[..at],
        meta.tool_count,
        &tail[at + needle.len()..]
    ))
}
