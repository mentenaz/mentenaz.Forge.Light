//! Server registry and lifecycle: lazy start per (spec, project root), status,
//! restart/stop, crash back-off and per-server logs. Thread-safe; the UI polls
//! `servers()` after `on_change` fires.

use crate::client::{Client, Event};
use lsp_types::{MessageType, NumberOrString, ProgressParamsValue, WorkDoneProgress};
use serde_json::Value;
use crate::diag::{Diag, ERROR, FileDiags, WARNING, dedupe, uri_key, uri_to_path};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::{Handle, Runtime};

const LOG_CAP: usize = 2000;
const MAX_RETRIES: u32 = 3;
const STABLE_AFTER: Duration = Duration::from_secs(30);

pub type ServerId = u64;

#[derive(Clone, Debug)]
pub struct ServerSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// File extensions (no dot) this server handles.
    pub extensions: Vec<String>,
    /// Files whose presence marks a project root, nearest ancestor wins.
    pub root_markers: Vec<String>,
    pub init_options: Option<Value>,
    /// Diagnostic `source`s that describe the last saved file, not the live
    /// text (rust-analyzer republishes its `cargo check` results unmoved).
    pub saved_text_sources: Vec<String>,
}

impl ServerSpec {
    pub fn rust_analyzer() -> Self {
        ServerSpec {
            name: "rust-analyzer".into(),
            command: "rust-analyzer".into(),
            args: vec![],
            extensions: vec!["rs".into()],
            root_markers: vec!["Cargo.toml".into(), "rust-project.json".into()],
            init_options: None,
            saved_text_sources: vec!["rustc".into(), "clippy".into()],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Starting,
    Running,
    /// Running, but the server reported a warning or error message.
    Warning,
    Error,
    Stopped,
    NotInstalled,
}

#[derive(Clone, Debug)]
pub struct ServerInfo {
    pub id: ServerId,
    pub name: String,
    pub root: PathBuf,
    pub status: Status,
    pub message: String,
    pub version: Option<String>,
    /// Current `$/progress` activity, e.g. "Indexing: 42%".
    pub activity: Option<String>,
}

struct Entry {
    info: ServerInfo,
    spec: ServerSpec,
    client: Option<Client>,
    logs: VecDeque<String>,
    /// Bumped on every start/stop so stale tasks and events are ignored.
    generation: u64,
    retries: u32,
    started: Option<Instant>,
    /// Set to `generation` once `initialize` completed for that start.
    ready_gen: Option<u64>,
    /// What the server last published, by `uri_key`.
    diags: HashMap<String, FileDiags>,
    progress: Vec<(String, String)>,
}

impl Entry {
    fn log(&mut self, line: impl Into<String>) {
        if self.logs.len() == LOG_CAP {
            self.logs.pop_front();
        }
        self.logs.push_back(line.into());
    }

    fn set(&mut self, status: Status, message: impl Into<String>) {
        self.info.status = status;
        self.info.message = message.into();
    }
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    next_id: ServerId,
}

struct Shared {
    state: Mutex<State>,
    handle: Handle,
    /// Bumped whenever any diagnostics change.
    diag_gen: AtomicU64,
    on_change: Arc<dyn Fn() + Send + Sync>,
}

pub struct Manager {
    shared: Arc<Shared>,
    specs: Vec<ServerSpec>,
    runtime: Mutex<Option<Runtime>>,
}

impl Manager {
    /// `on_change` runs on a background thread whenever any server's state or
    /// logs change; marshal to the UI thread inside it.
    pub fn new(specs: Vec<ServerSpec>, on_change: impl Fn() + Send + Sync + 'static) -> Manager {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("lsp")
            .enable_all()
            .build()
            .expect("tokio runtime");
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            handle: runtime.handle().clone(),
            diag_gen: AtomicU64::new(0),
            on_change: Arc::new(on_change),
        });
        Manager { shared, specs, runtime: Mutex::new(Some(runtime)) }
    }

