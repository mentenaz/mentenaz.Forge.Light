use super::*;

pub(crate) fn wire_panels(ui: &AppWindow, shell: &Rc<RefCell<Shell>>) {
    on!(ui, shell, on_toggle_panel, |ui, s, id| {
        if let Some(&(_, _, _, dock)) = s.panel(&id) {
            if dock_panel(&ui, dock) == id {
                set_dock_panel(&ui, dock, SharedString::new());
            } else {
                set_dock_panel(&ui, dock, id.clone());
            }
            s.last_panel[dock as usize] = id.to_string();
        }
    });
    on!(ui, shell, on_toggle_dock, |ui, s, dock| {
        if dock_panel(&ui, dock).is_empty() {
            set_dock_panel(&ui, dock, s.last_panel[dock as usize].as_str().into());
        } else {
            set_dock_panel(&ui, dock, SharedString::new());
        }
    });
    on!(ui, shell, on_open_panel_tab, |_ui, s, id| s.open_panel_tab(&id));
    on!(ui, shell, on_filter_projects, |_ui, s, text| s.recent_filter = text.to_string());
    on!(ui, shell, on_pick_project, |ui, s, path| {
        let path = PathBuf::from(path.as_str());
        if path.is_dir() {
            if path_key(&path) != path_key(&s.root) {
                // Each project keeps its own tabs: save this one's, then open the other's.
                persist(&ui, &mut s);
                s.close_all_tabs();
                s.set_root(path);
                s.last_saved = None;
                if let Some(saved) = s.saved_session() {
                    s.restore_tabs(&saved.tabs);
                    apply_layout(&ui, &mut s, &saved.layout);
                }
                s.restore_drafts();
                persist(&ui, &mut s);
            }
        } else {
            s.notify(2, "Project not found", format!("{} doesn't exist on this machine.", path.display()));
        }
    });
    on!(ui, shell, on_pick_branch, |_ui, s, name| {
        s.notify(0, "Branch switching isn't wired yet", format!("Would switch to {name}; git operations arrive with the Git panel."));
    });
    on!(ui, shell, on_mark_notices_read, |_ui, s| s.notices.iter_mut().for_each(|n| n.read = true));
    on!(ui, shell, on_clear_notices, |_ui, s| s.notices.clear());
    ui.on_minimize({
        let weak = ui.as_weak();
        move || weak.unwrap().window().set_minimized(true)
    });
    ui.on_toggle_maximize({
        let weak = ui.as_weak();
        move || {
            let window = weak.unwrap();
            let window = window.window();
            window.set_maximized(!window.is_maximized());
        }
    });
    // Mouse deltas arrive in logical pixels; the window position and size are physical.
    ui.on_drag_window({
        let weak = ui.as_weak();
        move |dx, dy| {
            let window = weak.unwrap();
            let window = window.window();
            if window.is_maximized() {
                return;
            }
            let scale = window.scale_factor();
            let p = window.position();
            window.set_position(slint::PhysicalPosition::new(p.x + (dx * scale) as i32, p.y + (dy * scale) as i32));
        }
    });
    ui.on_resize_window({
        let weak = ui.as_weak();
        move |dw, dh| {
            let window = weak.unwrap();
            let window = window.window();
            let scale = window.scale_factor();
            let size = window.size();
            window.set_size(slint::PhysicalSize::new(
                (size.width as f32 + dw * scale).max(760.0 * scale) as u32,
                (size.height as f32 + dh * scale).max(480.0 * scale) as u32,
            ));
        }
    });
    ui.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
}

