use super::*;

pub(crate) struct Notices {
    pub(crate) kind: i32,
    pub(crate) title: String,
    pub(crate) message: String,
    pub(crate) time: String,
    pub(crate) read: bool,
}

/// The project search panel: the latest hits and the search that is producing them.
#[derive(Default)]
pub(crate) struct SearchState {
    pub(crate) hits: Vec<search::Hit>,
    pub(crate) files: usize,
    pub(crate) truncated: bool,
    pub(crate) searching: bool,
    /// Only the newest search's answer is kept.
    pub(crate) seq: u64,
    pub(crate) cancel: Option<Arc<AtomicBool>>,
    pub(crate) inbox: Arc<Mutex<Option<(u64, search::Outcome)>>>,
}

/// Most hits a search collects before it stops.
pub(crate) const SEARCH_CAP: usize = 2000;

pub(crate) struct Shell {
    pub(crate) root: PathBuf,
    pub(crate) panels: Vec<(&'static str, &'static str, &'static str, i32)>,
    /// The panel each dock showed last, restored by the dock toggles.
    pub(crate) last_panel: [String; 3],
    pub(crate) groups: [Vec<Tab>; 2],
    pub(crate) active: [Option<usize>; 2],
    pub(crate) group: usize,
    pub(crate) expanded: HashSet<PathBuf>,
    pub(crate) recent_all: Vec<String>,
    pub(crate) recent_filter: String,
    pub(crate) notices: Vec<Notices>,
    pub(crate) highlighter: RefCell<Highlighter>,
    pub(crate) lsp: Option<Arc<Manager>>,
    pub(crate) replies: Arc<Mutex<VecDeque<Reply>>>,
    /// Only the newest hover / definition request is answered.
    pub(crate) hover_seq: u64,
    pub(crate) definition_seq: u64,
    pub(crate) references_seq: u64,
    pub(crate) outline_seq: u64,
    /// The outline of one file: its symbols, the file, and the (tab key, buffer revision) they were asked for.
    pub(crate) outline: Vec<lsp_client::Symbol>,
    pub(crate) outline_path: Option<PathBuf>,
    pub(crate) outline_for: Option<(String, u64)>,
    pub(crate) rename_seq: u64,
    pub(crate) completion_seq: u64,
    pub(crate) signature_seq: u64,
    pub(crate) format_seq: u64,
    pub(crate) actions_seq: u64,
    pub(crate) bulb_seq: u64,
    pub(crate) format_on_save: bool,
    pub(crate) search: SearchState,
    /// Settings and sessions; `None` if the database couldn't be opened.
    pub(crate) store: Option<store::Store>,
    /// The session as last written, so unchanged state isn't written again.
    pub(crate) last_saved: Option<store::Session>,
    /// Files that have a draft in the database right now.
    pub(crate) drafted: HashSet<String>,
    pub(crate) references: Vec<RefItem>,
    /// Set when a References answer arrived, so the panel gets opened.
    pub(crate) show_references: bool,
}

impl Shell {
    /// A shell with no database: nothing is saved or restored (what the tests use).
    #[cfg(test)]
    pub(crate) fn new(root: PathBuf) -> Self {
        Self::with_store(root, None)
    }

