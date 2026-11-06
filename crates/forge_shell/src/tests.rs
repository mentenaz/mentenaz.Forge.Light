use super::*;
use lsp_client::lsp_types::{self, DocumentSymbolResponse};
use std::str::FromStr;
use std::time::{Duration, Instant};

#[test]
fn outline_rows_keep_depth_order_and_a_kind_tag() {
    let sym = |name: &str, kind, depth| lsp_client::Symbol { name: name.into(), detail: String::new(), kind, depth, line: 0, col: 0 };
    let rows = outline_rows(&[sym("Shape", 23, 0), sym("area", 6, 1), sym("odd", 99, 1)]);
    let got: Vec<_> = rows.iter().map(|r| (r.item, r.depth, r.label.to_string(), r.kind.to_string())).collect();
    assert_eq!(got, [(0, 0, "Shape".into(), "struct".into()), (1, 1, "area".into(), "fn".into()), (2, 1, "odd".into(), String::new())]);
}

#[test]
fn a_search_hit_opens_its_file_with_the_match_selected() {
    let dir = std::env::temp_dir().join(format!("forge_goto_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.go"), "package main\n\tx := needle\n\tcall(needle)\n").unwrap();

    let outcome = search::search(&dir, "needle", true, &Default::default(), MAX_FILE_BYTES, &AtomicBool::new(false), 100);
    let mut shell = Shell::new(dir.clone());
    shell.search.hits = outcome.hits;
    // The second hit sits after a tab; the click selects exactly the word.
    let hit = shell.search.hits[1].clone();
    shell.goto_match(hit.path, hit.line, hit.col, hit.len);
    let tab = shell.active[0].and_then(|a| shell.groups[0].get(a)).unwrap();
    let buf = tab.buffer.as_ref().unwrap();
    assert_eq!(buf.selected_text(), "needle");
    assert_eq!(buf.selection_bounds(), ((2, 6), (2, 12)));
    // Clicking another hit in the same file reuses its tab.
    let first = shell.search.hits[0].clone();
    shell.goto_match(first.path, first.line, first.col, first.len);
    assert_eq!(shell.groups[0].len(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn tabs_come_back_as_they_were_left() {
    let dir = std::env::temp_dir().join(format!("forge_session_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for (name, text) in [("a.rs", "fn a() {}\nfn b() {}\n"), ("b.rs", "let x = 1;\n"), ("gone.rs", "x\n")] {
        fs::write(dir.join(name), text).unwrap();
    }

    let mut before = Shell::new(dir.clone());
    before.open_file(dir.join("a.rs"));
    before.open_file(dir.join("b.rs"));
    before.open_file(dir.join("gone.rs"));
    before.open_panel_tab("git");
    before.toggle_pin(0, 1);
    before.group = 1;
    before.open_file(dir.join("a.rs"));
    before.groups[0][1].buffer.as_mut().unwrap().set_caret(0, 4, false);
    before.groups[0][1].buffer.as_mut().unwrap().set_caret(1, 5, true);
    before.active[0] = Some(1);

    // Through the database and back, with one file deleted in between.
    let mut store = store::Store::open_in_memory().unwrap();
    let layout = store::Layout { panels: ["explorer".into(), String::new(), String::new()], last_panels: Default::default(), left_w: 260.0, right_w: 300.0, bottom_h: 240.0, active_group: 1, window: None };
    let session = store::Session { layout, tabs: before.tab_states() };
    store.save_session("p", "P", &session).unwrap();
    fs::remove_file(dir.join("gone.rs")).unwrap();
    let loaded = store.load_session("p").unwrap().unwrap();

    let mut after = Shell::new(dir.clone());
    after.restore_tabs(&loaded.tabs);
    let titles = |s: &Shell, g: usize| s.groups[g].iter().map(|t| t.title.clone()).collect::<Vec<_>>();
    // The pinned tab sits first, the missing file is skipped, the panel tab is back.
    assert_eq!(titles(&after, 0), ["b.rs", "a.rs", "Git"]);
    assert_eq!(titles(&after, 1), ["a.rs"]);
    assert!(after.groups[0][0].pinned && !after.groups[0][1].pinned);
    assert_eq!(after.active, [Some(1), Some(0)]);
    let buf = after.groups[0][1].buffer.as_ref().unwrap();
    assert_eq!((buf.anchor_pos(), buf.caret()), ((0, 4), (1, 5)));
    // The split pane has its own copy of the buffer, as before.
    assert_eq!(after.groups[1][0].buffer.as_ref().unwrap().caret(), (0, 0));
    // A caret saved past the end of a shortened file is pulled back inside it.
    let mut short = Shell::new(dir.clone());
    short.restore_tabs(&[store::TabState { group: 0, kind: store::TabKind::File, target: dir.join("b.rs").display().to_string(), pinned: false, active: true, anchor: (40, 9), caret: (40, 9) }]);
    assert_eq!(short.groups[0][0].buffer.as_ref().unwrap().caret(), (1, 0));
    let _ = fs::remove_dir_all(&dir);
}

fn drafts_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("forge_drafts_{}_{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn text_of(shell: &Shell, g: usize, i: usize) -> String {
    shell.groups[g][i].buffer.as_ref().unwrap().text()
}

#[test]
fn unsaved_edits_survive_a_restart_and_undo_returns_to_disk() {
    let dir = drafts_dir("restart");
    fs::write(dir.join("a.txt"), "hello\n").unwrap();
    fs::write(dir.join("b.txt"), "clean\n").unwrap();

    let mut before = Shell::with_store(dir.clone(), Some(store::Store::open_in_memory().unwrap()));
    before.open_file(dir.join("a.txt"));
    before.open_file(dir.join("b.txt"));
    before.groups[0][0].buffer.as_mut().unwrap().insert("X");
    before.persist_drafts();
    let project = path_key(&dir);
    let stored = before.store.as_ref().unwrap().load_drafts(&project).unwrap();
    assert_eq!(stored.len(), 1, "only the edited file has a draft");
    assert_eq!(stored[0].text, "Xhello\n");

    // More typing replaces the draft; nothing is rewritten when nothing changed.
    before.groups[0][0].buffer.as_mut().unwrap().insert("Y");
    before.persist_drafts();
    assert_eq!(before.store.as_ref().unwrap().load_drafts(&project).unwrap()[0].text, "XYhello\n");

    // A new run: same files, same database.
    let mut after = Shell::with_store(dir.clone(), before.store.take());
    after.open_file(dir.join("a.txt"));
    after.open_file(dir.join("b.txt"));
    after.restore_drafts();
    assert_eq!(text_of(&after, 0, 0), "XYhello\n");
    assert!(after.groups[0][0].buffer.as_ref().unwrap().is_dirty());
    assert!(!after.groups[0][1].buffer.as_ref().unwrap().is_dirty());
    assert!(after.notices.iter().any(|n| n.title == "Recovered unsaved changes"));
    assert!(!after.notices.iter().any(|n| n.title == "File changed on disk"));
    // Recovering again changes nothing in the database.
    after.persist_drafts();
    assert_eq!(after.store.as_ref().unwrap().load_drafts(&project).unwrap().len(), 1);
    // Undo goes back to the file as it is on disk, and that is clean.
    let buf = after.groups[0][0].buffer.as_mut().unwrap();
    buf.undo();
    assert_eq!(buf.text(), "hello\n");
    assert!(!buf.is_dirty());
    after.persist_drafts();
    assert!(after.store.as_ref().unwrap().load_drafts(&project).unwrap().is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn saving_or_closing_forgets_the_draft() {
    let dir = drafts_dir("forget");
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    fs::write(dir.join("b.txt"), "two\n").unwrap();
    let mut shell = Shell::with_store(dir.clone(), Some(store::Store::open_in_memory().unwrap()));
    shell.open_file(dir.join("a.txt"));
    shell.open_file(dir.join("b.txt"));
    for i in 0..2 {
        shell.groups[0][i].buffer.as_mut().unwrap().insert("!");
    }
    shell.persist_drafts();
    let count = |s: &Shell| s.store.as_ref().unwrap().load_drafts(&path_key(&s.root)).unwrap().len();
    assert_eq!(count(&shell), 2);
    // Saving a file makes it clean...
    assert_eq!(save_tab(&mut shell.groups[0][0]), Ok(true));
    shell.persist_drafts();
    assert_eq!(count(&shell), 1);
    assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "!one\n");
    // ...and closing a tab discards its edits.
    shell.close_tab(0, 1);
    shell.persist_drafts();
    assert_eq!(count(&shell), 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_file_changed_on_disk_still_gets_its_edits_back_with_a_warning() {
    let dir = drafts_dir("moved");
    fs::write(dir.join("a.txt"), "hello\n").unwrap();
    fs::write(dir.join("b.txt"), "bye\n").unwrap();
    let mut before = Shell::with_store(dir.clone(), Some(store::Store::open_in_memory().unwrap()));
    before.open_file(dir.join("a.txt"));
    before.open_file(dir.join("b.txt"));
    before.groups[0][0].buffer.as_mut().unwrap().insert("mine ");
    before.groups[0][1].buffer.as_mut().unwrap().insert("mine ");
    before.persist_drafts();
    // One file is rewritten by another program, the other is deleted.
    fs::write(dir.join("a.txt"), "hello, from another program\n").unwrap();
    fs::remove_file(dir.join("b.txt")).unwrap();

    let mut after = Shell::with_store(dir.clone(), before.store.take());
    after.restore_drafts();
    assert_eq!(after.groups[0].len(), 2, "the deleted file's tab is brought back too");
    let find = |name: &str| after.groups[0].iter().position(|t| t.title == name).unwrap();
    assert_eq!(text_of(&after, 0, find("a.txt")), "mine hello\n");
    assert_eq!(text_of(&after, 0, find("b.txt")), "mine bye\n");
    let warnings: Vec<_> = after.notices.iter().filter(|n| n.title == "File changed on disk").map(|n| n.message.clone()).collect();
    assert_eq!(warnings.len(), 2);
    assert!(warnings.iter().any(|w| w.starts_with("a.txt also changed")));
    assert!(warnings.iter().any(|w| w.starts_with("b.txt no longer exists")));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn search_rows_split_the_preview_around_the_match() {
    let dir = std::env::temp_dir().join(format!("forge_rows_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/a.rs"), "let value = 1;\nvalue\n").unwrap();
    let outcome = search::search(&dir, "value", true, &Default::default(), MAX_FILE_BYTES, &AtomicBool::new(false), 100);
    let rows = search_rows(&dir, &outcome.hits);
    assert_eq!(rows.len(), 3);
    assert!(rows[0].header && rows[0].before == "a.rs" && rows[0].sub.contains("(2)"));
    assert_eq!((rows[1].before.as_str(), rows[1].matched.as_str(), rows[1].after.as_str(), rows[1].sub.as_str()), ("let ", "value", " = 1;", "1"));
    assert_eq!((rows[2].before.as_str(), rows[2].matched.as_str(), rows[2].after.as_str()), ("", "value", ""));
    let state = SearchState { hits: outcome.hits, files: 1, ..Default::default() };
    assert_eq!(search_status(&state, "value"), "2 results in 1 file");
    assert_eq!(search_status(&SearchState::default(), ""), "Type to search the project");
    assert_eq!(search_status(&SearchState::default(), "x"), "No results");
    let _ = fs::remove_dir_all(&dir);
}

fn names(symbols: &[lsp_types::DocumentSymbol], out: &mut Vec<String>) {
    for s in symbols {
        out.push(s.name.clone());
        names(s.children.as_deref().unwrap_or_default(), out);
    }
}

/// Needs rust-analyzer on PATH: `cargo test -p forge_shell -- --ignored`.
/// Edits after an emoji on the same line, so a wrong UTF-16 column would
/// garble the server's copy and the new function would not show up.
#[test]
#[ignore]
fn edits_reach_rust_analyzer_incrementally() {
    let dir = std::env::temp_dir().join(format!("forge-sync-{}", std::process::id()));
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("Cargo.toml"), "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    let file = dir.join("src").join("main.rs");
    let text = "fn main() {\n    let s = \"héllo 😀\";\n}\n";
    fs::write(&file, text).unwrap();

    let mgr = Manager::new(vec![ServerSpec::rust_analyzer()], || {});
    let id = mgr.ensure_for_file(&file).unwrap();
    let mut buf = Buffer::new(text);
    buf.set_track_edits(true);
    let (mut doc, mut cursor) = (DocSync::new(&file, "rust"), 0);

    let deadline = Instant::now() + Duration::from_secs(60);
    while mgr.ready(id).is_none() {
        assert!(Instant::now() < deadline, "server never became ready: {:?}", mgr.servers());
        std::thread::sleep(Duration::from_millis(50));
    }
    doc.sync(&mgr, id, &mut BufSource { buf: &buf, cursor: &mut cursor });

    let uri = lsp_types::Uri::from_str(doc.uri()).unwrap();
    let symbols = |expect_inner: bool| {
        let (client, _) = mgr.ready(id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let params = lsp_types::DocumentSymbolParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            };
            let reply = mgr.handle().block_on(client.request::<lsp_types::request::DocumentSymbolRequest>(params));
            let mut found = Vec::new();
            match &reply {
                Ok(Some(DocumentSymbolResponse::Nested(s))) => names(s, &mut found),
                Ok(Some(DocumentSymbolResponse::Flat(s))) => found.extend(s.iter().map(|i| i.name.clone())),
                _ => {}
            }
            if found.iter().any(|n| n == "inner_fn") == expect_inner || Instant::now() > deadline {
                return (found, reply.is_ok());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    };

    // After the emoji: `    let s = "héllo 😀";` is 23 chars, 24 UTF-16 units.
    buf.set_caret(1, 23, false);
    buf.insert(" fn inner_fn() {}");
    doc.sync(&mgr, id, &mut BufSource { buf: &buf, cursor: &mut cursor });
    let (found, ok) = symbols(true);
    assert!(found.iter().any(|n| n == "inner_fn"), "symbols after edit: {found:?} (reply ok: {ok})\n{}", mgr.logs(id).join("\n"));

    buf.undo();
    doc.sync(&mgr, id, &mut BufSource { buf: &buf, cursor: &mut cursor });
    let (found, _) = symbols(false);
    assert!(!found.iter().any(|n| n == "inner_fn"), "symbols after undo: {found:?}");
    assert!(mgr.logs(id).iter().filter(|l| l.contains("didChange")).count() >= 2);

    doc.close(&mgr);
    mgr.shutdown();
}