pub(crate) fn wire_search(ui: &AppWindow, shell: &Rc<RefCell<Shell>>) {
    // Ctrl+Shift+F: show the Search panel, seeded from a one-line selection.
    {
        let weak = ui.as_weak();
        let sh = Rc::clone(shell);
        ui.on_open_search(move || {
            let ui = weak.unwrap();
            let seed = {
                let s = sh.borrow();
                let tab = s.active[s.group].and_then(|a| s.groups[s.group].get(a));
                tab.and_then(|t| t.buffer.as_ref()).map(Buffer::selected_text).filter(|t| !t.is_empty() && !t.contains('\n') && t.chars().count() <= 80)
            };
            {
                let mut s = sh.borrow_mut();
                s.last_panel[0] = "search".into();
                set_dock_panel(&ui, 0, "search".into());
                if let Some(seed) = seed {
                    ui.set_search_query(seed.into());
                    s.start_search(ui.as_weak(), ui.get_search_query().to_string(), ui.get_search_case());
                }
            }
            sync(&ui, &sh.borrow());
            // The panel may have just been created, so focus it a moment later.
            let weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(60), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_search_focus(ui.get_search_focus() + 1);
                }
            });
        });
    }
    {
        // Typing searches after a short pause; Enter searches at once.
        let timer = Rc::new(slint::Timer::default());
        let run = {
            let sh = Rc::clone(shell);
            move |ui: &AppWindow| {
                sh.borrow_mut().start_search(ui.as_weak(), ui.get_search_query().to_string(), ui.get_search_case());
                sync_search(ui, &sh.borrow());
            }
        };
        {
            let (weak, timer, run) = (ui.as_weak(), Rc::clone(&timer), run.clone());
            ui.on_search_edited(move || {
                let (weak, run) = (weak.clone(), run.clone());
                timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_millis(200), move || {
                    if let Some(ui) = weak.upgrade() {
                        run(&ui);
                    }
                });
            });
        }
        {
            let (weak, timer, run) = (ui.as_weak(), Rc::clone(&timer), run.clone());
            ui.on_search_accepted(move || {
                timer.stop();
                run(&weak.unwrap());
            });
        }
    }
    {
        let (weak, sh) = (ui.as_weak(), Rc::clone(shell));
        ui.on_search_ready(move || {
            if sh.borrow_mut().take_search_results() {
                sync_search(&weak.unwrap(), &sh.borrow());
            }
        });
    }
    on!(ui, shell, on_search_clicked, |_ui, s, i| {
        if let Some(h) = s.search.hits.get(i as usize).cloned() {
            s.goto_match(h.path, h.line, h.col, h.len);
        }
    });
    on!(ui, shell, on_file_clicked, |_ui, s, i| {
        let Some((path, row)) = s.file_rows().into_iter().nth(i as usize) else { return };
        if row.is_dir {
            if !s.expanded.remove(&path) {
                s.expanded.insert(path);
            }
        } else {
            s.open_file(path);
        }
    });
}

pub(crate) fn wire_tabs(ui: &AppWindow, shell: &Rc<RefCell<Shell>>) {
    on!(ui, shell, on_tab_activate, |_ui, s, g, i| {
        let g = g as usize;
        s.group = g;
        s.active[g] = Some(i as usize);
    });
    on!(ui, shell, on_tab_close, |_ui, s, g, i| s.close_tab(g as usize, i as usize));
    on!(ui, shell, on_tab_close_others, |_ui, s, g, i| s.close_others(g as usize, i as usize));
    on!(ui, shell, on_tab_toggle_pin, |_ui, s, g, i| s.toggle_pin(g as usize, i as usize));
    on!(ui, shell, on_tab_split, |_ui, s, g, i| s.split_tab(g as usize, i as usize));
    on!(ui, shell, on_split_active_tab, |_ui, s| {
        if let Some(i) = s.active[0] {
            s.split_tab(0, i);
        }
    });
    on!(ui, shell, on_swap_panes, |_ui, s| s.swap_panes());
    on!(ui, shell, on_focus_group, |_ui, s, g| {
        if g == 0 || !s.groups[1].is_empty() {
            s.group = g as usize;
        }
    });
    on!(ui, shell, on_cycle_tab, |_ui, s, dir| s.cycle(dir));
    on!(ui, shell, on_close_active_tab, |_ui, s| {
        let g = s.group;
        if let Some(i) = s.active[g] {
            s.close_tab(g, i);
        }
    });

}

