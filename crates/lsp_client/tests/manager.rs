use lsp_client::{ContentChange, DocSource, DocSync, Manager, ServerId, ServerSpec, Status};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn spec(command: &str, args: &[&str]) -> ServerSpec {
    ServerSpec {
        name: "fake".into(),
        command: command.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        extensions: vec!["fk".into()],
        root_markers: vec!["fake.toml".into()],
        init_options: None,
        saved_text_sources: vec!["fake".into()],
    }
}

fn fake() -> &'static str {
    env!("CARGO_BIN_EXE_fake_lsp")
}

fn project() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("forge-lsp-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("fake.toml"), "").unwrap();
    dir
}

fn wait_for(m: &Manager, id: ServerId, what: &str, ok: impl Fn(&lsp_client::ServerInfo) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let info = m.servers().into_iter().find(|s| s.id == id).unwrap();
        if ok(&info) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}: {info:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn routes_by_extension_and_root() {
    let root = project();
    let m = Manager::new(vec![spec(fake(), &[])], || {});
    let file = root.join("src").join("a.fk");
    let id = m.ensure_for_file(&file).unwrap();
    assert_eq!(m.ensure_for_file(&root.join("b.FK")), Some(id), "same root reuses the server");
    assert_eq!(m.servers()[0].root, root);
    assert!(m.ensure_for_file(Path::new("x.txt")).is_none());
}

#[test]
fn starts_runs_restarts_and_stops() {
    let root = project();
    let m = Manager::new(vec![spec(fake(), &[])], || {});
    let id = m.ensure_for_file(&root.join("a.fk")).unwrap();
    wait_for(&m, id, "running", |s| s.status == Status::Running);
    wait_for(&m, id, "progress", |s| s.activity.as_deref() == Some("Indexing 42%"));
    let info = m.servers().remove(0);
    assert_eq!(info.version.as_deref(), Some("fake 1.0"));
    let logs = m.logs(id).join("\n");
    assert!(logs.contains("[info] hello from fake"), "{logs}");
    assert!(logs.contains("[stderr] fake server up"), "{logs}");

    m.restart(id);
    wait_for(&m, id, "running again", |s| {
        s.status == Status::Running && s.activity.is_some()
    });
    assert!(m.logs(id).join("\n").contains("restart requested"));

    m.stop(id);
    wait_for(&m, id, "stopped", |s| s.status == Status::Stopped);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(m.servers()[0].status, Status::Stopped, "a stopped server stays stopped");
    m.shutdown();
}

#[test]
fn crash_restarts_with_backoff_then_gives_up() {
    let root = project();
    let m = Manager::new(vec![spec(fake(), &["crash"])], || {});
    let id = m.ensure_for_file(&root.join("a.fk")).unwrap();
    wait_for(&m, id, "give up", |s| s.message.contains("giving up"));
    assert_eq!(m.servers()[0].status, Status::Error);
    assert_eq!(
        m.logs(id).iter().filter(|l| l.starts_with("-- started")).count(),
        4,
        "initial start plus three restarts"
    );
    m.shutdown();
}

#[test]
fn missing_binary_is_not_installed() {
    let root = project();
    let m = Manager::new(vec![spec("definitely-not-a-real-server", &[])], || {});
    let id = m.ensure_for_file(&root.join("a.fk")).unwrap();
    wait_for(&m, id, "not installed", |s| s.status == Status::NotInstalled);
    assert!(m.servers()[0].message.contains("not found"));
}

#[test]
fn on_change_fires() {
    let root = project();
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = count.clone();
    let m = Manager::new(vec![spec(fake(), &[])], move || {
        c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let id = m.ensure_for_file(&root.join("a.fk")).unwrap();
    wait_for(&m, id, "running", |s| s.status == Status::Running);
    assert!(count.load(std::sync::atomic::Ordering::SeqCst) >= 2);
}

struct Doc {
    text: String,
    pending: Option<Vec<ContentChange>>,
}

impl DocSource for Doc {
    fn full_text(&mut self) -> String {
        self.pending = Some(Vec::new());
        self.text.clone()
    }
    fn take_changes(&mut self) -> Option<Vec<ContentChange>> {
        self.pending.replace(Vec::new())
    }
}

fn wait_log(m: &Manager, id: ServerId, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let logs = m.logs(id).join("
");
        if logs.contains(needle) {
            return logs;
        }
        assert!(Instant::now() < deadline, "no `{needle}` in:
{logs}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn documents_open_change_save_close_and_reopen_after_restart() {
    let root = project();
    let m = Manager::new(vec![spec(fake(), &[])], || {});
    let file = root.join("a.fk");
    let id = m.ensure_for_file(&file).unwrap();
    let mut sync = DocSync::new(&file, "fake");
    let mut doc = Doc { text: "hello".into(), pending: None };

    // Not ready yet (or just ready): sync must not panic and eventually opens.
    wait_for(&m, id, "running", |s| s.status == Status::Running);
    sync.sync(&m, id, &mut doc);
    let logs = wait_log(&m, id, "recv textDocument/didOpen");
    assert!(logs.contains("\"text\":\"hello\"") && logs.contains("\"version\":1"), "{logs}");

    doc.pending = Some(vec![ContentChange { start: (0, 5), end: (0, 5), text: "!".into() }]);
    sync.sync(&m, id, &mut doc);
    let logs = wait_log(&m, id, "recv textDocument/didChange");
    assert!(logs.contains("\"version\":2") && logs.contains("\"character\":5"), "{logs}");

    // Unknown history falls back to the full text.
    doc.text = "fresh".into();
    doc.pending = None;
    sync.sync(&m, id, &mut doc);
    wait_log(&m, id, "full resync");
    wait_log(&m, id, "\"text\":\"fresh\"");

    // No changes: nothing is sent.
    let before = m.logs(id).len();
    sync.sync(&m, id, &mut doc);
    assert_eq!(m.logs(id).len(), before);

    sync.save(&m, id);
    wait_log(&m, id, "recv textDocument/didSave");

    // A restarted server has lost the document: the next sync re-opens it.
    m.restart(id);
    wait_for(&m, id, "restarted", |s| s.status == Status::Running && s.activity.is_some());
    sync.sync(&m, id, &mut doc);
    let logs = wait_log(&m, id, "[sync] didOpen");
    assert_eq!(logs.matches("[sync] didOpen").count(), 2, "{logs}");

    sync.close(&m);
    wait_log(&m, id, "recv textDocument/didClose");
    m.shutdown();
}

#[test]
fn normalize_expands_short_names_and_strips_verbatim_prefix() {
    let dir = project();
    let n = lsp_client::normalize_path(&dir.join("src").join("..").join("fake.toml"));
    let text = n.to_string_lossy();
    assert!(!text.starts_with(r"\?\") && !text.contains(".."), "{text}");
    assert!(n.is_file());
    // A path that doesn't exist yet is left alone.
    let missing = dir.join("nope.rs");
    assert_eq!(lsp_client::normalize_path(&missing), missing);
}

#[test]
fn diagnostics_are_stored_counted_and_cleared() {
    let root = project();
    let m = Manager::new(vec![spec(fake(), &[])], || {});
    let file = root.join("a.fk");
    let id = m.ensure_for_file(&file).unwrap();
    let mut sync = DocSync::new(&file, "fake");
    let mut doc = Doc { text: "hello".into(), pending: None };
    wait_for(&m, id, "running", |s| s.status == Status::Running);
    let before = m.diag_gen();
    sync.sync(&m, id, &mut doc);

    let deadline = Instant::now() + Duration::from_secs(10);
    while m.diag_counts() != (1, 1) {
        assert!(Instant::now() < deadline, "counts: {:?}", m.diag_counts());
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(m.diag_gen() > before);
    // Looked up by a key that URIs, drive-letter case and separators don't affect.
    let found = m.diagnostics(&lsp_client::path_key(&file)).expect("diagnostics for the file");
    assert_eq!(found.version, Some(1));
    assert_eq!(found.diags.len(), 2);
    assert_eq!(m.all_diagnostics().len(), 1);
    assert_eq!(lsp_client::path_key(&m.all_diagnostics()[0].path), lsp_client::path_key(&file));
    assert_eq!(found.diags[0].text(), "bad thing [fake E1]");
    assert_eq!((found.diags[0].start, found.diags[0].end), ((0, 1), (0, 3)));

    m.stop(id);
    wait_for(&m, id, "stopped", |s| s.status == Status::Stopped);
    assert_eq!(m.diag_counts(), (0, 0), "a stopped server's diagnostics go away");
    assert!(m.diagnostics(&lsp_client::path_key(&file)).is_none());
    m.shutdown();
}
