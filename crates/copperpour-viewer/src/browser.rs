//! Loopback browser transport for the shared viewer engine.
//!
//! Instances own one root, their listener, watcher and render worker. HTTP
//! clients can select cached sheets, never supply paths to read or execute.
use axum::{
    extract::{DefaultBodyLimit, Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use notify::{EventKind, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    sync::{mpsc, oneshot, watch, RwLock},
    task::JoinHandle,
};
use tokio_stream::{wrappers::WatchStream, StreamExt};

const PAGE: &str = include_str!("../../schematic-viewer/frontend/index.html");
const SCRIPT: &str = include_str!("../../schematic-viewer/frontend/viewer.js");
const PAGE_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; connect-src 'self'; frame-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const SVG_CSP: &str = "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'";

#[derive(Clone, Serialize)]
pub struct BrowserSnapshot {
    pub sheets: Vec<crate::SheetEntry>,
    pub root_file: String,
    pub revision: u64,
    pub rendering: bool,
    pub errors: Vec<String>,
    pub closed: bool,
}
struct Cache {
    snapshot: BrowserSnapshot,
    svg: HashMap<String, String>,
}
struct Shared {
    cache: RwLock<Cache>,
    events: watch::Sender<BrowserSnapshot>,
    commands: mpsc::Sender<Refresh>,
    token: String,
    port: u16,
}
struct Change {
    paths: Vec<PathBuf>,
    error: Option<String>,
}
struct Refresh {
    file: String,
    result: oneshot::Sender<Result<String, String>>,
}
/// A browser viewer owned by its caller. No browser is launched by this API.
/// Call `shutdown` to await teardown; dropping signals stop and terminates streams.
pub struct BrowserViewer {
    pub url: String,
    stop: watch::Sender<bool>,
    shared: Arc<Shared>,
    watcher: Arc<std::sync::Mutex<Option<notify::RecommendedWatcher>>>,
    server: Option<JoinHandle<std::io::Result<()>>>,
    worker: Option<JoinHandle<()>>,
}
impl BrowserViewer {
    /// Start a root-bound viewer on an ephemeral IPv4 loopback port. The CLI is
    /// supplied by the caller, not accepted from HTTP. Each render is bounded.
    pub async fn start(root: &Path, cli: &str, render_timeout: Duration) -> Result<Self, String> {
        if render_timeout.is_zero() {
            return Err("render timeout must be positive".into());
        }
        let root = root
            .canonicalize()
            .map_err(|e| format!("resolve schematic: {e}"))?;
        let mut revisions = HashMap::new();
        let sheets = checked_sheets_observed(&root, &mut |path| {
            revisions
                .entry(path.to_path_buf())
                .or_insert_with(|| source_revision(path));
        })?;
        let project = root.with_extension("kicad_pro");
        revisions.insert(project.clone(), source_revision(&project));
        let (stop, stop_rx) = watch::channel(false);
        let (svg, errors) = render(&root, &sheets, cli, render_timeout, stop_rx.clone()).await;
        let root_file = root.to_string_lossy().into_owned();
        if !svg.contains_key(&root_file) {
            return Err(format!("root render failed: {}", errors.join("; ")));
        }
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|e| format!("bind viewer: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let snapshot = BrowserSnapshot {
            sheets,
            root_file,
            revision: 1,
            rendering: false,
            errors,
            closed: false,
        };
        let (events, _) = watch::channel(snapshot.clone());
        let (commands, command_rx) = mpsc::channel(8);
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let shared = Arc::new(Shared {
            cache: RwLock::new(Cache { snapshot, svg }),
            events,
            commands,
            token: token.clone(),
            port,
        });
        let (changes, change_rx) = mpsc::unbounded_channel();
        let notifications = changes.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: Result<notify::Event, notify::Error>| {
                let relevant = match &event {
                    Ok(event) => {
                        matches!(
                            event.kind,
                            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                        ) && event.paths.iter().any(|p| crate::is_relevant_sch_change(p))
                    }
                    Err(_) => true,
                };
                if relevant {
                    // The worker coalesces the burst, retaining watcher errors and
                    // invalidating the complete tree rather than losing changed paths.
                    let paths = event
                        .as_ref()
                        .map(|event| event.paths.clone())
                        .unwrap_or_default();
                    let error = event.err().map(|e| format!("watcher: {e}"));
                    let _ = notifications.send(Change { paths, error });
                }
            })
            .map_err(|e| format!("create watcher: {e}"))?;
        let watched = crate::compute_watch_dirs(&shared.cache.read().await.snapshot.sheets);
        for dir in &watched {
            watcher
                .watch(dir, RecursiveMode::NonRecursive)
                .map_err(|e| format!("watch {}: {e}", dir.display()))?;
        }
        // Watches now cover edits after this check. Catch only sources that
        // changed since discovery/snapshotting, rather than rendering all twice.
        let changed = startup_changes(
            &revisions,
            &project,
            &shared.cache.read().await.snapshot.sheets,
        );
        if !changed.is_empty() {
            let _ = changes.send(Change {
                paths: changed,
                error: None,
            });
        }
        let watcher = Arc::new(std::sync::Mutex::new(Some(watcher)));
        let worker = tokio::spawn(worker(
            shared.clone(),
            Worker {
                root,
                cli: cli.to_owned(),
                timeout: render_timeout,
                stop: stop_rx.clone(),
                commands: command_rx,
                changes: change_rx,
                watcher: watcher.clone(),
                watched,
            },
        ));
        let app = router(shared.clone());
        let server = tokio::spawn(serve_owned(listener, app, stop_rx));
        Ok(Self {
            url: format!("http://127.0.0.1:{port}/#token={token}"),
            stop,
            shared,
            watcher,
            server: Some(server),
            worker: Some(worker),
        })
    }
    /// Whether both owned tasks are still running and shutdown is not requested.
    pub fn is_running(&self) -> bool {
        self.server.as_ref().is_some_and(|task| !task.is_finished())
            && self.worker.as_ref().is_some_and(|task| !task.is_finished())
            && !*self.stop.borrow()
    }
    /// Signal shutdown without consuming the handle. Health becomes false
    /// immediately; use `shutdown().await` to wait for owned tasks to finish.
    pub fn request_shutdown(&self) {
        self.watcher.lock().unwrap().take();
        self.stop.send_replace(true);
        let mut last = self.shared.events.borrow().clone();
        last.closed = true;
        self.shared.events.send_replace(last);
    }
    /// Wait for the render worker (including a cancelled CLI) and HTTP server.
    pub async fn shutdown(mut self) {
        self.request_shutdown();
        if let Some(mut worker) = self.worker.take() {
            if tokio::time::timeout(Duration::from_secs(3), &mut worker)
                .await
                .is_err()
            {
                worker.abort();
                let _ = worker.await;
            }
        }
        if let Some(mut server) = self.server.take() {
            if tokio::time::timeout(Duration::from_secs(3), &mut server)
                .await
                .is_err()
            {
                server.abort();
                let _ = server.await;
            }
        }
    }
}
impl Drop for BrowserViewer {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}
// Own accepted connections as well as the listener. Aborting an axum::serve
// future alone does not bound a slow response's lifetime after shutdown.
async fn serve_owned(
    listener: tokio::net::TcpListener,
    app: Router,
    stop: watch::Receiver<bool>,
) -> std::io::Result<()> {
    serve_accepting(|| listener.accept(), app, stop).await
}

// The accept seam permits deterministic transient-error tests without changing
// the process-wide file-descriptor limit or depending on OS socket races.
async fn serve_accepting<F, Fut>(
    mut accept: F,
    app: Router,
    mut stop: watch::Receiver<bool>,
) -> std::io::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<
        Output = std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)>,
    >,
{
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            accepted = accept() => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        if !matches!(error.kind(), std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::ConnectionReset)
                        {
                            tracing::error!(%error, "viewer accept failed; retrying in one second");
                            tokio::select! {
                                _ = stop.changed() => break,
                                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                            }
                        }
                        continue;
                    }
                };
                let service = hyper_util::service::TowerToHyperService::new(app.clone());
                let mut stopping = stop.clone();
                connections.spawn(async move {
                    if *stopping.borrow() { return; }
                    let connection = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service);
                    tokio::pin!(connection);
                    tokio::select! {
                        _ = stopping.changed() => {
                            connection.as_mut().graceful_shutdown();
                            let _ = tokio::time::timeout(Duration::from_secs(1), &mut connection).await;
                        }
                        _ = &mut connection => {}
                    }
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    while connections.join_next().await.is_some() {}
    Ok(())
}