pub(crate) fn wire_editor(ui: &AppWindow, shell: &Rc<RefCell<Shell>>) {
    // ---- editor ---------------------------------------------------------
    // Editor callbacks only refresh the tabs and editors, not the file tree.
    macro_rules! on_editor {
        ($reg:ident, |$s:ident, $b:ident $(, $a:ident)*| $body:expr) => {{
            let weak = ui.as_weak();
            let sh = Rc::clone(&shell);
            ui.$reg(move |g $(, $a)*| {
                let ui = weak.unwrap();
                {
                    let mut $s = sh.borrow_mut();
                    let g = g as usize;
                    #[allow(unused_variables)]
                    if let Some($b) = $s.active[g].and_then(|a| $s.groups[g].get_mut(a)) {
                        $body;
                    }
                }
                sync_tabs(&ui, &sh.borrow());
            });
        }};
    }
    on_editor!(on_ed_viewport, |s, tab, first, count| tab.view = (first.max(0) as usize, count.max(1) as usize));
    {
        let weak = ui.as_weak();
        let sh = Rc::clone(shell);
        ui.on_ed_text(move |g, text| {
            let ui = weak.unwrap();
            {
                let mut s = sh.borrow_mut();
                let g = g as usize;
                let mut typed = None;
                if let Some(tab) = s.active[g].and_then(|a| s.groups[g].get_mut(a)) {
                    if let Some(b) = tab.buffer.as_mut() {
                        let mut chars = text.chars();
                        match (chars.next(), chars.next()) {
                            (None, _) => {}
                            // One typed character may close a bracket or skip over one; IME and paste insert as-is.
                            (Some(c), None) => {
                                b.type_char(c);
                                typed = Some(c);
                            }
                            _ => b.insert(&text),
                        }
                    }
                    tab.refresh_matches();
                }
                s.close_actions(g);
                s.completion_after_edit(g, typed);
                s.signature_after_edit(g, typed);
            }
            sync_tabs(&ui, &sh.borrow());
        });
    }
    // Double and triple clicks select a word and a line.
    let clicks = std::cell::Cell::new((std::time::Instant::now(), -1i32, -1i32, 0u8));
    on_editor!(on_ed_pressed, |s, tab, line, col, shift| {
        let now = std::time::Instant::now();
        let (at, last_line, last_col, n) = clicks.get();
        let n = if !shift && now.duration_since(at).as_millis() < 450 && last_line == line && (last_col - col).abs() <= 1 { n % 3 + 1 } else { 1 };
        clicks.set((now, line, col, n));
        if let Some(b) = tab.buffer.as_mut() {
            let l = (line.max(0) as usize).min(b.len_lines() - 1);
            let c = char_col(&b.line(l), col.max(0) as usize);
            match n {
                2 => b.select_word(l, c),
                3 => b.select_line(l),
                _ => b.set_caret(l, c, shift),
            }
        }
    });
    on_editor!(on_ed_dragged, |s, tab, line, col| {
        if let Some(b) = tab.buffer.as_mut() {
            let l = (line.max(0) as usize).min(b.len_lines() - 1);
            let c = char_col(&b.line(l), col.max(0) as usize);
            b.set_caret(l, c, true);
        }
    });
    {
        let weak = ui.as_weak();
        let sh = Rc::clone(shell);
        ui.on_ed_command(move |g, name, shift| {
            let ui = weak.unwrap();
            let mut full = false;
            {
                let mut s = sh.borrow_mut();
                let g = g as usize;
                s.group = g;
                let format_on_save = s.format_on_save;
                let (mut want_save, mut format_document, mut format_then_save) = (false, false, false);
                let mut save_error = None;
                let mut saved = false;
                let mut goto_definition = None;
                let mut find_references = None;
                let mut completion_command: Option<String> = None;
                if let Some(tab) = s.active[g].and_then(|a| s.groups[g].get_mut(a)) {
                    let page = tab.view.1.saturating_sub(2).max(1) as isize;
                    if let Some(b) = tab.buffer.as_mut() {
                        match name.as_str() {
                            "left" => b.left(shift, false),
                            "right" => b.right(shift, false),
                            "word-left" => b.left(shift, true),
                            "word-right" => b.right(shift, true),
                            "up" => b.vertical(-1, shift),
                            "down" => b.vertical(1, shift),
                            "page-up" => b.vertical(-page, shift),
                            "page-down" => b.vertical(page, shift),
                            "home" => b.home(shift),
                            "end" => b.end(shift),
                            "doc-start" => b.doc_start(shift),
                            "doc-end" => b.doc_end(shift),
                            "backspace" => b.backspace(),
                            "delete" => b.delete(),
                            "newline" => b.newline(),
                            "tab" => b.tab(),
                            "backtab" => b.outdent(),
                            "move-up" => b.move_lines(-1),
                            "move-down" => b.move_lines(1),
                            "toggle-comment" => {
                                if let Some(prefix) = tab.syntax.map(|s| s.line_comment) {
                                    b.toggle_comment(prefix);
                                }
                            }
                            "goto-definition" => goto_definition = Some(b.caret()),
                            "find-references" => find_references = Some(b.caret()),
                            "completion-up" | "completion-down" | "completion-accept" | "completion-close" | "completion-trigger" | "signature-close" | "code-actions" | "actions-up" | "actions-down" | "actions-accept" | "actions-close" => completion_command = Some(name.to_string()),
                            "next-problem" => jump_problem(b, &tab.diag.borrow(), 1),
                            "prev-problem" => jump_problem(b, &tab.diag.borrow(), -1),
                            "undo" => b.undo(),
                            "redo" => b.redo(),
                            "select-all" => b.select_all(),
                            "save" => want_save = true,
                            "format-document" => format_document = true,
                            _ => {}
                        }
                    }
                    tab.refresh_matches();
                    if want_save {
                        if format_on_save && tab.lsp.borrow().is_some() {
                            format_then_save = true;
                        } else {
                            match save_tab(tab) {
                                Ok(true) => saved = true,
                                Ok(false) => {}
                                Err(m) => save_error = Some(m),
                            }
                        }
                    }
                    if saved {
                        tab.lsp_sync(true);
                    }
                }
                if format_then_save && !s.request_format(g, true) {
                    s.save_active(g);
                }
                if format_document && !s.request_format(g, false) {
                    s.notify(2, "Can't format", "There is no language server for this file.".into());
                }
                match completion_command {
                    Some(command) => s.completion_command(g, &command),
                    None => {
                        s.close_actions(g);
                        s.completion_after_edit(g, None);
                        s.signature_after_edit(g, None);
                    }
                }
                if let Some((line, col)) = goto_definition {
                    s.request_definition(g, line, col);
                }
                if let Some((line, col)) = find_references {
                    s.request_references(g, line, col);
                }
                if let Some(message) = save_error {
                    s.notify(3, "Couldn't save", message);
                    full = true;
                }
            }
            if full { sync(&ui, &sh.borrow()) } else { sync_tabs(&ui, &sh.borrow()) }
        });
    }
    {
        let sh = Rc::clone(shell);
        ui.on_ed_hover(move |g, line, col| sh.borrow_mut().request_hover(g as usize, line.max(0) as usize, col.max(0) as usize));
        let sh = Rc::clone(shell);
        ui.on_ed_definition(move |g, line, col| sh.borrow_mut().request_definition(g as usize, line.max(0) as usize, col.max(0) as usize));
        let sh = Rc::clone(shell);
        ui.on_ed_idle(move |g, _line, _col| sh.borrow_mut().request_code_actions(g as usize, true));
        let sh = Rc::clone(shell);
        ui.on_ed_rename(move |g, name| sh.borrow_mut().request_rename(g as usize, name.to_string()));
    }
    // Find / replace.
    {
        let weak = ui.as_weak();
        let sh = Rc::clone(shell);
        ui.on_ed_find(move |g, kind, text, flag| {
            let ui = weak.unwrap();
            {
                let mut s = sh.borrow_mut();
                let g = g as usize;
                s.group = g;
                if let Some(tab) = s.active[g].and_then(|a| s.groups[g].get_mut(a)) {
                    find(tab, kind.as_str(), &text, flag);
                }
            }
            sync_tabs(&ui, &sh.borrow());
        });
    }
    {
        let sh = Rc::clone(shell);
        ui.on_ed_seed(move |g| {
            let s = sh.borrow();
            let g = g as usize;
            s.active[g]
                .and_then(|a| s.groups[g].get(a))
                .and_then(|t| t.buffer.as_ref())
                .map(|b| {
                    let selected = b.selected_text();
                    if selected.is_empty() {
                        let (line, col) = b.caret();
                        word_at(&b.line(line), col)
                    } else {
                        selected
                    }
                })
                .filter(|t| !t.contains('\n') && t.chars().count() <= 80)
                .map(SharedString::from)
                .unwrap_or_default()
        });
    }
    {
        let sh = Rc::clone(shell);
        ui.on_ed_copy(move |g| {
            let s = sh.borrow();
            let g = g as usize;
            s.active[g]
                .and_then(|a| s.groups[g].get(a))
                .and_then(|t| t.buffer.as_ref())
                .map(|b| SharedString::from(b.selected_text()))
                .unwrap_or_default()
        });
    }
}

