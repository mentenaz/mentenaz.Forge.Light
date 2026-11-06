//! The shell's small database: user settings and, per project, the session
//! (which tabs were open, where the carets were, how the docks were laid out).
//! Nothing here knows about the UI; the shell hands in plain data and gets it back.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

/// Bumped whenever the tables change; `migrate` brings older files up to it.
const SCHEMA_VERSION: i32 = 2;

#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    /// The file was written by a newer version, so it is left alone.
    TooNew(i32),
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "{e}"),
            Self::TooNew(v) => write!(f, "database is from a newer version (schema {v})"),
        }
    }
}

/// The main window's last place and size, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Dock contents and sizes. A panel id of "" means that dock was closed.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub panels: [String; 3],
    /// The panel each dock showed last, which its toggle brings back.
    pub last_panels: [String; 3],
    pub left_w: f32,
    pub right_w: f32,
    pub bottom_h: f32,
    pub active_group: u8,
    pub window: Option<WindowRect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    /// A file; `target` is its path.
    File,
    /// A panel shown as a tab; `target` is the panel id.
    Panel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabState {
    pub group: u8,
    pub kind: TabKind,
    pub target: String,
    pub pinned: bool,
    /// The tab showing in its pane.
    pub active: bool,
    /// (line, char column) of the selection's anchor and head.
    pub anchor: (usize, usize),
    pub caret: (usize, usize),
}

/// Everything restored when a project is reopened. Tabs are in pane order.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub layout: Layout,
    pub tabs: Vec<TabState>,
}

/// The unsaved text of a file, kept so a crash or a close doesn't lose it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub path: String,
    /// The edited text, with LF line endings.
    pub text: String,
    /// The file on disk when the draft's text was started from: its length and
    /// modified time (ms). If the file differs from this now, it changed behind the draft's back.
    pub base: Option<(u64, i64)>,
}

