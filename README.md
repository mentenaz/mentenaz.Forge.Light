# Forge Light

A light version of the Mentenaz Forge editor, written in Rust with [Slint](https://slint.dev).
It is a small code editor with a docked shell (explorer, search, problems, outline,
extensions), a tree-sitter highlighted editor and a built-in language-server client.
Extensions (themes and languages so far) are installed from the Forge registry.

Licensed under the **GPL-3.0-or-later**, see [LICENSE](LICENSE) and [NOTICE](NOTICE).

> Status: early. It runs and its tests pass, but expect rough edges.

## Building and running

You need a recent stable Rust toolchain (the workspace uses edition 2024). The first build
compiles wasmtime, which runs the grammars of language extensions, and takes several minutes;
later builds reuse it.

```
cargo run          # starts forge_shell, the default member
cargo test         # unit and integration tests for every crate
```

Language-server features need the server on your `PATH`. Today only `rust-analyzer` is
configured, so completion, hover, go to definition, references, rename, formatting, code
actions, diagnostics and the Outline panel work for `.rs` files.

### Environment variables

| Variable | Effect |
|---|---|
| `FORGE_DB` | Path of the settings and session database (default `%APPDATA%\ForgeSlint\forge.db`) |
| `FORGE_EXTENSIONS` | Directory extensions install into (default `%APPDATA%\ForgeSlint\extensions`) |
| `FORGE_OPEN=<file>` | Open a file at start-up |
| `FORGE_SEARCH=<query>` | Open the Search panel with a query at start-up |
| `FORGE_LSP_TRACE=1` | Echo language-server logs to stderr |

## Layout

| Crate | Purpose |
|---|---|
| `crates/forge_shell` | The application: Slint UI (`ui/`) and the shell state. `main.rs` sets up and calls `wire.rs`; `shell`, `tab`, `tabs`, `lsp`, `session`, `view`, `extensions` and `themes` hold the rest. |
| `crates/editor_core` | Text buffer with undo, the language registry, and tree-sitter highlighting (built-in grammars and wasm grammars from extensions). No UI dependency. |
| `crates/lsp_client` | JSON-RPC language-server client and manager. No UI dependency. |
| `crates/forge_extensions` | Registry client, archive installer, `extension.toml` parser, theme-file reader and installed-language reader. No UI dependency. |
| `crates/light_cpu_app` | A separate small app. |

## Extensions

`forge_extensions` talks to the Forge registry (`https://forge.mentenaz-server.com/api/registry`),
which mirrors the shape of Zed's extension API. It lists extensions, downloads their `.tar.gz`
archives and unpacks them into `installed/<id>/`. Archives are treated as untrusted: only
regular files and directories are unpacked and nothing may land outside the extension's folder.

### Using extensions

Open the **Extensions** panel from the left dock. It lists registry extensions (themes by
default; the button next to the search box cycles Themes, Languages and All), and installs
them in the background. Installed themes appear under "Installed themes": click one to apply it. The choice
is remembered, and "Forge Default" (or the sun/moon button) returns to the built-in colors.

Themes use Zed's theme format. Colors a theme does not define fall back to the built-in ones.

### Languages

A language extension adds syntax highlighting for the file types it claims. It ships a
tree-sitter grammar compiled to wasm, which Forge runs with wasmtime, and a `highlights.scm`
query. A file is matched by extension or whole file name; the longest match wins, and a
built-in language wins an equal claim, so an extension cannot take over `.rs` files. The grammar
loads in the background when the first matching file is open, so the file is shown uncolored
for a moment. A grammar or query that does not load is reported in the notifications and the
file stays plain. Highlighting colors captures in 10 categories, so a theme's finer syntax
colors are not all used yet.

Language servers from extensions (which are wasm components too) are the next step.

Tests that use the real registry are ignored by default:

```
cargo test -p forge_extensions -- --ignored
```

## Window state

The window's size, position and whether it was maximized are saved with the session. A maximized
window saves its last normal size rather than the screen-sized one, so it no longer reopens
larger than the screen.

## Relationship to Zed

The extension format and registry API follow [Zed](https://github.com/zed-industries/zed), and some
code is adapted from it. See [NOTICE](NOTICE) for what is derived and under which license.