    pub fn servers(&self) -> Vec<ServerInfo> {
        let state = self.shared.state.lock().unwrap();
        state.entries.iter().map(|e| e.info.clone()).collect()
    }

    pub fn logs(&self, id: ServerId) -> Vec<String> {
        let state = self.shared.state.lock().unwrap();
        state.entries.iter().find(|e| e.info.id == id).map_or_else(Vec::new, |e| {
            e.logs.iter().cloned().collect()
        })
    }

    /// The live connection, for sending requests (phases 2+).
    pub fn client(&self, id: ServerId) -> Option<Client> {
        let state = self.shared.state.lock().unwrap();
        state.entries.iter().find(|e| e.info.id == id)?.client.clone()
    }

    /// The connection once the server finished `initialize`, with a number
    /// that changes with every (re)start so documents know to re-open.
    pub fn ready(&self, id: ServerId) -> Option<(Client, u64)> {
        let state = self.shared.state.lock().unwrap();
        let e = state.entries.iter().find(|e| e.info.id == id)?;
        (e.ready_gen == Some(e.generation)).then(|| (e.client.clone(), e.generation)).and_then(
            |(c, g)| c.map(|c| (c, g)),
        )
    }

    /// A handle that runs the `on_change` callback, for tasks that finish later.
    pub fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.shared.on_change.clone()
    }

    /// Changes whenever a server publishes or clears diagnostics.
    pub fn diag_gen(&self) -> u64 {
        self.shared.diag_gen.load(Ordering::SeqCst)
    }

    /// What the servers last published for `key` (a `path_key`).
    pub fn diagnostics(&self, key: &str) -> Option<FileDiags> {
        let state = self.shared.state.lock().unwrap();
        state.entries.iter().find_map(|e| e.diags.get(key)).cloned()
    }

    /// Everything the servers last published, for every file they reported on.
    pub fn all_diagnostics(&self) -> Vec<FileDiags> {
        let state = self.shared.state.lock().unwrap();
        state.entries.iter().flat_map(|e| e.diags.values()).cloned().collect()
    }

    /// (errors, warnings) over every file the servers reported on.
    pub fn diag_counts(&self) -> (usize, usize) {
        let state = self.shared.state.lock().unwrap();
        let all = state.entries.iter().flat_map(|e| e.diags.values()).flat_map(|f| &f.diags);
        all.fold((0, 0), |(errors, warnings), d| match d.severity {
            ERROR => (errors + 1, warnings),
            WARNING => (errors, warnings + 1),
            _ => (errors, warnings),
        })
    }

    /// Appends a line to the server's log.
    pub fn log(&self, id: ServerId, line: impl Into<String>) {
        let line = line.into();
        self.shared.with(id, |e| e.log(line));
    }

    pub fn handle(&self) -> Handle {
        self.shared.handle.clone()
    }

    /// Finds (or lazily starts) the server responsible for `file`.
    pub fn ensure_for_file(&self, file: &Path) -> Option<ServerId> {
        let file = &normalize_path(file);
        let ext = file.extension()?.to_string_lossy().to_ascii_lowercase();
        let spec = self
            .specs
            .iter()
            .find(|s| s.extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)))?;
        let root = find_root(file, &spec.root_markers);
        let id = {
            let mut state = self.shared.state.lock().unwrap();
            if let Some(e) =
                state.entries.iter().find(|e| e.spec.name == spec.name && e.info.root == root)
            {
                return Some(e.info.id);
            }
            state.next_id += 1;
            let id = state.next_id;
            state.entries.push(Entry {
                info: ServerInfo {
                    id,
                    name: spec.name.clone(),
                    root,
                    status: Status::Starting,
                    message: String::new(),
                    version: None,
                    activity: None,
                },
                spec: spec.clone(),
                client: None,
                logs: VecDeque::new(),
                generation: 0,
                retries: 0,
                started: None,
                ready_gen: None,
                diags: HashMap::new(),
                progress: Vec::new(),
            });
            id
        };
        self.shared.start(id, Duration::ZERO);
        Some(id)
    }

    pub fn restart(&self, id: ServerId) {
        let old = self.shared.with(id, |e| {
            e.generation += 1;
            e.retries = 0;
            e.info.activity = None;
            e.progress.clear();
            e.set(Status::Starting, "Restarting");
            e.log("-- restart requested --");
            e.diags.clear();
            e.client.take()
        });
        let shared = self.shared.clone();
        self.shared.handle.spawn(async move {
            if let Some(Some(old)) = old {
                old.shutdown().await;
            }
            shared.start(id, Duration::ZERO);
        });
    }

    pub fn stop(&self, id: ServerId) {
        if let Some(Some(old)) = self.shared.with(id, |e| {
            e.generation += 1;
            e.retries = 0;
            e.info.activity = None;
            e.progress.clear();
            e.set(Status::Stopped, "Stopped");
            e.log("-- stop requested --");
            e.diags.clear();
            e.client.take()
        }) {
            self.shared.handle.spawn(async move { old.shutdown().await });
        }
    }

    pub fn restart_all(&self) {
        for s in self.servers() {
            self.restart(s.id);
        }
    }

    pub fn stop_all(&self) {
        for s in self.servers() {
            self.stop(s.id);
        }
    }

    /// Stops every server and waits (bounded) for the processes to exit.
    pub fn shutdown(&self) {
        let Some(runtime) = self.runtime.lock().unwrap().take() else { return };
        let clients: Vec<Client> = {
            let mut state = self.shared.state.lock().unwrap();
            state
                .entries
                .iter_mut()
                .filter_map(|e| {
                    e.generation += 1;
                    e.client.take()
                })
                .collect()
        };
        runtime.block_on(async {
            let tasks: Vec<_> =
                clients.into_iter().map(|c| tokio::spawn(async move { c.shutdown().await })).collect();
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                for t in tasks {
                    let _ = t.await;
                }
            })
            .await;
        });
        runtime.shutdown_timeout(Duration::from_secs(1));
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Shared {
    /// Runs `f` on the entry and then notifies listeners.
    fn with<T>(&self, id: ServerId, f: impl FnOnce(&mut Entry) -> T) -> Option<T> {
        let out = {
            let mut state = self.state.lock().unwrap();
            state.entries.iter_mut().find(|e| e.info.id == id).map(f)
        };
        if out.is_some() {
            (self.on_change)();
        }
        out
    }

    /// Like `with`, but only while `generation` is still current.
    fn with_gen<T>(
        &self,
        id: ServerId,
        generation: u64,
        f: impl FnOnce(&mut Entry) -> T,
    ) -> Option<T> {
        self.with(id, |e| (e.generation == generation).then(|| f(e))).flatten()
    }

    fn start(self: &Arc<Self>, id: ServerId, delay: Duration) {
        let Some(generation) = self.with(id, |e| {
            e.generation += 1;
            e.generation
        }) else {
            return;
        };
        let shared = self.clone();
        self.handle.spawn(async move { shared.run(id, generation, delay).await });
    }

    async fn run(self: Arc<Self>, id: ServerId, generation: u64, delay: Duration) {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let Some((spec, root)) = self.with_gen(id, generation, |e| {
            e.set(Status::Starting, "Starting");
            (e.spec.clone(), e.info.root.clone())
        }) else {
            return;
        };

        let Some(command) = resolve_command(&spec.command) else {
            let msg = format!("`{}` was not found on PATH", spec.command);
            self.with_gen(id, generation, |e| {
                e.log(msg.clone());
                e.set(Status::NotInstalled, msg);
            });
            return;
        };
        let (client, events) = match Client::spawn(&self.handle, &command, &spec.args, Some(&root))
        {
            Ok(pair) => pair,
            Err(err) => {
                let msg = format!("failed to start: {err}");
                self.with_gen(id, generation, |e| {
                    e.log(msg.clone());
                    e.set(Status::Error, msg);
                });
                return;
            }
        };
        let stored = self.with_gen(id, generation, |e| {
            e.client = Some(client.clone());
            e.started = Some(Instant::now());
            e.log(format!("-- started {} --", command.display()));
        });
        if stored.is_none() {
            client.kill();
            return;
        }
        let shared = self.clone();
        self.handle.spawn(async move { shared.events(id, generation, events).await });

        match client.initialize(Some(&root), spec.init_options.clone()).await {
            Ok(res) => {
                let version = res.server_info.map(|i| match i.version {
                    Some(v) => format!("{} {v}", i.name),
                    None => i.name,
                });
                self.with_gen(id, generation, |e| {
                    e.ready_gen = Some(generation);
                    e.info.version = version;
                    // A warning raised while starting should survive.
                    if e.info.status == Status::Starting {
                        e.set(Status::Running, "Running");
                    }
                });
            }
            Err(err) => {
                let msg = format!("initialize failed: {err}");
                let current = self
                    .with_gen(id, generation, |e| {
                        e.log(msg.clone());
                        e.set(Status::Error, msg);
                    })
                    .is_some();
                if current {
                    client.kill();
                }
            }
        }
    }

    async fn events(
        self: Arc<Self>,
        id: ServerId,
        generation: u64,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<Event>,
    ) {
        while let Some(event) = rx.recv().await {
            let alive = match event {
                Event::Stderr(line) => self.with_gen(id, generation, |e| e.log(format!("[stderr] {line}"))),
                Event::LogMessage(p) => self.with_gen(id, generation, |e| {
                    e.log(format!("[{}] {}", level(p.typ), p.message));
                }),
                Event::ShowMessage(p) => self.with_gen(id, generation, |e| {
                    e.log(format!("[{}] {}", level(p.typ), p.message));
                    if p.typ == MessageType::ERROR || p.typ == MessageType::WARNING {
                        e.set(Status::Warning, p.message);
                    }
                }),
                Event::Progress(p) => self.with_gen(id, generation, |e| progress(e, p)),
                Event::Diagnostics(p) => {
                    let key = uri_key(p.uri.as_str());
                    let stored = self.with_gen(id, generation, |e| {
                        let shown: Vec<String> = p
                            .diagnostics
                            .iter()
                            .take(6)
                            .map(|d| {
                                let (s, t) = (d.range.start, d.range.end);
                                format!("{}:{}-{}:{}", s.line, s.character, t.line, t.character)
                            })
                            .collect();
                        e.log(format!(
                            "[diagnostics] {} v{:?}: {} item(s) {}",
                            key.rsplit('/').next().unwrap_or(&key),
                            p.version,
                            p.diagnostics.len(),
                            shown.join(" ")
                        ));
                        if p.diagnostics.is_empty() {
                            e.diags.remove(&key);
                        } else {
                            let diags: Vec<Diag> = p
                                .diagnostics
                                .into_iter()
                                .map(|d| {
                                    let mut d = Diag::from(d);
                                    d.saved_text = d.source.as_ref().is_some_and(|s| e.spec.saved_text_sources.contains(s));
                                    d
                                })
                                .collect();
                            let diags = dedupe(diags);
                            e.diags.insert(key, FileDiags { path: uri_to_path(p.uri.as_str()), version: p.version.map(i64::from), diags });
                        }
                    });
                    self.diag_gen.fetch_add(1, Ordering::SeqCst);
                    stored
                }
                Event::Exited(code) => {
                    self.exited(id, generation, code);
                    return;
                }
            };
            if alive.is_none() {
                return;
            }
        }
    }

    fn exited(self: &Arc<Self>, id: ServerId, generation: u64, code: Option<i32>) {
        let retry = self
            .with_gen(id, generation, |e| {
                e.client = None;
                e.diags.clear();
                e.info.activity = None;
                e.progress.clear();
                if e.started.is_some_and(|t| t.elapsed() > STABLE_AFTER) {
                    e.retries = 0;
                }
                let code = code.map_or("unknown".to_string(), |c| c.to_string());
                e.retries += 1;
                if e.retries <= MAX_RETRIES {
                    let wait = Duration::from_secs(1 << (e.retries - 1));
                    let msg = format!(
                        "Exited (code {code}); restarting in {}s ({}/{MAX_RETRIES})",
                        wait.as_secs(),
                        e.retries
                    );
                    e.log(msg.clone());
                    e.set(Status::Error, msg);
                    Some(wait)
                } else {
                    let msg = format!("Exited (code {code}); giving up after {MAX_RETRIES} restarts");
                    e.log(msg.clone());
                    e.set(Status::Error, msg);
                    None
                }
            })
            .flatten();
        if let Some(wait) = retry {
            self.start(id, wait);
        }
    }
}