pub struct Store {
    conn: Connection,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl Store {
    /// Opens (creating if needed) the database at `path`. A file that isn't a
    /// usable database is moved aside and replaced, so a damaged file never
    /// stops the app from starting. A file from a newer version is an error and is not touched.
    pub fn open(path: &Path) -> Result<Self, Error> {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        match Self::init(Connection::open(path)?) {
            Ok(store) => Ok(store),
            Err(Error::TooNew(v)) => Err(Error::TooNew(v)),
            Err(first) => {
                let aside = path.with_extension(format!("db.damaged-{}", now()));
                if fs::rename(path, &aside).is_err() {
                    return Err(first);
                }
                for suffix in ["-wal", "-shm"] {
                    let mut side = path.as_os_str().to_owned();
                    side.push(suffix);
                    let _ = fs::remove_file(side);
                }
                Self::init(Connection::open(path)?)
            }
        }
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, Error> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, Error> {
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        // `journal_mode` answers with a row, so it is read rather than executed.
        let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), Error> {
        let version: i32 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(Error::TooNew(version));
        }
        if version < 1 {
            self.conn.execute_batch(
                "BEGIN;
                 CREATE TABLE settings(
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 ) STRICT;
                 CREATE TABLE projects(
                     id INTEGER PRIMARY KEY,
                     key TEXT NOT NULL UNIQUE,
                     path TEXT NOT NULL,
                     last_opened INTEGER NOT NULL,
                     left_panel TEXT NOT NULL,
                     right_panel TEXT NOT NULL,
                     bottom_panel TEXT NOT NULL,
                     last_left TEXT NOT NULL,
                     last_right TEXT NOT NULL,
                     last_bottom TEXT NOT NULL,
                     left_w REAL NOT NULL,
                     right_w REAL NOT NULL,
                     bottom_h REAL NOT NULL,
                     active_group INTEGER NOT NULL,
                     win_x INTEGER,
                     win_y INTEGER,
                     win_w INTEGER,
                     win_h INTEGER
                 ) STRICT;
                 CREATE TABLE tabs(
                     project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                     grp INTEGER NOT NULL,
                     position INTEGER NOT NULL,
                     kind TEXT NOT NULL,
                     target TEXT NOT NULL,
                     pinned INTEGER NOT NULL,
                     active INTEGER NOT NULL,
                     anchor_line INTEGER NOT NULL,
                     anchor_col INTEGER NOT NULL,
                     caret_line INTEGER NOT NULL,
                     caret_col INTEGER NOT NULL,
                     PRIMARY KEY(project_id, grp, position)
                 ) STRICT;
                 PRAGMA user_version = 1;
                 COMMIT;",
            )?;
        }
        if version < 2 {
            self.conn.execute_batch(
                "BEGIN;
                 CREATE TABLE drafts(
                     project_key TEXT NOT NULL,
                     path TEXT NOT NULL,
                     text TEXT NOT NULL,
                     base_len INTEGER,
                     base_mtime INTEGER,
                     saved_at INTEGER NOT NULL,
                     PRIMARY KEY(project_key, path)
                 ) STRICT;
                 PRAGMA user_version = 2;
                 COMMIT;",
            )?;
        }
        Ok(())
    }

    // ---- settings ----------------------------------------------------------

    pub fn setting(&self, key: &str) -> Option<String> {
        self.conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), Error> {
        self.conn.execute("INSERT INTO settings(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [key, value])?;
        Ok(())
    }

    // ---- sessions ----------------------------------------------------------

    /// Replaces the saved session of the project `key` (a normalised path), shown as `path`.
    pub fn save_session(&mut self, key: &str, path: &str, session: &Session) -> Result<(), Error> {
        let l = &session.layout;
        let w = l.window;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO projects(key, path, last_opened, left_panel, right_panel, bottom_panel, last_left, last_right, last_bottom,
                                  left_w, right_w, bottom_h, active_group, win_x, win_y, win_w, win_h)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(key) DO UPDATE SET path = excluded.path, last_opened = excluded.last_opened,
                 left_panel = excluded.left_panel, right_panel = excluded.right_panel, bottom_panel = excluded.bottom_panel,
                 last_left = excluded.last_left, last_right = excluded.last_right, last_bottom = excluded.last_bottom,
                 left_w = excluded.left_w, right_w = excluded.right_w, bottom_h = excluded.bottom_h,
                 active_group = excluded.active_group, win_x = excluded.win_x, win_y = excluded.win_y,
                 win_w = excluded.win_w, win_h = excluded.win_h",
            params![
                key, path, now(),
                l.panels[0], l.panels[1], l.panels[2],
                l.last_panels[0], l.last_panels[1], l.last_panels[2],
                l.left_w, l.right_w, l.bottom_h, l.active_group,
                w.map(|w| w.x), w.map(|w| w.y), w.map(|w| w.w), w.map(|w| w.h),
            ],
        )?;
        let id: i64 = tx.query_row("SELECT id FROM projects WHERE key = ?1", [key], |r| r.get(0))?;
        tx.execute("DELETE FROM tabs WHERE project_id = ?1", [id])?;
        let mut position = [0i64; 2];
        for t in &session.tabs {
            let g = (t.group as usize).min(1);
            tx.execute(
                "INSERT INTO tabs(project_id, grp, position, kind, target, pinned, active, anchor_line, anchor_col, caret_line, caret_col)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    id, g as i64, position[g],
                    match t.kind { TabKind::File => "file", TabKind::Panel => "panel" },
                    t.target, t.pinned, t.active,
                    t.anchor.0 as i64, t.anchor.1 as i64, t.caret.0 as i64, t.caret.1 as i64,
                ],
            )?;
            position[g] += 1;
        }
        tx.commit()?;
        Ok(())
    }

