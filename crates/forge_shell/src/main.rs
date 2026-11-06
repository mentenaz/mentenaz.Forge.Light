//! Forge Super shell in Slint: title bar, three docks, two tab panes, status bar.
//!
//! The Slint side (`ui/`) owns layout and look; this file owns the state the
//! gpui shell kept in `ForgeShell`: panels, tabs, the split pane, the file
//! tree and the notification list. Language servers, diagnostics and the
//! notices are stand-in data, as in the gpui shell's `dummy.rs`.

#![deny(unsafe_code)]

/// Registers a callback that updates the shared state, then re-syncs the window.
macro_rules! on {
    ($ui:expr, $sh:expr, $reg:ident, |$u:ident, $s:ident $(, $a:ident)*| $body:expr) => {{
        let weak = $ui.as_weak();
        let sh = Rc::clone(&$sh);
        $ui.$reg(move |$($a),*| {
            let $u = weak.unwrap();
            {
                #[allow(unused_mut)]
                let mut $s = sh.borrow_mut();
                $body;
            }
            sync(&$u, &sh.borrow());
        });
    }};
}

mod lsp;
mod search;
mod session;
mod shell;
mod store;
mod tab;
mod tabs;
#[cfg(test)]
mod tests;
mod view;
mod wire;

use lsp::*;
use session::*;
use shell::*;
use tab::*;
use view::*;
use wire::*;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use editor_core::columns::{char_col, expand_tabs_at, visual_col};
use editor_core::{Buffer, Highlighter, Language, Match, Parsed, Span, Syntax};
use lsp_client::{ContentChange, Diag, DocSource, DocSync, Manager, ServerId, ServerSpec, Status, char_to_utf16_col, normalize_path, path_key, utf16_to_char_col};
use slint::{ModelRc, SharedString, VecModel};

slint::include_modules!();

const SKIPPED_DIRS: [&str; 3] = [".git", "target", "node_modules"];

const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

fn main() -> Result<(), slint::PlatformError> {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let shell = Rc::new(RefCell::new(Shell::with_store(root, store::open_default())));
    // Reopen what this project had open last time.
    let saved = shell.borrow().saved_session();
    if let Some(saved) = &saved {
        shell.borrow_mut().restore_tabs(&saved.tabs);
    }
    shell.borrow_mut().restore_drafts();
    // `FORGE_OPEN=<file>` opens a file at start-up (handy for testing).
    if let Some(file) = std::env::var_os("FORGE_OPEN") {
        shell.borrow_mut().open_file(PathBuf::from(file));
    }

    let ui = AppWindow::new()?;
    {
        let s = shell.borrow();
        let infos = |dock: Option<i32>| -> ModelRc<PanelInfo> {
            model(
                s.panels
                    .iter()
                    .filter(|p| dock.is_none_or(|d| p.3 == d))
                    .map(|p| PanelInfo { id: p.0.into(), title: p.1.into(), icon: p.2.into(), dock: p.3 })
                    .collect(),
            )
        };
        ui.set_panels(infos(None));
        ui.set_left_panels(infos(Some(0)));
        ui.set_right_panels(infos(Some(1)));
        ui.set_bottom_panels(infos(Some(2)));
        ui.set_left_panel("explorer".into());
    }
    if let Some(saved) = &saved {
        apply_layout(&ui, &mut shell.borrow_mut(), &saved.layout);
    }

    wire_panels(&ui, &shell);
    wire_search(&ui, &shell);
    wire_tabs(&ui, &shell);
    wire_editor(&ui, &shell);
    let manager = wire_lsp(&ui, &shell);

    // `FORGE_SEARCH=<query>` opens the Search panel with that query (handy for testing).
    if let Some(query) = std::env::var("FORGE_SEARCH").ok().filter(|q| !q.is_empty()) {
        ui.set_search_query(query.clone().into());
        set_dock_panel(&ui, 0, "search".into());
        shell.borrow_mut().start_search(ui.as_weak(), query, ui.get_search_case());
    }
    sync(&ui, &shell.borrow());
    // Save the session a moment after it changes, and once more on the way out.
    let autosave = slint::Timer::default();
    {
        let (weak, sh) = (ui.as_weak(), Rc::clone(&shell));
        autosave.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(2), move || {
            if let (Some(ui), Ok(mut s)) = (weak.upgrade(), sh.try_borrow_mut()) {
                persist(&ui, &mut s);
            }
        });
    }
    // Ask for the outline of the file in view while the Outline panel is open.
    let outline_poll = slint::Timer::default();
    {
        let (weak, sh) = (ui.as_weak(), Rc::clone(&shell));
        outline_poll.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(400), move || {
            let (Some(ui), Ok(mut s)) = (weak.upgrade(), sh.try_borrow_mut()) else { return };
            if (0..3).any(|dock| dock_panel(&ui, dock) == "outline") {
                let g = s.group;
                s.refresh_outline(g);
            }
        });
    }
    let result = ui.run();
    persist(&ui, &mut shell.borrow_mut());
    drop(autosave);
    drop(outline_poll);
    // Stop the servers before the process exits so none are left behind.
    manager.shutdown();
    result
}
