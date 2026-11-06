# Forge Light: notes for Claude and contributors

A Slint/Rust code editor. See `README.md` for what it is and `NOTICE` for what is derived from Zed
(GPL-3.0-or-later; the license is declared once in `[workspace.package]`).

## Commands

```
cargo run                          # forge_shell, the default member
cargo test -p forge_shell          # run tests per package: see "Gotchas"
cargo test -p forge_extensions -- --ignored   # live tests against the Forge registry
cargo clippy --workspace --all-targets        # keep it warning-free
```

## Crates

- `forge_shell`: the app. `main.rs` builds the window and calls `wire_*` functions in `wire.rs`;
  state lives in `shell.rs` (`Shell`), with `tab(s).rs`, `lsp.rs`, `session.rs`, `view.rs`,
  `extensions.rs`, `themes.rs`. The Slint UI is in `ui/`.
- `editor_core`: rope buffer, undo, language registry (`languages.rs`), tree-sitter highlighting
  (`syntax.rs`) with built-in grammars and wasm grammars from extensions. No UI dependency.
- `lsp_client`: JSON-RPC language-server client and manager. No UI dependency.
- `forge_extensions`: registry client, safe installer, `extension.toml`, theme and language readers.
  Blocking (`ureq`); callers use worker threads. No UI dependency.

## Conventions

- `#![deny(unsafe_code)]` in `forge_shell`. Items shared between its modules are `pub(crate)`.
- UI callbacks go through the `on!` macro (borrow state mutably, then re-sync the window). Worker
  threads never touch the UI: they push a reply onto a queue and wake the UI thread with
  `weak.upgrade_in_event_loop(|ui| ui.invoke_...())`.
- Archives and extension files are untrusted: paths come from `Manifest::resolve`, ids from
  `valid_id`, sizes are capped.
- A maximized window must not save its (screen-sized) rectangle; see `window_to_save`.
- Match the surrounding comment density and style. Tests sit next to the code; shell tests are in
  `crates/forge_shell/src/tests.rs`.

## Gotchas

- On Windows, `cargo test --workspace` can fail with "Permission denied" on one `forge_extensions`
  test executable; per-package runs work. rust-analyzer also locks files in `target/`.
- The first build after enabling tree-sitter's `wasm` feature compiles wasmtime (about 5 minutes).
- Slint `out` properties cannot be set from Rust, so themes use `in property c-*` plus `Theme.custom`.
- Wasm grammars run on the UI thread while parsing and have no time limit yet.

## Where things stand

Done: extension registry client and installer, Extensions panel, themes, wasm language grammars,
outline panel, window-state fix. Next, in order: parse timeout for wasm grammars; more syntax
categories than the current 10 `Kind`s; language servers from extensions (start with native
binaries from a registry, wasm hosting later).
