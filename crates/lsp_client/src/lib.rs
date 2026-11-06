//! Lightweight LSP client: JSON-RPC over stdio, no UI dependency.

mod client;
mod codeaction;
mod completion;
mod diag;
mod docs;
mod manager;
mod requests;
mod signature;
mod symbols;
mod transport;

pub use client::{Client, Error, Event, path_to_uri};
pub use completion::{CompletionItem, CompletionList, Trigger, completion, filter_and_rank, fuzzy_score, parse_completion, snippet_to_text};
pub use codeaction::{CodeAction, code_actions, parse_code_actions};
pub use symbols::{Symbol, document_symbols, parse_document_symbols};
pub use signature::{SignatureHelp, parse_signature_help, signature_help};
pub use requests::{
    FileEdit, HoverInfo, Target, apply_changes, definition, hover, parse_hover, parse_locations, parse_workspace_edit, plain_text,
    format, references, rename,
};
pub use diag::{Diag, ERROR, FileDiags, HINT, INFO, WARNING, char_to_utf16_col, path_key, uri_key, uri_to_path, utf16_to_char_col};
pub use docs::{ContentChange, DocSource, DocSync};
pub use lsp_types;
pub use manager::{
    Manager, ServerId, ServerInfo, ServerSpec, Status, find_root, normalize_path, resolve_command,
};