pub(crate) fn wire_lsp(ui: &AppWindow, shell: &Rc<RefCell<Shell>>) -> Arc<Manager> {
    // ---- language servers -----------------------------------------------
    // The manager runs on its own threads; it only wakes the UI thread, which
    // then reads the state it needs.
    let wake = Arc::new(AtomicBool::new(false));
    let weak = std::sync::Mutex::new(ui.as_weak());
    let manager = {
        let wake = wake.clone();
        Arc::new(Manager::new(vec![ServerSpec::rust_analyzer()], move || {
            if !wake.swap(true, Ordering::SeqCst) {
                let wake = wake.clone();
                let _ = weak.lock().unwrap().upgrade_in_event_loop(move |ui| {
                    wake.store(false, Ordering::SeqCst);
                    ui.invoke_lsp_changed();
                });
            }
        }))
    };
    {
        let mut s = shell.borrow_mut();
        s.lsp = Some(manager.clone());
        for tab in s.groups.iter().flatten() {
            s.attach_lsp(tab);
        }
    }
    ui.set_format_on_save(shell.borrow().format_on_save);
    on!(ui, shell, on_toggle_format_on_save, |ui, s| {
        let on = !s.format_on_save;
        s.set_format_on_save(on);
        ui.set_format_on_save(s.format_on_save);
    });
    on!(ui, shell, on_outline_clicked, |_ui, s, i| {
        if let (Some(sym), Some(path)) = (s.outline.get(i as usize).cloned(), s.outline_path.clone()) {
            s.goto(path, sym.line, sym.col);
        }
    });
    on!(ui, shell, on_reference_clicked, |_ui, s, i| {
        if let Some(r) = s.references.get(i as usize).cloned() {
            s.goto(r.path, r.line, r.col);
        }
    });
    on!(ui, shell, on_problem_clicked, |_ui, s, i| {
        if let Some((path, d)) = s.problem_items().get(i as usize).cloned() {
            s.goto(path, d.start.0, d.start.1);
        }
    });
    on!(ui, shell, on_lsp_action, |_ui, s, id, action| {
        let Some(mgr) = s.lsp.clone() else { return };
        let id = id as ServerId;
        match action.as_str() {
            "logs" => {
                s.open_log_tab(id);
                s.refresh_log_tabs();
            }
            "restart" => mgr.restart(id),
            "stop" => mgr.stop(id),
            "restart-all" => mgr.restart_all(),
            "stop-all" => mgr.stop_all(),
            _ => {}
        }
    });
    {
        let sh = Rc::clone(shell);
        let mgr = manager.clone();
        // `FORGE_LSP_TRACE=1` echoes server logs to stderr.
        let trace = std::env::var_os("FORGE_LSP_TRACE").is_some();
        let printed = RefCell::new(std::collections::HashMap::<ServerId, usize>::new());
        let weak = ui.as_weak();
        let last_diag_gen = Cell::new(0u64);
        ui.on_lsp_changed(move || {
            let ui = weak.unwrap();
            let logs_changed = sh.borrow_mut().refresh_log_tabs();
            let generation = mgr.diag_gen();
            let diags_changed = generation != last_diag_gen.replace(generation);
            let answered = sh.borrow_mut().apply_replies();
            if std::mem::take(&mut sh.borrow_mut().show_references) {
                set_dock_panel(&ui, 2, "references".into());
                sh.borrow_mut().last_panel[2] = "references".into();
            }
            if answered {
                sync(&ui, &sh.borrow());
            } else if diags_changed {
                sync_problems(&ui, &sh.borrow());
            }
            if logs_changed || diags_changed {
                sync_tabs(&ui, &sh.borrow());
            } else {
                sh.borrow().lsp_sync_open();
            }
            sync_lsp(&ui, &sh.borrow());
            if trace {
                for info in mgr.servers() {
                    let mut printed = printed.borrow_mut();
                    let seen = printed.entry(info.id).or_default();
                    let logs = mgr.logs(info.id);
                    for line in logs.iter().skip(*seen) {
                        eprintln!("[lsp {} {:?}] {line}", info.name, info.status);
                    }
                    *seen = logs.len();
                }
            }
        });
    }
    manager
}

