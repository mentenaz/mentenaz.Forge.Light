//! Keeps one document in step with a language server: `didOpen` when the
//! server (re)starts, incremental `didChange`, `didSave`, `didClose`.

use crate::client::path_to_uri;
use crate::manager::{Manager, ServerId, normalize_path};
use serde_json::{Value, json};
use std::path::Path;

/// A replaced range in LSP terms (line, UTF-16 column) and its new text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentChange {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub text: String,
}

/// Where a document's text comes from.
pub trait DocSource {
    /// The whole text. Must also mark everything up to now as consumed, so
    /// the next `take_changes` only reports later edits.
    fn full_text(&mut self) -> String;
    /// Edits since the last `full_text`/`take_changes`, oldest first, each
    /// relative to the text after the previous one. `None` if unknown.
    fn take_changes(&mut self) -> Option<Vec<ContentChange>>;
}

pub struct DocSync {
    uri: String,
    language_id: String,
    /// Server instance (id, start generation) that has this document open.
    opened_in: Option<(ServerId, u64)>,
    version: i64,
}

impl DocSync {
    pub fn new(path: &Path, language_id: &str) -> Self {
        DocSync {
            uri: path_to_uri(&normalize_path(path)),
            language_id: language_id.to_string(),
            opened_in: None,
            version: 0,
        }
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn version(&self) -> i64 {
        self.version
    }

    /// Brings the server up to date. Does nothing until the server is ready;
    /// call it again after edits and whenever the server's status changes.
    pub fn sync(&mut self, mgr: &Manager, id: ServerId, source: &mut impl DocSource) {
        let Some((client, generation)) = mgr.ready(id) else { return };
        if self.opened_in != Some((id, generation)) {
            let text = source.full_text();
            self.version = 1;
            let _ = client.notify_raw(
                "textDocument/didOpen",
                json!({"textDocument": {
                    "uri": self.uri, "languageId": self.language_id,
                    "version": self.version, "text": text
                }}),
            );
            self.opened_in = Some((id, generation));
            mgr.log(id, format!("[sync] didOpen {} ({} bytes)", self.uri, text.len()));
            return;
        }
        let (changes, summary): (Vec<Value>, String) = match source.take_changes() {
            Some(c) if c.is_empty() => return,
            Some(c) => {
                let summary = format!("{} change(s)", c.len());
                (c.iter().map(change_json).collect(), summary)
            }
            None => {
                let text = source.full_text();
                let summary = format!("full resync ({} bytes)", text.len());
                (vec![json!({"text": text})], summary)
            }
        };
        self.version += 1;
        let _ = client.notify_raw(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": self.uri, "version": self.version},
                "contentChanges": changes
            }),
        );
        mgr.log(id, format!("[sync] didChange v{} {summary}", self.version));
    }

    /// Tells the server the file was saved. Call `sync` first.
    pub fn save(&self, mgr: &Manager, id: ServerId) {
        let Some((client, generation)) = mgr.ready(id) else { return };
        if self.opened_in == Some((id, generation)) {
            let _ = client.notify_raw(
                "textDocument/didSave",
                json!({"textDocument": {"uri": self.uri}}),
            );
            mgr.log(id, format!("[sync] didSave {}", self.uri));
        }
    }

    pub fn close(&mut self, mgr: &Manager) {
        let Some((id, generation)) = self.opened_in.take() else { return };
        if let Some((client, now)) = mgr.ready(id)
            && now == generation
        {
            let _ = client.notify_raw(
                "textDocument/didClose",
                json!({"textDocument": {"uri": self.uri}}),
            );
            mgr.log(id, format!("[sync] didClose {}", self.uri));
        }
    }
}

fn change_json(c: &ContentChange) -> Value {
    json!({
        "range": {
            "start": {"line": c.start.0, "character": c.start.1},
            "end": {"line": c.end.0, "character": c.end.1}
        },
        "text": c.text
    })
}