    /// The saved session of the project `key`, if there is one.
    pub fn load_session(&self, key: &str) -> Result<Option<Session>, Error> {
        let row = self
            .conn
            .query_row(
                "SELECT id, left_panel, right_panel, bottom_panel, last_left, last_right, last_bottom,
                        left_w, right_w, bottom_h, active_group, win_x, win_y, win_w, win_h
                 FROM projects WHERE key = ?1",
                [key],
                |r| {
                    let window = match (r.get::<_, Option<i32>>(11)?, r.get::<_, Option<i32>>(12)?, r.get::<_, Option<u32>>(13)?, r.get::<_, Option<u32>>(14)?) {
                        (Some(x), Some(y), Some(w), Some(h)) => Some(WindowRect { x, y, w, h }),
                        _ => None,
                    };
                    Ok((
                        r.get::<_, i64>(0)?,
                        Layout {
                            panels: [r.get(1)?, r.get(2)?, r.get(3)?],
                            last_panels: [r.get(4)?, r.get(5)?, r.get(6)?],
                            left_w: r.get(7)?,
                            right_w: r.get(8)?,
                            bottom_h: r.get(9)?,
                            active_group: r.get(10)?,
                            window,
                        },
                    ))
                },
            )
            .optional()?;
        let Some((id, layout)) = row else { return Ok(None) };
        let mut stmt = self.conn.prepare(
            "SELECT grp, kind, target, pinned, active, anchor_line, anchor_col, caret_line, caret_col
             FROM tabs WHERE project_id = ?1 ORDER BY grp, position",
        )?;
        let tabs = stmt
            .query_map([id], |r| {
                Ok(TabState {
                    group: r.get(0)?,
                    kind: if r.get::<_, String>(1)? == "panel" { TabKind::Panel } else { TabKind::File },
                    target: r.get(2)?,
                    pinned: r.get(3)?,
                    active: r.get(4)?,
                    anchor: (r.get::<_, i64>(5)? as usize, r.get::<_, i64>(6)? as usize),
                    caret: (r.get::<_, i64>(7)? as usize, r.get::<_, i64>(8)? as usize),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Session { layout, tabs }))
    }

    // ---- drafts ------------------------------------------------------------

    /// Writes `put` and forgets the drafts of the files in `remove`, in one step.
    pub fn apply_drafts(&mut self, project: &str, put: &[Draft], remove: &[String]) -> Result<(), Error> {
        let tx = self.conn.transaction()?;
        for d in put {
            tx.execute(
                "INSERT INTO drafts(project_key, path, text, base_len, base_mtime, saved_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(project_key, path) DO UPDATE SET text = excluded.text, base_len = excluded.base_len,
                     base_mtime = excluded.base_mtime, saved_at = excluded.saved_at",
                params![project, d.path, d.text, d.base.map(|b| b.0 as i64), d.base.map(|b| b.1), now()],
            )?;
        }
        for path in remove {
            tx.execute("DELETE FROM drafts WHERE project_key = ?1 AND path = ?2", [project, path.as_str()])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_drafts(&self, project: &str) -> Result<Vec<Draft>, Error> {
        let mut stmt = self.conn.prepare("SELECT path, text, base_len, base_mtime FROM drafts WHERE project_key = ?1 ORDER BY path")?;
        let drafts = stmt
            .query_map([project], |r| {
                let base = match (r.get::<_, Option<i64>>(2)?, r.get::<_, Option<i64>>(3)?) {
                    (Some(len), Some(mtime)) => Some((len as u64, mtime)),
                    _ => None,
                };
                Ok(Draft { path: r.get(0)?, text: r.get(1)?, base })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(drafts)
    }

    /// Project paths, most recently opened first.
    pub fn recent_projects(&self, limit: usize) -> Vec<String> {
        let Ok(mut stmt) = self.conn.prepare("SELECT path FROM projects ORDER BY last_opened DESC, id DESC LIMIT ?1") else { return Vec::new() };
        stmt.query_map([limit as i64], |r| r.get(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }
}

/// Where the database lives: `FORGE_DB` if set (for tests), else under `%APPDATA%`.
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("FORGE_DB").map(PathBuf::from).or_else(|| std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("ForgeSlint").join("forge.db")))
}

/// Opens the default database and imports the old `settings.txt` once.
/// `None` (with a note on stderr) if it can't be used; the app then runs without saving.
pub fn open_default() -> Option<Store> {
    let path = default_path()?;
    let store = match Store::open(&path) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("[store] {}: {e}; running without saved settings", path.display());
            return None;
        }
    };
    if store.setting("format_on_save").is_none() {
        let legacy = path.parent().map(|d| d.join("settings.txt"));
        if let Some(text) = legacy.and_then(|p| fs::read_to_string(p).ok()) {
            let _ = store.set_setting("format_on_save", if text.lines().any(|l| l.trim() == "format_on_save=true") { "true" } else { "false" });
        }
    }
    Some(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout {
            panels: ["explorer".into(), String::new(), "problems".into()],
            last_panels: ["explorer".into(), "git".into(), "problems".into()],
            left_w: 280.0,
            right_w: 300.0,
            bottom_h: 220.0,
            active_group: 1,
            window: Some(WindowRect { x: -20, y: 40, w: 1400, h: 900 }),
        }
    }

    fn tab(group: u8, target: &str, active: bool) -> TabState {
        TabState { group, kind: TabKind::File, target: target.into(), pinned: false, active, anchor: (3, 4), caret: (5, 6) }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forge_store_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_session_round_trips() {
        let mut store = Store::open_in_memory().unwrap();
        let session = Session {
            layout: layout(),
            tabs: vec![
                tab(0, r"E:\p\a.rs", false),
                TabState { pinned: true, ..tab(0, r"E:\p\b.rs", true) },
                TabState { kind: TabKind::Panel, target: "git".into(), ..tab(1, "", true) },
            ],
        };
        store.save_session("e:/p", r"E:\p", &session).unwrap();
        assert_eq!(store.load_session("e:/p").unwrap(), Some(session));
        assert_eq!(store.load_session("e:/other").unwrap(), None);
    }

    #[test]
    fn saving_again_replaces_the_tabs_and_keeps_projects_apart() {
        let mut store = Store::open_in_memory().unwrap();
        let three = Session { layout: layout(), tabs: vec![tab(0, "a", true), tab(0, "b", false), tab(0, "c", false)] };
        store.save_session("p1", "P1", &three).unwrap();
        store.save_session("p2", "P2", &Session { layout: layout(), tabs: vec![tab(0, "z", true)] }).unwrap();
        let one = Session { layout: Layout { window: None, ..layout() }, tabs: vec![tab(0, "b", true)] };
        store.save_session("p1", "P1", &one).unwrap();
        assert_eq!(store.load_session("p1").unwrap(), Some(one));
        assert_eq!(store.load_session("p2").unwrap().unwrap().tabs.len(), 1);
        // An empty session is a session too: the project was open with nothing in it.
        store.save_session("p3", "P3", &Session { layout: layout(), tabs: vec![] }).unwrap();
        assert!(store.load_session("p3").unwrap().unwrap().tabs.is_empty());
    }

    #[test]
    fn settings_and_recent_projects() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.setting("format_on_save"), None);
        store.set_setting("format_on_save", "true").unwrap();
        store.set_setting("format_on_save", "false").unwrap();
        assert_eq!(store.setting("format_on_save").as_deref(), Some("false"));

        let session = Session { layout: layout(), tabs: vec![] };
        for (i, name) in ["one", "two", "three"].iter().enumerate() {
            store.save_session(name, name, &session).unwrap();
            // Same-second saves are ordered by when they happened.
            store.conn.execute("UPDATE projects SET last_opened = ?1 WHERE key = ?2", params![100 + i as i64, name]).unwrap();
        }
        assert_eq!(store.recent_projects(2), ["three", "two"]);
        store.save_session("one", "one", &session).unwrap();
        store.conn.execute("UPDATE projects SET last_opened = 500 WHERE key = 'one'", []).unwrap();
        assert_eq!(store.recent_projects(10), ["one", "three", "two"]);
    }

    #[test]
    fn the_file_persists_across_opens() {
        let dir = scratch("persist");
        let path = dir.join("forge.db");
        let session = Session { layout: layout(), tabs: vec![tab(0, "a", true)] };
        Store::open(&path).unwrap().save_session("p", "P", &session).unwrap();
        Store::open(&path).unwrap().set_setting("k", "v").unwrap();
        let again = Store::open(&path).unwrap();
        assert_eq!(again.load_session("p").unwrap(), Some(session));
        assert_eq!(again.setting("k").as_deref(), Some("v"));
        drop(again);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_file_is_set_aside_and_replaced() {
        let dir = scratch("damaged");
        let path = dir.join("forge.db");
        fs::write(&path, b"this is not a database, just text that is long enough to look like one".repeat(100)).unwrap();
        let mut store = Store::open(&path).expect("starts fresh instead of failing");
        store.save_session("p", "P", &Session { layout: layout(), tabs: vec![] }).unwrap();
        drop(store);
        let aside: Vec<_> = fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("damaged")).collect();
        assert_eq!(aside.len(), 1, "the old file is kept, not deleted");
        let _ = fs::remove_dir_all(&dir);
    }

    fn draft(path: &str, text: &str) -> Draft {
        Draft { path: path.into(), text: text.into(), base: Some((12, 1_700_000_000_123)) }
    }

    #[test]
    fn drafts_are_written_replaced_and_removed() {
        let mut store = Store::open_in_memory().unwrap();
        store.apply_drafts("p1", &[draft("a", "one"), draft("b", "two")], &[]).unwrap();
        store.apply_drafts("p2", &[draft("a", "other project")], &[]).unwrap();
        // Writing the same file again replaces its text; a draft with no known base keeps `None`.
        store.apply_drafts("p1", &[draft("a", "one, edited"), Draft { base: None, ..draft("c", "three") }], &["b".to_string()]).unwrap();
        let got = store.load_drafts("p1").unwrap();
        assert_eq!(got, [draft("a", "one, edited"), Draft { base: None, ..draft("c", "three") }]);
        assert_eq!(store.load_drafts("p2").unwrap(), [draft("a", "other project")]);
        assert!(store.load_drafts("nobody").unwrap().is_empty());
        // Text survives exactly: tabs, blank lines, non-ASCII.
        let odd = "\tx\n\n  ünï 😀\n";
        store.apply_drafts("p1", &[draft("odd", odd)], &[]).unwrap();
        assert_eq!(store.load_drafts("p1").unwrap().iter().find(|d| d.path == "odd").unwrap().text, odd);
    }

    #[test]
    fn a_version_1_file_gains_the_drafts_table_and_keeps_its_data() {
        let dir = scratch("v1");
        let path = dir.join("forge.db");
        {
            // What the first release wrote.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
                 INSERT INTO settings VALUES('format_on_save', 'true');
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.setting("format_on_save").as_deref(), Some("true"));
        store.apply_drafts("p", &[draft("a", "x")], &[]).unwrap();
        assert_eq!(store.load_drafts("p").unwrap().len(), 1);
        drop(store);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newer_database_is_left_alone() {
        let dir = scratch("newer");
        let path = dir.join("forge.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA user_version = 99; CREATE TABLE future(x);").unwrap();
        }
        assert!(matches!(Store::open(&path), Err(Error::TooNew(99))));
        assert!(path.exists());
        assert_eq!(fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("damaged")).count(), 0);
        let _ = fs::remove_dir_all(&dir);
    }
}