#[derive(PartialEq, Eq)]
enum SourceRevision {
    Missing,
    Known(SystemTime, u64),
    Unavailable,
}
fn source_revision(path: &Path) -> SourceRevision {
    match std::fs::metadata(path) {
        Ok(metadata) => match metadata.modified() {
            Ok(modified) => SourceRevision::Known(modified, metadata.len()),
            Err(_) => SourceRevision::Unavailable,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => SourceRevision::Missing,
        Err(_) => SourceRevision::Unavailable,
    }
}
fn startup_changes(
    before: &HashMap<PathBuf, SourceRevision>,
    project: &Path,
    sheets: &[crate::SheetEntry],
) -> Vec<PathBuf> {
    let changed: Vec<_> = before
        .iter()
        .filter(|(path, revision)| {
            **revision == SourceRevision::Unavailable || **revision != source_revision(path)
        })
        .map(|(path, _)| path.clone())
        .collect();
    // Project settings affect every sheet, including ones whose source is stable.
    if changed.iter().any(|path| path == project) {
        sheets
            .iter()
            .map(|sheet| PathBuf::from(&sheet.file))
            .collect()
    } else {
        changed
    }
}

fn checked_sheets(root: &Path) -> Result<Vec<crate::SheetEntry>, String> {
    checked_sheets_observed(root, &mut |_| {})
}
fn checked_sheets_observed(
    root: &Path,
    before_read: &mut impl FnMut(&Path),
) -> Result<Vec<crate::SheetEntry>, String> {
    if root.extension().and_then(|s| s.to_str()) != Some("kicad_sch") || !root.is_file() {
        return Err("viewer root must be a schematic file".into());
    }
    let base = root.parent().ok_or("schematic has no parent")?;
    let sheets = crate::walk_sheet_tree_observed(root, before_read);
    let known: HashSet<_> = sheets
        .iter()
        .map(|entry| PathBuf::from(&entry.file).canonicalize())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("resolve hierarchy: {e}"))?;
    let mut ancestors = Vec::new();
    for entry in &sheets {
        let path = Path::new(&entry.file);
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("resolve sheet: {e}"))?;
        if !canonical.starts_with(base) {
            return Err("browser viewer refuses sheets outside the root directory".into());
        }
        ancestors.truncate(entry.depth);
        ancestors.push(canonical);
        let sch = copperpour_schematic_editor::Schematic::load(path)
            .map_err(|e| format!("read hierarchy: {e}"))?;
        for child in sch.sheets.iter() {
            let file = Path::new(child.file());
            if file.is_absolute()
                || file.components().any(|c| {
                    matches!(
                        c,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
            {
                return Err(
                    "browser viewer requires relative child paths without parent traversal".into(),
                );
            }
            let child = path
                .parent()
                .unwrap()
                .join(file)
                .canonicalize()
                .map_err(|e| format!("missing child sheet {}: {e}", file.display()))?;
            if ancestors.contains(&child) {
                return Err("browser viewer refuses a cyclic hierarchy".into());
            }
            if !known.contains(&child) {
                return Err("browser hierarchy exceeds supported discovery depth".into());
            }
        }
    }
    Ok(sheets)
}
async fn render(
    root: &Path,
    entries: &[crate::SheetEntry],
    cli: &str,
    timeout: Duration,
    stop: watch::Receiver<bool>,
) -> (HashMap<String, String>, Vec<String>) {
    render_selected(root, entries, cli, timeout, stop, None).await
}
async fn render_selected(
    root: &Path,
    entries: &[crate::SheetEntry],
    cli: &str,
    timeout: Duration,
    mut stop: watch::Receiver<bool>,
    subset: Option<&HashSet<String>>,
) -> (HashMap<String, String>, Vec<String>) {
    let mut cache = HashMap::new();
    let mut errors = Vec::new();
    let batch = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(e) => return (cache, vec![e.to_string()]),
    };
    let snapshot = crate::snapshot_tree(root, entries, &batch.path().join("src"));
    // A failed copy must never turn browser rendering into a live-file read.
    if snapshot.values().any(|p| !p.starts_with(batch.path())) {
        return (
            cache,
            vec!["snapshot copy failed; browser render refused".into()],
        );
    }
    let project = root.with_extension("kicad_pro");
    if project.exists()
        && !batch
            .path()
            .join("src")
            .join(project.file_name().unwrap())
            .is_file()
    {
        return (
            cache,
            vec!["project snapshot copy failed; browser render refused".into()],
        );
    }
    let mut seen = HashSet::new();
    for (index, entry) in entries.iter().enumerate() {
        if !seen.insert(&entry.file) || subset.is_some_and(|needed| !needed.contains(&entry.file)) {
            continue;
        }
        if *stop.borrow() {
            break;
        }
        let out = batch.path().join(index.to_string());
        if let Err(e) = std::fs::create_dir_all(&out) {
            errors.push(e.to_string());
            continue;
        }
        let mut command = tokio::process::Command::new(cli);
        command
            .args(["sch", "export", "svg", "--output"])
            .arg(&out)
            .arg(&snapshot[&entry.file]);
        let mut diagnostics = match tempfile::tempfile() {
            Ok(file) => file,
            Err(error) => {
                errors.push(format!(
                    "{}: create diagnostic capture: {error}",
                    entry.name
                ));
                continue;
            }
        };
        let output = match diagnostics
            .try_clone()
            .and_then(|stdout| diagnostics.try_clone().map(|stderr| (stdout, stderr)))
        {
            Ok(output) => output,
            Err(error) => {
                errors.push(format!("{}: capture renderer output: {error}", entry.name));
                continue;
            }
        };
        command
            .kill_on_drop(true)
            .stdout(Stdio::from(output.0))
            .stderr(Stdio::from(output.1));
        #[cfg(target_os = "windows")]
        {
            command.creation_flags(0x08000000);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                errors.push(format!("{}: launch kicad-cli: {e}", entry.name));
                continue;
            }
        };
        let status = tokio::select! {
            _ = stop.changed() => { let _ = child.kill().await; let _ = child.wait().await; break; }
            status = tokio::time::timeout(timeout, child.wait()) => status,
        };
        let successful = match status {
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                errors.push(format!("{}: render timed out", entry.name));
                continue;
            }
            Ok(Err(e)) => {
                errors.push(format!("{}: wait for renderer: {e}", entry.name));
                continue;
            }
            Ok(Ok(status)) => status.success(),
        };
        use std::io::{Read, Seek};
        let mut output = Vec::new();
        let diagnostic = match diagnostics
            .rewind()
            .and_then(|_| diagnostics.take(64 * 1024).read_to_end(&mut output))
        {
            Ok(_) => String::from_utf8_lossy(&output).trim().to_string(),
            Err(error) => format!("renderer diagnostic unavailable: {error}"),
        };
        let svg_path = out.join(format!(
            "{}.svg",
            Path::new(&entry.file)
                .file_stem()
                .unwrap()
                .to_string_lossy()
        ));
        match std::fs::read_to_string(svg_path) {
            Ok(svg) if valid_svg(&svg) => {
                cache.insert(entry.file.clone(), svg);
                if !successful {
                    errors.push(format!(
                        "{}: renderer exited unsuccessfully; valid SVG retained: {diagnostic}",
                        entry.name
                    ));
                }
            }
            _ => errors.push(format!(
                "{}: renderer produced no valid nonempty SVG: {diagnostic}",
                entry.name
            )),
        }
    }
    (cache, errors)
}
fn valid_svg(svg: &str) -> bool {
    // KiCad emits the SVG 1.1 external DOCTYPE. No resolver is supplied:
    // parsing this declaration must never fetch a DTD or a local entity.
    roxmltree::Document::parse_with_options(
        svg,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            entity_resolver: None,
            ..Default::default()
        },
    )
    .is_ok_and(|document| {
        let root = document.root_element();
        root.tag_name().name() == "svg"
            && root.tag_name().namespace() == Some("http://www.w3.org/2000/svg")
    })
}
struct Worker {
    root: PathBuf,
    cli: String,
    timeout: Duration,
    stop: watch::Receiver<bool>,
    commands: mpsc::Receiver<Refresh>,
    changes: mpsc::UnboundedReceiver<Change>,
    watcher: Arc<std::sync::Mutex<Option<notify::RecommendedWatcher>>>,
    watched: HashSet<PathBuf>,
}
async fn worker(shared: Arc<Shared>, worker: Worker) {
    let Worker {
        root,
        cli,
        timeout,
        mut stop,
        mut commands,
        mut changes,
        watcher,
        mut watched,
    } = worker;
    loop {
        if *stop.borrow() {
            break;
        }
        let mut completion = None;
        let mut watcher_errors = Vec::new();
        let mut changed = HashSet::new();
        let mut full_refresh = false;
        tokio::select! {
            _ = stop.changed() => break,
            command = commands.recv() => {
                let Some(command) = command else { break; };
                completion = Some(command); full_refresh = true;
            }
            event = changes.recv() => {
                let Some(event) = event else { break; };
                if event.paths.is_empty() { full_refresh = true; }
                crate::note_changed(&mut changed, event.paths);
                if let Some(error) = event.error { watcher_errors.push(error); }
                let start = tokio::time::Instant::now();
                loop {
                    tokio::select! {
                        _ = stop.changed() => return,
                        _ = tokio::time::sleep(crate::QUIET_WINDOW) => break,
                        next = changes.recv() => { match next { None => return, Some(event) => {
                            if event.paths.is_empty() { full_refresh = true; }
                            crate::note_changed(&mut changed, event.paths);
                            if let Some(error) = event.error { watcher_errors.push(error); }
                        } } }
                    }
                    if start.elapsed() >= crate::MAX_BURST_WAIT { break; }
                }
            }
        }
        if *stop.borrow() {
            break;
        }
        let (mut latest, old_cache) = {
            let cache = shared.cache.read().await;
            (cache.snapshot.clone(), cache.svg.clone())
        };
        latest.rendering = true;
        shared.cache.write().await.snapshot = latest.clone();
        shared.events.send_if_modified(|old| {
            if old.closed {
                false
            } else {
                *old = latest.clone();
                true
            }
        });
        let result = checked_sheets(&root);
        let (sheets, svg, mut errors) = match result {
            Ok(sheets) => {
                let desired = crate::compute_watch_dirs(&sheets);
                let mut watch_errors = Vec::new();
                {
                    let mut guard = watcher.lock().unwrap();
                    if let Some(watcher) = guard.as_mut() {
                        for dir in desired.difference(&watched).cloned().collect::<Vec<_>>() {
                            match watcher.watch(&dir, RecursiveMode::NonRecursive) {
                                Ok(()) => {
                                    watched.insert(dir);
                                }
                                Err(e) => {
                                    watch_errors.push(format!("watch {}: {e}", dir.display()))
                                }
                            }
                        }
                        for dir in watched.difference(&desired).cloned().collect::<Vec<_>>() {
                            match watcher.unwatch(&dir) {
                                Ok(()) => {
                                    watched.remove(&dir);
                                }
                                Err(e) => {
                                    watch_errors.push(format!("unwatch {}: {e}", dir.display()))
                                }
                            }
                        }
                    }
                }
                let needed = if full_refresh {
                    sheets.iter().map(|sheet| sheet.file.clone()).collect()
                } else {
                    crate::files_needing_render(&sheets, &changed, &old_cache)
                };
                let (fresh, mut errors) =
                    render_selected(&root, &sheets, &cli, timeout, stop.clone(), Some(&needed))
                        .await;
                let mut svg = HashMap::new();
                for sheet in &sheets {
                    if let Some(value) = fresh.get(&sheet.file).or_else(|| {
                        if needed.contains(&sheet.file) {
                            None
                        } else {
                            old_cache.get(&sheet.file)
                        }
                    }) {
                        svg.insert(sheet.file.clone(), value.clone());
                    }
                }
                errors.extend(watch_errors);
                (sheets, svg, errors)
            }
            Err(error) => (latest.sheets.clone(), HashMap::new(), vec![error]),
        };
        if *stop.borrow() {
            break;
        }
        errors.extend(watcher_errors);
        latest.sheets = sheets;
        latest.revision += 1;
        latest.rendering = false;
        latest.errors = errors;
        let completion_result = completion.as_ref().map(|command| {
            svg.get(&command.file).cloned().ok_or_else(|| {
                if latest.errors.is_empty() {
                    "requested sheet is no longer available in the current hierarchy".into()
                } else {
                    latest.errors.join("; ")
                }
            })
        });
        *shared.cache.write().await = Cache {
            snapshot: latest.clone(),
            svg,
        };
        shared.events.send_if_modified(|old| {
            if old.closed {
                false
            } else {
                *old = latest;
                true
            }
        });
        if let (Some(command), Some(result)) = (completion, completion_result) {
            let _ = command.result.send(result);
        }
    }
}
#[derive(Default, Deserialize)]
struct Parameters {
    token: Option<String>,
    file: Option<String>,
}
fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/", get(|| async { Html(PAGE) }))
        .route(
            "/viewer.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    SCRIPT,
                )
            }),
        )
        .route("/state", get(state))
        .route("/sheet", get(sheet))
        .route("/events", get(events))
        .route("/refresh", post(refresh))
        .route("/open-in-kicad", post(open_in_kicad))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(shared.clone(), boundary))
        .with_state(shared)
}
fn authorized(token: &str, expected: &str) -> bool {
    token.len() == expected.len()
        && token
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}
async fn boundary(
    State(shared): State<Arc<Shared>>,
    Query(query): Query<Parameters>,
    request: Request,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let allowed = host == format!("127.0.0.1:{}", shared.port)
        || host == format!("localhost:{}", shared.port);
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|h| h.to_str().ok());
    if !allowed
        || origin.is_some_and(|o| o != format!("http://{host}"))
        || (request.method() == axum::http::Method::POST && origin.is_none())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if shared.events.borrow().closed {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let public = matches!(request.uri().path(), "/" | "/viewer.js");
    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let token = bearer.or_else(|| {
        (request.uri().path() == "/events")
            .then_some(query.token.as_deref())
            .flatten()
    });
    if !public && !token.is_some_and(|token| authorized(token, &shared.token)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let svg = request.uri().path() == "/sheet";
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(if svg { SVG_CSP } else { PAGE_CSP }),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}
async fn state(State(shared): State<Arc<Shared>>) -> Json<BrowserSnapshot> {
    Json(shared.cache.read().await.snapshot.clone())
}
async fn sheet(State(shared): State<Arc<Shared>>, Query(query): Query<Parameters>) -> Response {
    let cache = shared.cache.read().await;
    let Some(file) = query.file else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if !cache.snapshot.sheets.iter().any(|entry| entry.file == file) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match cache.svg.get(&file) {
        Some(svg) => (
            [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
            svg.clone(),
        )
            .into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, "sheet render unavailable").into_response(),
    }
}
async fn events(State(shared): State<Arc<Shared>>) -> impl IntoResponse {
    let stream = WatchStream::new(shared.events.subscribe())
        .take_while(|snapshot| !snapshot.closed)
        .map(|snapshot| {
            Ok::<_, Infallible>(
                Event::default()
                    .event("state")
                    .id(snapshot.revision.to_string())
                    .json_data(snapshot)
                    .unwrap(),
            )
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRequest {
    file: String,
}
async fn refresh(State(shared): State<Arc<Shared>>, Json(request): Json<FileRequest>) -> Response {
    if !shared
        .cache
        .read()
        .await
        .snapshot
        .sheets
        .iter()
        .any(|entry| entry.file == request.file)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (result, receive) = oneshot::channel();
    match shared.commands.try_send(Refresh {
        file: request.file,
        result,
    }) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            return StatusCode::TOO_MANY_REQUESTS.into_response()
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
    match receive.await {
        Ok(Ok(svg)) => Json(svg).into_response(),
        Ok(Err(error)) => (StatusCode::SERVICE_UNAVAILABLE, error).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
async fn open_in_kicad(
    State(shared): State<Arc<Shared>>,
    Json(request): Json<FileRequest>,
) -> Response {
    if !shared
        .cache
        .read()
        .await
        .snapshot
        .sheets
        .iter()
        .any(|entry| entry.file == request.file)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let root = PathBuf::from(&shared.cache.read().await.snapshot.root_file);
    let project = root.with_extension("kicad_pro");
    let target = if project.is_file() {
        project
    } else {
        PathBuf::from(request.file)
    };
    if crate::kicad_lock_path(&target).exists() {
        return (StatusCode::CONFLICT, "project is already open in KiCad").into_response();
    }
    match tokio::process::Command::new(crate::resolve_kicad_binary())
        .arg(target)
        .spawn()
    {
        Ok(mut child) => {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            Json(serde_json::json!({"spawned":true})).into_response()
        }
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("launch KiCad: {error}"),
        )
            .into_response(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;
    struct Fixture {
        _dir: TempDir,
        root: PathBuf,
        cli: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("root.revision.kicad_sch");
            std::fs::write(
                &root,
                include_str!("../../copperpour-core/tests/fixtures/derived_lib_name.kicad_sch"),
            )
            .unwrap();
            let cli = dir.path().join("cli");
            std::fs::write(&cli, r##"#!/bin/sh
mkdir -p "$5"
printf '%s\n' "$(basename "$6")" >> "$0.calls"
printf '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 80"><text>%d</text></svg>' "$(wc -c < "$6")" > "$5/$(basename "$6" .kicad_sch).svg"
"##).unwrap();
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                _dir: dir,
                root,
                cli,
            }
        }
        async fn start(&self) -> BrowserViewer {
            BrowserViewer::start(
                &self.root,
                self.cli.to_str().unwrap(),
                Duration::from_secs(2),
            )
            .await
            .unwrap()
        }
    }
    #[tokio::test]
    async fn service_health_requires_both_owned_tasks() {
        let fixture = Fixture::new();
        let viewer = fixture.start().await;
        assert!(viewer.is_running());
        viewer.server.as_ref().unwrap().abort();
        tokio::task::yield_now().await;
        assert!(!viewer.is_running());
        viewer.shutdown().await;
        let viewer = fixture.start().await;
        assert!(viewer.is_running());
        viewer.worker.as_ref().unwrap().abort();
        tokio::task::yield_now().await;
        assert!(!viewer.is_running());
        viewer.shutdown().await;
    }
    struct Client {
        http: reqwest::Client,
        base: String,
        token: String,
    }
    impl Client {
        fn new(viewer: &BrowserViewer) -> Self {
            let (base, token) = viewer.url.split_once("/#token=").unwrap();
            Self {
                http: reqwest::Client::new(),
                base: base.to_string(),
                token: token.to_string(),
            }
        }
        fn get(&self, route: &str) -> reqwest::RequestBuilder {
            self.http
                .get(format!("{}{route}", self.base))
                .bearer_auth(&self.token)
        }
        fn post(&self, route: &str, file: &Path) -> reqwest::RequestBuilder {
            self.http
                .post(format!("{}{route}", self.base))
                .header("Origin", &self.base)
                .bearer_auth(&self.token)
                .json(&serde_json::json!({"file":file}))
        }
        async fn state(&self) -> serde_json::Value {
            self.get("/state")
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap()
        }
        async fn quiet(&self) -> serde_json::Value {
            tokio::time::timeout(Duration::from_secs(6), async {
                loop {
                    let state = self.state().await;
                    if state["rendering"] == false {
                        return state;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap()
        }
        fn sheet_url(&self, file: &Path) -> String {
            let mut url = reqwest::Url::parse(&format!("{}/sheet", self.base)).unwrap();
            url.query_pairs_mut()
                .append_pair("file", &file.to_string_lossy());
            url.to_string()
        }
    }
    async fn assert_accept_recovery(errors: Vec<std::io::ErrorKind>, minimum: Duration) {
        let listener = Arc::new(
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap(),
        );
        let address = listener.local_addr().unwrap();
        let mut errors = std::collections::VecDeque::from(errors);
        let (stop, rx) = watch::channel(false);
        let server = tokio::spawn(async move {
            serve_accepting(
                move || {
                    let error = errors.pop_front();
                    let listener = listener.clone();
                    async move {
                        if let Some(error) = error {
                            Err(std::io::Error::from(error))
                        } else {
                            listener.accept().await
                        }
                    }
                },
                Router::new().route("/", get(|| async { "recovered" })),
                rx,
            )
            .await
        });
        let start = tokio::time::Instant::now();
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            reqwest::get(format!("http://{address}/")),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "recovered");
        assert!(
            start.elapsed() >= minimum,
            "resource-pressure errors must back off"
        );
        stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    async fn browser_accept_retries_connection_errors_without_backoff() {
        tokio::time::timeout(
            Duration::from_secs(2),
            assert_accept_recovery(
                vec![
                    std::io::ErrorKind::ConnectionAborted,
                    std::io::ErrorKind::ConnectionReset,
                    std::io::ErrorKind::ConnectionRefused,
                ],
                Duration::ZERO,
            ),
        )
        .await
        .expect("connection errors must not back off on each retry");
    }
    #[tokio::test]
    async fn browser_accept_recovers_after_resource_pressure_with_backoff() {
        assert_accept_recovery(vec![std::io::ErrorKind::Other], Duration::from_secs(1)).await;
    }
    #[tokio::test]
    async fn browser_shutdown_interrupts_accept_error_backoff() {
        let started = Arc::new(tokio::sync::Notify::new());
        let notify = started.clone();
        let (stop, rx) = watch::channel(false);
        let server = tokio::spawn(serve_accepting(
            move || {
                notify.notify_one();
                std::future::ready(Err(std::io::Error::from(std::io::ErrorKind::Other)))
            },
            Router::new(),
            rx,
        ));
        started.notified().await;
        stop.send_replace(true);
        tokio::time::timeout(Duration::from_millis(500), server)
            .await
            .expect("shutdown must interrupt backoff")
            .unwrap()
            .unwrap();
    }
    fn add_child(root: &Path, name: &str, file: &str) {
        let mut sch = copperpour_schematic_editor::Schematic::load(root).unwrap();
        sch.add_sheet(copperpour_schematic_editor::Sheet::new(
            name, file, 50.0, 50.0, 80.0, 50.0,
        ));
        sch.overwrite().unwrap();
    }
    #[tokio::test]
    async fn browser_startup_renders_each_unchanged_sheet_only_once() {
        let fixture = Fixture::new();
        let child = fixture.root.parent().unwrap().join("child.kicad_sch");
        std::fs::copy(&fixture.root, &child).unwrap();
        add_child(&fixture.root, "child", "child.kicad_sch");
        let viewer = fixture.start().await;
        let client = Client::new(&viewer);
        tokio::time::sleep(crate::QUIET_WINDOW * 2).await;
        let state = client.state().await;
        assert_eq!(state["revision"], 1);
        assert_eq!(state["rendering"], false);
        let calls = std::fs::read_to_string(fixture.cli.with_extension("calls")).unwrap();
        assert_eq!(calls.lines().count(), 2, "{calls}");
        assert_eq!(
            calls
                .lines()
                .filter(|name| *name == "child.kicad_sch")
                .count(),
            1
        );
        viewer.shutdown().await;
    }
    #[tokio::test]
    async fn browser_startup_catches_edits_and_new_children_without_rerendering_stable_sheets() {
        let fixture = Fixture::new();
        let child = fixture.root.parent().unwrap().join("child.kicad_sch");
        let added = fixture.root.parent().unwrap().join("added.kicad_sch");
        std::fs::copy(&fixture.root, &child).unwrap();
        std::fs::copy(&fixture.root, &added).unwrap();
        add_child(&fixture.root, "child", "child.kicad_sch");
        // Initial snapshots already exist when the first CLI call pauses.
        let script = std::fs::read_to_string(&fixture.cli).unwrap().replace(
            "mkdir -p",
            "touch \"$0.started\"\nwhile [ ! -f \"$0.continue\" ]; do sleep 0.02; done\nmkdir -p",
        );
        std::fs::write(&fixture.cli, script).unwrap();
        let root = fixture.root.clone();
        let cli = fixture.cli.to_string_lossy().into_owned();
        let starting = tokio::spawn(async move {
            BrowserViewer::start(&root, &cli, Duration::from_secs(5))
                .await
                .unwrap()
        });
        tokio::time::timeout(Duration::from_secs(4), async {
            while !fixture.cli.with_extension("started").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        add_child(&child, "added", "added.kicad_sch");
        let expected_size = std::fs::metadata(&child).unwrap().len();
        std::fs::write(fixture.cli.with_extension("continue"), "").unwrap();
        let viewer = starting.await.unwrap();
        let client = Client::new(&viewer);
        let state = wait_revision(&client, 1).await;
        assert_eq!(state["sheets"].as_array().unwrap().len(), 3);
        let calls = std::fs::read_to_string(fixture.cli.with_extension("calls")).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|name| *name == "root.revision.kicad_sch")
                .count(),
            1,
            "stable root rendered again: {calls}"
        );
        assert_eq!(
            calls
                .lines()
                .filter(|name| *name == "child.kicad_sch")
                .count(),
            2,
            "changed child was not refreshed: {calls}"
        );
        assert_eq!(
            calls
                .lines()
                .filter(|name| *name == "added.kicad_sch")
                .count(),
            1,
            "new child missing: {calls}"
        );
        let svg = client
            .http
            .get(client.sheet_url(&child))
            .bearer_auth(&client.token)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(
            svg.contains(&format!(">{expected_size}</text>")),
            "stale startup SVG: {svg}"
        );
        viewer.shutdown().await;
    }
    #[test]
    fn browser_startup_records_sources_before_discovery_reads() {
        let fixture = Fixture::new();
        let child = fixture.root.parent().unwrap().join("child.kicad_sch");
        std::fs::copy(&fixture.root, &child).unwrap();
        let mut revisions = HashMap::new();
        let mut edited = false;
        let sheets = checked_sheets_observed(&fixture.root, &mut |path| {
            revisions
                .entry(path.to_path_buf())
                .or_insert_with(|| source_revision(path));
            if path == fixture.root && !edited {
                edited = true;
                let modified = std::fs::metadata(&fixture.root)
                    .unwrap()
                    .modified()
                    .unwrap();
                add_child(&fixture.root, "child", "child.kicad_sch");
                // Keep mtime stable to pin the length fallback independently.
                std::fs::File::open(&fixture.root)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(modified))
                    .unwrap();
            }
        })
        .unwrap();
        assert_eq!(
            sheets.len(),
            2,
            "discovery must read after revision observation"
        );
        assert_eq!(
            startup_changes(
                &revisions,
                &fixture.root.with_extension("kicad_pro"),
                &sheets
            ),
            vec![fixture.root]
        );
    }
    #[test]
    fn browser_startup_project_changes_invalidate_every_sheet_and_unknown_revisions_retry() {
        let fixture = Fixture::new();
        let child = fixture.root.parent().unwrap().join("child.kicad_sch");
        std::fs::copy(&fixture.root, &child).unwrap();
        add_child(&fixture.root, "child", "child.kicad_sch");
        let sheets = checked_sheets(&fixture.root).unwrap();
        let project = fixture.root.with_extension("kicad_pro");
        let mut revisions: HashMap<_, _> = sheets
            .iter()
            .map(|sheet| {
                (
                    PathBuf::from(&sheet.file),
                    source_revision(Path::new(&sheet.file)),
                )
            })
            .collect();
        revisions.insert(project.clone(), source_revision(&project));
        assert!(
            startup_changes(&revisions, &project, &sheets).is_empty(),
            "absent project is stable"
        );
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../copperpour-core/tests/fixtures/specctra_rounded_outline.kicad_pro"),
            &project,
        )
        .unwrap();
        let changed: HashSet<_> = startup_changes(&revisions, &project, &sheets)
            .into_iter()
            .collect();
        assert_eq!(
            changed,
            sheets
                .iter()
                .map(|sheet| PathBuf::from(&sheet.file))
                .collect()
        );
        revisions.insert(project.clone(), source_revision(&project));
        let modified = std::fs::metadata(&project).unwrap().modified().unwrap();
        std::fs::File::open(&project)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified + Duration::from_secs(2)))
            .unwrap();
        let changed: HashSet<_> = startup_changes(&revisions, &project, &sheets)
            .into_iter()
            .collect();
        assert_eq!(
            changed,
            sheets
                .iter()
                .map(|sheet| PathBuf::from(&sheet.file))
                .collect(),
            "mtime-only project change must refresh all sheets"
        );
        revisions.insert(project.clone(), source_revision(&project));
        revisions.insert(child.clone(), SourceRevision::Unavailable);
        assert_eq!(startup_changes(&revisions, &project, &sheets), vec![child]);
        // Both observations can fail, so equality of unavailable metadata must
        // never establish that a source stayed unchanged.
        let unavailable = fixture.root.join("not-a-directory.kicad_sch");
        let unknown = HashMap::from([(unavailable.clone(), source_revision(&unavailable))]);
        assert!(matches!(unknown[&unavailable], SourceRevision::Unavailable));
        assert_eq!(
            startup_changes(&unknown, &project, &sheets),
            vec![unavailable]
        );
    }
    #[test]
    fn browser_parent_traversal_inside_root_gets_the_lexical_refusal() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.root.parent().unwrap().join("sub")).unwrap();
        std::fs::copy(
            &fixture.root,
            fixture.root.parent().unwrap().join("inside.kicad_sch"),
        )
        .unwrap();
        add_child(&fixture.root, "inside", "sub/../inside.kicad_sch");
        let error = checked_sheets(&fixture.root).unwrap_err();
        assert!(
            error.contains("relative child paths without parent traversal"),
            "{error}"
        );
    }
    #[test]
    fn browser_parent_traversal_outside_root_gets_the_lexical_refusal_first() {
        let fixture = Fixture::new();
        let folder = fixture.root.parent().unwrap().join("project");
        std::fs::create_dir(&folder).unwrap();
        let root = folder.join("root.kicad_sch");
        std::fs::copy(&fixture.root, &root).unwrap();
        std::fs::copy(
            &fixture.root,
            fixture.root.parent().unwrap().join("outside.kicad_sch"),
        )
        .unwrap();
        add_child(&root, "outside", "../outside.kicad_sch");
        let error = checked_sheets(&root).unwrap_err();
        assert!(
            error.contains("relative child paths without parent traversal"),
            "{error}"
        );
    }
    #[test]
    fn svg_validation_accepts_kicad_doctype_but_never_resolves_entities() {
        assert!(valid_svg(
            r#"<?xml version="1.0"?><!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd"><svg xmlns="http://www.w3.org/2000/svg"/>"#
        ));
        assert!(!valid_svg("<html/>"));
        assert!(!valid_svg("<html xmlns='http://www.w3.org/2000/svg'/>"));
        assert!(!valid_svg("<svg xmlns='http://www.w3.org/2000/svg'>"));
        assert!(!valid_svg(
            r#"<!DOCTYPE svg [<!ENTITY secret SYSTEM "file:///etc/passwd">]><svg xmlns="http://www.w3.org/2000/svg">&secret;</svg>"#
        ));
    }

    #[tokio::test]
    async fn browser_refuses_unauthorized_origins_hosts_and_posts() {
        let fixture = Fixture::new();
        let viewer = fixture.start().await;
        let client = Client::new(&viewer);
        assert_eq!(
            client
                .http
                .get(format!("{}/state", client.base))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            client
                .http
                .get(format!("{}/state", client.base))
                .bearer_auth("wrong")
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            client
                .get("/state")
                .header("Origin", "https://attacker.example")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .get("/state")
                .header("Host", "attacker.example")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .http
                .post(format!("{}/refresh", client.base))
                .bearer_auth(&client.token)
                .json(&serde_json::json!({"file":fixture.root}))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        let state = client.state().await;
        assert_eq!(state["root_file"], fixture.root.to_string_lossy().as_ref());
        assert!(!state.to_string().contains(&client.token));
        viewer.shutdown().await;
    }
    #[tokio::test]
    async fn browser_rejects_malformed_refresh_records_without_work() {
        let fixture = Fixture::new();
        let viewer = fixture.start().await;
        let client = Client::new(&viewer);
        let before = client.quiet().await;
        for body in [
            serde_json::json!({}),
            serde_json::json!({"file":42}),
            serde_json::json!({"file":fixture.root,"force":true}),
        ] {
            assert_eq!(
                client
                    .http
                    .post(format!("{}/refresh", client.base))
                    .header("Origin", &client.base)
                    .bearer_auth(&client.token)
                    .json(&body)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                422
            );
        }
        assert_eq!(client.state().await["revision"], before["revision"]);
        viewer.shutdown().await;
    }

    #[tokio::test]
    async fn browser_serves_only_allowlisted_cached_sheets_and_isolates_svg() {
        let fixture = Fixture::new();
        let viewer = fixture.start().await;
        let client = Client::new(&viewer);
        let root = client
            .http
            .get(client.sheet_url(&fixture.root))
            .bearer_auth(&client.token)
            .send()
            .await
            .unwrap();
        assert_eq!(root.status(), 200);
        assert!(root.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .starts_with("sandbox;"));
        assert_eq!(root.headers()["x-content-type-options"], "nosniff");
        assert_eq!(root.headers()["referrer-policy"], "no-referrer");
        for file in [
            fixture.root.parent().unwrap().join("secret.kicad_sch"),
            fixture.root.parent().unwrap().join("../secret"),
        ] {
            assert_eq!(
                client
                    .http
                    .get(client.sheet_url(&file))
                    .bearer_auth(&client.token)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                404
            );
            assert_eq!(
                client
                    .post("/refresh", &file)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                404
            );
            assert_eq!(
                client
                    .post("/open-in-kicad", &file)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                404
            );
        }
        let page = client.http.get(&viewer.url).send().await.unwrap();
        assert!(page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'self'"));
        assert!(page.text().await.unwrap().contains("src=\"viewer.js\""));
        let script = client
            .get("/viewer.js")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(script.contains("setAttribute('sandbox', 'allow-same-origin')"));
        assert!(!script.contains("allow-scripts"));
        viewer.shutdown().await;
    }
    #[tokio::test]
    async fn browser_refreshes_from_new_snapshot_and_reconnects_to_latest_state() {
        let fixture = Fixture::new();
        let viewer = fixture.start().await;
        let client = Client::new(&viewer);
        let before = client.quiet().await;
        let mut source = std::fs::read_to_string(&fixture.root).unwrap();
        source.push('\n');
        std::fs::write(&fixture.root, &source).unwrap();
        let (a, b) = tokio::join!(
            client.post("/refresh", &fixture.root).send(),
            client.post("/refresh", &fixture.root).send()
        );
        for response in [a.unwrap(), b.unwrap()] {
            assert_eq!(response.status(), 200);
            assert!(response
                .json::<String>()
                .await
                .unwrap()
                .contains(&format!(">{}</text>", source.len())));
        }
        let after = client.state().await;
        assert!(after["revision"].as_u64().unwrap() > before["revision"].as_u64().unwrap());
        let mut events = client
            .http
            .get(format!("{}/events?token={}", client.base, client.token))
            .header("Last-Event-ID", "0")
            .send()
            .await
            .unwrap();
        let first = tokio::time::timeout(Duration::from_secs(2), async {
            let mut bytes = Vec::new();
            while !bytes.windows(2).any(|s| s == b"\n\n") {
                bytes.extend_from_slice(&events.chunk().await.unwrap().unwrap());
            }
            String::from_utf8(bytes).unwrap()
        })
        .await
        .unwrap();
        assert!(first.contains("event: state"));
        let data: serde_json::Value = serde_json::from_str(
            first
                .lines()
                .find_map(|l| l.strip_prefix("data: "))
                .unwrap(),
        )
        .unwrap();
        assert!(data["revision"].as_u64().unwrap() >= after["revision"].as_u64().unwrap());
        viewer.shutdown().await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while events.chunk().await.unwrap().is_some() {}
        })
        .await
        .unwrap();
        assert!(client.get("/state").send().await.is_err());
    }
    #[tokio::test]
    async fn browser_watcher_invalidates_cache_without_manual_refresh() {
        let fixture = Fixture::new();
        let viewer = fixture.start().await;
        let client = Client::new(&viewer);
        let before = client.quiet().await;
        let mut source = std::fs::read_to_string(&fixture.root).unwrap();
        source.push('\n');
        std::fs::write(&fixture.root, &source).unwrap();
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let state = client.state().await;
                if state["revision"].as_u64().unwrap() > before["revision"].as_u64().unwrap()
                    && state["rendering"] == false
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let svg = client
            .http
            .get(client.sheet_url(&fixture.root))
            .bearer_auth(&client.token)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(svg.contains(&format!(">{}</text>", source.len())));
        viewer.shutdown().await;
    }
    async fn wait_revision(client: &Client, revision: u64) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let state = client.state().await;
                if state["revision"].as_u64().unwrap() > revision && state["rendering"] == false {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn browser_watches_new_hierarchy_directories_and_renders_only_changed_sheets() {
        let fixture = Fixture::new();
        let directory = fixture.root.parent().unwrap().join("nested");
        std::fs::create_dir(&directory).unwrap();
        let child = directory.join("child.kicad_sch");
        std::fs::copy(&fixture.root, &child).unwrap();
        let mut sch = copperpour_schematic_editor::Schematic::load(&fixture.root).unwrap();
        sch.add_sheet(copperpour_schematic_editor::Sheet::new(
            "child",
            "nested/child.kicad_sch",
            50.0,
            50.0,
            80.0,
            50.0,
        ));
        sch.overwrite().unwrap();
        let viewer = fixture.start().await;
        let client = Client::new(&viewer);
        let before = client.quiet().await;
        let calls = fixture.cli.with_extension("calls");
        std::fs::write(&calls, "").unwrap();
        let mut source = std::fs::read_to_string(&child).unwrap();
        source.push('\n');
        std::fs::write(&child, source).unwrap();
        let after = wait_revision(&client, before["revision"].as_u64().unwrap()).await;
        let calls = std::fs::read_to_string(&calls).unwrap();
        assert!(!calls.is_empty());
        assert!(
            calls.lines().all(|line| line == "child.kicad_sch"),
            "{calls}"
        );
        let new_directory = fixture.root.parent().unwrap().join("new");
        std::fs::create_dir(&new_directory).unwrap();
        let new_child = new_directory.join("added.kicad_sch");
        std::fs::copy(&child, &new_child).unwrap();
        let mut sch = copperpour_schematic_editor::Schematic::load(&fixture.root).unwrap();
        sch.add_sheet(copperpour_schematic_editor::Sheet::new(
            "added",
            "new/added.kicad_sch",
            60.0,
            60.0,
            80.0,
            50.0,
        ));
        sch.overwrite().unwrap();
        let added = wait_revision(&client, after["revision"].as_u64().unwrap()).await;
        assert_eq!(added["sheets"].as_array().unwrap().len(), 3);
        let mut source = std::fs::read_to_string(&new_child).unwrap();
        source.push('\n');
        std::fs::write(&new_child, &source).unwrap();
        wait_revision(&client, added["revision"].as_u64().unwrap()).await;
        let svg = client
            .http
            .get(client.sheet_url(&new_child))
            .bearer_auth(&client.token)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(svg.contains(&format!(">{}</text>", source.len())));
        viewer.shutdown().await;
    }

    #[tokio::test]
    async fn browser_render_failure_is_unavailable_not_a_stale_success() {
        let fixture = Fixture::new();
        let viewer = fixture.start().await;
        let client = Client::new(&viewer);
        client.quiet().await;
        std::fs::write(
            &fixture.cli,
            "#!/bin/sh\necho 'known renderer diagnostic' >&2\nexit 1\n",
        )
        .unwrap();
        let response = client.post("/refresh", &fixture.root).send().await.unwrap();
        assert_eq!(response.status(), 503);
        let error = response.text().await.unwrap();
        assert!(error.contains("no valid nonempty SVG"));
        assert!(error.contains("known renderer diagnostic"));
        assert_eq!(
            client
                .http
                .get(client.sheet_url(&fixture.root))
                .bearer_auth(&client.token)
                .send()
                .await
                .unwrap()
                .status(),
            503
        );
        assert!(!client.state().await["errors"]
            .as_array()
            .unwrap()
            .is_empty());
        viewer.shutdown().await;
    }
    #[tokio::test]
    async fn browser_refuses_a_malformed_render_artifact_even_after_successful_exit() {
        let fixture = Fixture::new();
        let viewer = fixture.start().await;
        let client = Client::new(&viewer);
        client.quiet().await;
        std::fs::write(&fixture.cli, "#!/bin/sh\nmkdir -p \"$5\"\nprintf '<html/>' > \"$5/$(basename \"$6\" .kicad_sch).svg\"\n").unwrap();
        assert_eq!(
            client
                .post("/refresh", &fixture.root)
                .send()
                .await
                .unwrap()
                .status(),
            503
        );
        viewer.shutdown().await;
    }

    #[tokio::test]
    async fn browser_refuses_absolute_child_paths_and_non_schematic_roots() {
        let fixture = Fixture::new();
        let text = fixture.root.with_extension("txt");
        std::fs::copy(&fixture.root, &text).unwrap();
        assert!(
            BrowserViewer::start(&text, fixture.cli.to_str().unwrap(), Duration::from_secs(2))
                .await
                .err()
                .unwrap()
                .contains("schematic file")
        );
        let child = fixture.root.parent().unwrap().join("child.kicad_sch");
        std::fs::copy(&fixture.root, &child).unwrap();
        let mut schematic = copperpour_schematic_editor::Schematic::load(&fixture.root).unwrap();
        schematic.add_sheet(copperpour_schematic_editor::Sheet::new(
            "child",
            child.to_str().unwrap(),
            50.0,
            50.0,
            80.0,
            50.0,
        ));
        schematic.overwrite().unwrap();
        assert!(BrowserViewer::start(
            &fixture.root,
            fixture.cli.to_str().unwrap(),
            Duration::from_secs(2)
        )
        .await
        .err()
        .unwrap()
        .contains("relative child paths"));
    }
    #[tokio::test]
    async fn browser_refuses_a_cyclic_hierarchy_instead_of_rendering_a_partial_tree() {
        let fixture = Fixture::new();
        let mut sch = copperpour_schematic_editor::Schematic::load(&fixture.root).unwrap();
        sch.add_sheet(copperpour_schematic_editor::Sheet::new(
            "self",
            "root.revision.kicad_sch",
            50.0,
            50.0,
            80.0,
            50.0,
        ));
        sch.overwrite().unwrap();
        assert!(BrowserViewer::start(
            &fixture.root,
            fixture.cli.to_str().unwrap(),
            Duration::from_secs(2)
        )
        .await
        .err()
        .unwrap()
        .contains("cyclic"));
    }

    #[tokio::test]
    async fn browser_refuses_outside_symlinks_and_incomplete_deep_hierarchies() {
        let fixture = Fixture::new();
        let external = tempfile::tempdir().unwrap();
        let outside = external.path().join("outside.kicad_sch");
        std::fs::copy(&fixture.root, &outside).unwrap();
        let link = fixture.root.parent().unwrap().join("linked.kicad_sch");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let mut sch = copperpour_schematic_editor::Schematic::load(&fixture.root).unwrap();
        sch.add_sheet(copperpour_schematic_editor::Sheet::new(
            "linked",
            "linked.kicad_sch",
            50.0,
            50.0,
            80.0,
            50.0,
        ));
        sch.overwrite().unwrap();
        assert!(BrowserViewer::start(
            &fixture.root,
            fixture.cli.to_str().unwrap(),
            Duration::from_secs(2)
        )
        .await
        .err()
        .unwrap()
        .contains("outside the root"));
        let fixture = Fixture::new();
        let source = std::fs::read(&fixture.root).unwrap();
        let mut previous = fixture.root.clone();
        for index in 1..=21 {
            let name = format!("level-{index}.kicad_sch");
            let child = fixture.root.parent().unwrap().join(&name);
            std::fs::write(&child, &source).unwrap();
            let mut sch = copperpour_schematic_editor::Schematic::load(&previous).unwrap();
            sch.add_sheet(copperpour_schematic_editor::Sheet::new(
                "child", &name, 50.0, 50.0, 80.0, 50.0,
            ));
            sch.overwrite().unwrap();
            previous = child;
        }
        assert!(BrowserViewer::start(
            &fixture.root,
            fixture.cli.to_str().unwrap(),
            Duration::from_secs(2)
        )
        .await
        .err()
        .unwrap()
        .contains("discovery depth"));
    }
    #[tokio::test]
    async fn browser_refuses_zero_render_timeout_before_launch() {
        let fixture = Fixture::new();
        assert!(
            BrowserViewer::start(&fixture.root, fixture.cli.to_str().unwrap(), Duration::ZERO)
                .await
                .err()
                .unwrap()
                .contains("positive")
        );
    }

    #[tokio::test]
    async fn browser_refuses_live_file_fallback_after_a_failed_snapshot_copy() {
        let fixture = Fixture::new();
        let absent = fixture.root.parent().unwrap().join("absent.kicad_sch");
        let entries = vec![crate::SheetEntry {
            name: "absent".into(),
            file: absent.to_string_lossy().into_owned(),
            depth: 0,
        }];
        let (_stop, rx) = watch::channel(false);
        let (cache, errors) = render(
            &fixture.root,
            &entries,
            fixture.cli.to_str().unwrap(),
            Duration::from_secs(2),
            rx,
        )
        .await;
        assert!(cache.is_empty());
        assert!(errors.iter().any(|e| e.contains("snapshot copy failed")));
    }
    #[tokio::test]
    async fn browser_refuses_an_uncopied_project_instead_of_using_default_settings() {
        let fixture = Fixture::new();
        let project = fixture.root.with_extension("kicad_pro");
        // Reading a directory as project bytes fails even for root/capability-
        // privileged CI runners, unlike chmod(000), which they can bypass.
        std::fs::create_dir(&project).unwrap();
        let entries = crate::walk_sheet_tree(&fixture.root);
        let (_stop, rx) = watch::channel(false);
        let (cache, errors) = render(
            &fixture.root,
            &entries,
            fixture.cli.to_str().unwrap(),
            Duration::from_secs(2),
            rx,
        )
        .await;
        assert!(cache.is_empty());
        assert!(errors
            .iter()
            .any(|error| error.contains("project snapshot copy failed")));
        assert!(
            !fixture.cli.with_extension("calls").exists(),
            "renderer must not run after failed project snapshot"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn browser_shutdown_cancels_and_reaps_inflight_renderer() {
        let fixture = Fixture::new();
        let viewer = fixture.start().await;
        let client = Client::new(&viewer);
        client.quiet().await;
        std::fs::write(
            &fixture.cli,
            "#!/bin/sh\necho $$ > \"$0.pid\"\nexec sleep 60\n",
        )
        .unwrap();
        let http = client.post("/refresh", &fixture.root);
        let request = tokio::spawn(async move { http.send().await });
        let pidfile = fixture.cli.with_extension("pid");
        tokio::time::timeout(Duration::from_secs(2), async {
            while !pidfile.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let pid = std::fs::read_to_string(pidfile).unwrap();
        tokio::time::timeout(Duration::from_secs(2), viewer.shutdown())
            .await
            .unwrap();
        assert!(!Path::new(&format!("/proc/{}", pid.trim())).exists());
        let _ = request.await;
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn browser_cancelled_start_kills_its_renderer_before_a_handle_exists() {
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if Path::new(&format!("/proc/{}", self.0)).exists() {
                    let _ = std::process::Command::new("kill")
                        .args(["-KILL", &self.0])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
            }
        }
        let fixture = Fixture::new();
        std::fs::write(
            &fixture.cli,
            "#!/bin/sh\necho $$ > \"$0.pid\"\nexec sleep 60\n",
        )
        .unwrap();
        let root = fixture.root.clone();
        let cli = fixture.cli.clone();
        let startup = tokio::spawn(async move {
            BrowserViewer::start(&root, cli.to_str().unwrap(), Duration::from_secs(60)).await
        });
        let pidfile = fixture.cli.with_extension("pid");
        tokio::time::timeout(Duration::from_secs(2), async {
            while !pidfile.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let pid = std::fs::read_to_string(pidfile).unwrap().trim().to_string();
        let _cleanup = Cleanup(pid.clone());
        startup.abort();
        let _ = startup.await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn owned_http_connections_close_even_when_a_response_cannot_drain() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, rx) = watch::channel(false);
        let app = Router::new().route(
            "/pending",
            get(|| async {
                let stream =
                    tokio_stream::once(Ok::<_, Infallible>(Event::default().data("ready")))
                        .chain(tokio_stream::pending::<Result<Event, Infallible>>());
                Sse::new(stream)
            }),
        );
        let server = tokio::spawn(serve_owned(listener, app, rx));
        let mut response = reqwest::get(format!("http://{address}/pending"))
            .await
            .unwrap();
        assert!(
            String::from_utf8(response.chunk().await.unwrap().unwrap().to_vec())
                .unwrap()
                .contains("ready")
        );
        stop.send_replace(true);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        // A forced close may be EOF or an incomplete chunked-response error.
        // Either must arrive promptly; an accepted task may not outlive shutdown.
        assert!(
            tokio::time::timeout(Duration::from_secs(2), response.chunk())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn browser_renderer_timeout_is_reported_and_cancels_child() {
        let fixture = Fixture::new();
        std::fs::write(&fixture.cli, "#!/bin/sh\nexec sleep 60\n").unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            BrowserViewer::start(
                &fixture.root,
                fixture.cli.to_str().unwrap(),
                Duration::from_millis(100),
            ),
        )
        .await
        .unwrap()
        .err()
        .unwrap();
        assert!(error.contains("render timed out"));
    }
}