pub(crate) fn wire_extensions(ui: &AppWindow, shell: &Rc<RefCell<Shell>>) {
    {
        let mut s = shell.borrow_mut();
        s.reload_extensions();
        if let Some((key, palette)) = s.saved_theme() {
            themes::apply(ui, &palette);
            s.theme_key = Some(key);
        }
        sync_extensions(ui, &s);
    }
    on!(ui, shell, on_ext_refresh, |ui, s| {
        s.ext_refresh(ui.as_weak(), false);
        sync_extensions(&ui, &s);
    });
    on!(ui, shell, on_ext_search, |ui, s, q, all| {
        s.ext_search(ui.as_weak(), q.to_string(), all);
        sync_extensions(&ui, &s);
    });
    on!(ui, shell, on_ext_install, |ui, s, id| {
        s.ext_install(ui.as_weak(), id.to_string());
        sync_extensions(&ui, &s);
    });
    on!(ui, shell, on_theme_picked, |ui, s, i| {
        if i < 0 {
            ui.global::<Theme>().set_custom(false);
            s.set_theme_key(None);
        } else if let Some((key, palette)) = s.theme_palette(i as usize) {
            themes::apply(&ui, &palette);
            s.set_theme_key(Some(key));
        }
        sync_extensions(&ui, &s);
    });
    let (weak, sh) = (ui.as_weak(), Rc::clone(shell));
    ui.on_ext_changed(move || {
        let Some(ui) = weak.upgrade() else { return };
        let changed = sh.borrow_mut().apply_ext_replies();
        if changed {
            sync_extensions(&ui, &sh.borrow());
        }
    });
}