fn level(t: MessageType) -> &'static str {
    if t == MessageType::ERROR {
        "error"
    } else if t == MessageType::WARNING {
        "warn"
    } else if t == MessageType::INFO {
        "info"
    } else {
        "log"
    }
}

fn progress(e: &mut Entry, p: lsp_types::ProgressParams) {
    let token = match p.token {
        NumberOrString::Number(n) => n.to_string(),
        NumberOrString::String(s) => s,
    };
    let ProgressParamsValue::WorkDone(work) = p.value;
    match work {
        WorkDoneProgress::Begin(b) => {
            let text = describe(&b.title, b.message.as_deref(), b.percentage);
            e.progress.retain(|(t, _)| *t != token);
            e.progress.push((token, text));
        }
        WorkDoneProgress::Report(r) => {
            if let Some(slot) = e.progress.iter_mut().find(|(t, _)| *t == token) {
                let title = slot.1.split([':', ' ']).next().unwrap_or_default().to_owned();
                slot.1 = describe(&title, r.message.as_deref(), r.percentage);
            }
        }
        WorkDoneProgress::End(_) => e.progress.retain(|(t, _)| *t != token),
    }
    e.info.activity = e.progress.last().map(|(_, text)| text.clone());
}

fn describe(title: &str, message: Option<&str>, percent: Option<u32>) -> String {
    let mut out = title.to_string();
    if let Some(m) = message.filter(|m| !m.is_empty()) {
        out.push_str(": ");
        out.push_str(m);
    }
    if let Some(p) = percent {
        out.push_str(&format!(" {p}%"));
    }
    out
}

/// The full, canonical form of `path` (8.3 short names expanded, no `\?\`
/// prefix), so the server sees the same path as its own file watcher. Falls
/// back to `path` itself when it doesn't exist.
pub fn normalize_path(path: &Path) -> PathBuf {
    let Ok(full) = std::fs::canonicalize(path) else { return path.to_path_buf() };
    match full.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => full,
    }
}

/// Nearest ancestor of `file` containing a root marker; else its directory.
pub fn find_root(file: &Path, markers: &[String]) -> PathBuf {
    let dir = file.parent().unwrap_or(file);
    dir.ancestors()
        .find(|d| markers.iter().any(|m| d.join(m).exists()))
        .unwrap_or(dir)
        .to_path_buf()
}

/// Locates `command` as given (if it has a directory part) or on PATH.
pub fn resolve_command(command: &str) -> Option<PathBuf> {
    let direct = Path::new(command);
    if direct.components().count() > 1 {
        return direct.is_file().then(|| direct.to_path_buf());
    }
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let plain = dir.join(command);
        if direct.extension().is_some() && plain.is_file() {
            return Some(plain);
        }
        for ext in &exts {
            let candidate = dir.join(format!("{command}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
