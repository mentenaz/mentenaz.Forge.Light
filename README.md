# Forge Light

A light version of the Mentenaz Forge editor, written in Rust with [Slint](https://slint.dev).
It is a small code editor with a docked shell (explorer, search, problems, outline), a
tree-sitter highlighted editor and a built-in language-server client. Extensions are
installed from the Forge registry.

Licensed under the **GPL-3.0-or-later**, see [LICENSE](LICENSE) and [NOTICE](NOTICE).

> Status: early. It runs and its tests pass, but expect rough edges.

## Building and running

You need a recent stable Rust toolchain (the workspace uses edition 2024).

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
| `crates/forge_shell` | The application: Slint UI (`ui/`) and the shell state. `main.rs` sets up and calls `wire.rs`; `shell`, `tab`, `tabs`, `lsp`, `session` and `view` hold the rest. |
| `crates/editor_core` | Text buffer with undo, tree-sitter syntax and highlighting. No UI dependency. |
| `crates/lsp_client` | JSON-RPC language-server client and manager. No UI dependency. |
| `crates/forge_extensions` | Registry client, archive installer and `extension.toml` parser. No UI dependency. |
| `crates/light_cpu_app` | A separate small app. |

## Extensions

`forge_extensions` talks to the Forge registry (`https://forge.mentenaz-server.com/api/registry`),
which mirrors the shape of Zed's extension API. It lists extensions, downloads their `.tar.gz`
archives and unpacks them into `installed/<id>/`. Archives are treated as untrusted: only
regular files and directories are unpacked and nothing may land outside the extension's folder.

What an installed extension can do inside Forge is being added step by step: themes first,
then languages (configs, queries, grammars), then language servers.

Tests that use the real registry are ignored by default:

```
cargo test -p forge_extensions -- --ignored
```

## Relationship to Zed

The extension format and registry API follow [Zed](https://github.com/zed-industries/zed), and some
code is adapted from it. See [NOTICE](NOTICE) for what is derived and under which license.