    pub(crate) fn with_store(root: PathBuf, store: Option<store::Store>) -> Self {
        // id, title, icon, default dock (0 left, 1 right, 2 bottom): the
        // Zed edition's panels.
        let panels = vec![
            ("explorer", "Explorer", "explorer", 0),
            ("search", "Search", "search", 0),
            ("cockpit", "Cockpit", "cockpit", 0),
            ("helm", "Helm", "helm", 0),
            ("python", "Python", "python", 0),
            ("node", "Node", "node", 0),
            ("dotnet", ".NET", "dotnet", 0),
            ("flows", "Flows", "flows", 0),
            ("rust", "Rust", "rust", 0),
            ("demo", "Demo", "demo", 0),
            ("git", "Git", "git", 1),
            ("outline", "Outline", "outline", 1),
            ("problems", "Problems", "error", 2),
            ("references", "References", "search", 2),
            ("terminal", "Terminal", "terminal", 2),
            ("debugger", "Debugger", "debugger", 2),
            ("database", "Database", "database", 2),
            ("script-runner", "Script Runner", "script-runner", 2),
            ("processes", "Processes", "processes", 2),
        ];
        // Projects opened before, or stand-in entries until the database has any.
        let mut recent_all: Vec<String> = match &store {
            Some(store) => store.recent_projects(10),
            None => [r"E:\mentenaz\Forge.Super", r"E:\zed_Source", r"E:\mentenaz\gpui", r"E:\Forge_GPUI"].iter().map(|s| (*s).to_string()).collect(),
        };
        let here = root.display().to_string();
        recent_all.retain(|p| *p != here);
        recent_all.insert(0, here);

        let notice = |kind, title: &str, message: &str, time: &str, read| Notices {
            kind,
            title: title.into(),
            message: message.into(),
            time: time.into(),
            read,
        };
        Self {
            expanded: HashSet::from([root.clone()]),
            root,
            panels,
            last_panel: ["explorer".into(), "git".into(), "terminal".into()],
            groups: [Vec::new(), Vec::new()],
            active: [None, None],
            group: 0,
            recent_filter: String::new(),
            highlighter: RefCell::new(Highlighter::new()),
            lsp: None,
            replies: Arc::default(),
            hover_seq: 0,
            definition_seq: 0,
            references_seq: 0,
            outline_seq: 0,
            outline: Vec::new(),
            outline_path: None,
            outline_for: None,
            rename_seq: 0,
            completion_seq: 0,
            signature_seq: 0,
            format_seq: 0,
            actions_seq: 0,
            bulb_seq: 0,
            format_on_save: store.as_ref().and_then(|s| s.setting("format_on_save")).is_some_and(|v| v == "true"),
            search: SearchState::default(),
            store,
            last_saved: None,
            drafted: HashSet::new(),
            references: Vec::new(),
            show_references: false,
            recent_all,
            notices: vec![
                notice(3, "dotnet build failed", "Api.csproj: 1 error, see the Script Runner output.", "Just now", false),
                notice(2, "npm audit", "3 vulnerable packages found in Backend.", "2 min ago", false),
                notice(1, "Script finished", "`npm run build` completed in 14.2s.", "8 min ago", false),
                notice(0, "Update available", "rust-analyzer 2026-10-20 is available.", "1 hour ago", true),
            ],
        }
    }

    pub(crate) fn panel(&self, id: &str) -> Option<&(&'static str, &'static str, &'static str, i32)> {
        self.panels.iter().find(|p| p.0 == id)
    }

    pub(crate) fn notify(&mut self, kind: i32, title: &str, message: String) {
        self.notices.insert(
            0,
            Notices { kind, title: title.into(), message, time: "Just now".into(), read: false },
        );
    }

    /// Starts a search for `query` in the background, replacing any still running.
    /// Open files are searched as edited. `weak` is told when the answer is ready.
    pub(crate) fn start_search(&mut self, weak: slint::Weak<AppWindow>, query: String, case: bool) {
        if let Some(cancel) = self.search.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.search.seq += 1;
        if query.is_empty() {
            self.search.hits.clear();
            self.search.files = 0;
            self.search.truncated = false;
            self.search.searching = false;
            return;
        }
        let open: std::collections::HashMap<String, String> = self
            .groups
            .iter()
            .flatten()
            .filter_map(|t| Some((path_key(t.path.as_ref()?), t.buffer.as_ref()?.text())))
            .collect();
        let cancel = Arc::new(AtomicBool::new(false));
        self.search.cancel = Some(cancel.clone());
        self.search.searching = true;
        let (seq, root, inbox) = (self.search.seq, self.root.clone(), self.search.inbox.clone());
        std::thread::spawn(move || {
            let outcome = search::search(&root, &query, case, &open, MAX_FILE_BYTES, &cancel, SEARCH_CAP);
            if outcome.cancelled {
                return;
            }
            *inbox.lock().unwrap() = Some((seq, outcome));
            let _ = weak.upgrade_in_event_loop(|ui| ui.invoke_search_ready());
        });
    }

    /// Takes a finished search's hits if they belong to the newest search.
    pub(crate) fn take_search_results(&mut self) -> bool {
        let Some((seq, outcome)) = self.search.inbox.lock().unwrap().take() else { return false };
        if seq != self.search.seq {
            return false;
        }
        self.search.hits = outcome.hits;
        self.search.files = outcome.files;
        self.search.truncated = outcome.truncated;
        self.search.searching = false;
        true
    }
}
