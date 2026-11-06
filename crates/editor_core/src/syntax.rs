//! Tree-sitter highlighting. A `Parsed` document keeps its syntax tree between
//! edits: the buffer records every change as a `TextEdit`, the tree is updated
//! from those and re-parsed incrementally, and highlighting runs the grammar's
//! query over just the lines asked for.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

use streaming_iterator::StreamingIterator;
use tree_sitter::wasmtime::Engine;
use tree_sitter::{InputEdit, Node, Parser, Point, Query, QueryCursor, Tree, WasmStore};

use crate::languages::{GrammarSource, LanguageDef};
use crate::{Buffer, Kind, Span, TextEdit};

/// Cap on a grammar's `.wasm` and on its highlight query, against absurd files.
const MAX_WASM_BYTES: u64 = 64 * 1024 * 1024;
const MAX_QUERY_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Language {
    Rust,
    Toml,
    Json,
    Python,
    JavaScript,
    Bash,
    CSharp,
    Css,
    Html,
}

impl Language {
    /// The grammar and its highlight query.
    fn grammar(self) -> (tree_sitter::Language, &'static str) {
        match self {
            Self::Rust => (tree_sitter_rust::LANGUAGE.into(), tree_sitter_rust::HIGHLIGHTS_QUERY),
            Self::Toml => (tree_sitter_toml_ng::LANGUAGE.into(), tree_sitter_toml_ng::HIGHLIGHTS_QUERY),
            Self::Json => (tree_sitter_json::LANGUAGE.into(), tree_sitter_json::HIGHLIGHTS_QUERY),
            Self::Python => (tree_sitter_python::LANGUAGE.into(), tree_sitter_python::HIGHLIGHTS_QUERY),
            Self::JavaScript => (tree_sitter_javascript::LANGUAGE.into(), tree_sitter_javascript::HIGHLIGHT_QUERY),
            Self::Bash => (tree_sitter_bash::LANGUAGE.into(), tree_sitter_bash::HIGHLIGHT_QUERY),
            Self::CSharp => (tree_sitter_c_sharp::LANGUAGE.into(), tree_sitter_c_sharp::HIGHLIGHTS_QUERY),
            Self::Css => (tree_sitter_css::LANGUAGE.into(), tree_sitter_css::HIGHLIGHTS_QUERY),
            Self::Html => (tree_sitter_html::LANGUAGE.into(), tree_sitter_html::HIGHLIGHTS_QUERY),
        }
    }
}

fn kind_of(name: &str) -> Kind {
    let head = name.split('.').next().unwrap_or(name);
    match head {
        "keyword" | "label" | "conditional" | "repeat" | "include" | "exception" => Kind::Keyword,
        "string" | "escape" | "character" => Kind::String,
        "comment" => Kind::Comment,
        "number" | "boolean" | "float" => Kind::Number,
        "type" | "constructor" | "module" | "namespace" => Kind::Type,
        "function" | "method" => Kind::Function,
        "property" | "attribute" | "tag" | "field" => Kind::Property,
        "constant" => Kind::Constant,
        "operator" => Kind::Operator,
        "punctuation" | "delimiter" => Kind::Punctuation,
        _ => Kind::Plain,
    }
}

struct Grammar {
    language: tree_sitter::Language,
    query: Query,
    /// The `Kind` of each capture in the query, by capture index.
    kinds: Vec<Kind>,
    /// Wasm grammars run inside a store that a parser has to hold while it parses.
    /// It is kept here between parses, so documents of one language share it.
    store: Option<RefCell<Option<WasmStore>>>,
}

impl Grammar {
    /// Parses `buf` (incrementally, given `old`) with `parser`.
    fn parse(&self, parser: &mut Parser, buf: &Buffer, old: Option<&Tree>) -> Option<Tree> {
        let Some(cell) = &self.store else { return parse(parser, buf, old) };
        let store = cell.borrow_mut().take()?;
        parser.set_wasm_store(store).ok()?;
        let tree = parser.set_language(&self.language).ok().and_then(|()| parse(parser, buf, old));
        // The parser deletes a store it still holds, so it always goes back.
        *cell.borrow_mut() = parser.take_wasm_store();
        tree
    }
}

/// A grammar loaded and ready to be installed in a [`Highlighter`]. Loading a wasm
/// grammar takes tens to hundreds of milliseconds, so it can be done on another thread.
pub struct LoadedGrammar {
    language: tree_sitter::Language,
    query: Query,
    kinds: Vec<Kind>,
    store: Option<WasmStore>,
}

fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(Engine::default)
}

fn read_capped(path: &std::path::Path, max: u64) -> Result<Vec<u8>, String> {
    let len = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if len > max {
        return Err(format!("{} is larger than {} MiB", path.display(), max / (1024 * 1024)));
    }
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn compile(language: tree_sitter::Language, source: &str, store: Option<WasmStore>) -> Result<LoadedGrammar, String> {
    let query = Query::new(&language, source).map_err(|e| format!("highlight query: {e}"))?;
    let kinds = query.capture_names().iter().map(|n| kind_of(n)).collect();
    Ok(LoadedGrammar { language, query, kinds, store })
}

/// Loads a language's grammar and highlight query. Blocking; for a wasm grammar call it off the UI thread.
pub fn load_grammar(def: &LanguageDef) -> Result<LoadedGrammar, String> {
    match &def.source {
        GrammarSource::BuiltIn(lang) => {
            let (language, source) = lang.grammar();
            compile(language, source, None)
        }
        GrammarSource::Wasm { grammar, wasm, highlights, .. } => {
            let bytes = read_capped(wasm, MAX_WASM_BYTES)?;
            let highlights = highlights.as_deref().ok_or("the extension has no highlights.scm for this language")?;
            let query = String::from_utf8(read_capped(highlights, MAX_QUERY_BYTES)?).map_err(|_| "highlights.scm is not UTF-8".to_string())?;
            let mut store = WasmStore::new(engine()).map_err(|e| format!("wasm runtime: {}", e.message))?;
            let language = store.load_language(grammar, &bytes).map_err(|e| format!("grammar {grammar:?}: {}", e.message))?;
            compile(language, &query, Some(store))
        }
    }
}

impl From<LoadedGrammar> for Grammar {
    fn from(g: LoadedGrammar) -> Self {
        Grammar { language: g.language, query: g.query, kinds: g.kinds, store: g.store.map(|s| RefCell::new(Some(s))) }
    }
}

/// Loads each grammar and its query once and starts per-document parsers.
#[derive(Default)]
pub struct Highlighter {
    /// By language name. `None`: loading failed, and is not retried.
    grammars: HashMap<String, Option<Rc<Grammar>>>,
}

impl Highlighter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `open` can work for this language now. Built-in grammars load on first use;
    /// an installed one has to be loaded with [`load_grammar`] and [`install`](Self::install)ed.
    pub fn is_ready(&self, def: &LanguageDef) -> bool {
        def.is_built_in() || matches!(self.grammars.get(&def.name), Some(Some(_)))
    }

    pub fn install(&mut self, name: &str, grammar: LoadedGrammar) {
        self.grammars.insert(name.to_owned(), Some(Rc::new(grammar.into())));
    }

    /// Drops a loaded grammar, e.g. when its extension was updated or removed.
    pub fn forget(&mut self, name: &str) {
        self.grammars.remove(name);
    }

    /// Parses `buf` and starts following its edits. `None` if the grammar or
    /// its query can't be loaded, or an installed grammar is not loaded yet.
    pub fn open(&mut self, def: &LanguageDef, buf: &Buffer) -> Option<Parsed> {
        let grammar = match self.grammars.get(&def.name) {
            Some(g) => g.clone(),
            None if def.is_built_in() => {
                let g = load_grammar(def).ok().map(|g| Rc::new(Grammar::from(g)));
                self.grammars.insert(def.name.clone(), g.clone());
                g
            }
            None => None,
        }?;
        let mut parser = Parser::new();
        if grammar.store.is_none() {
            parser.set_language(&grammar.language).ok()?;
        }
        buf.set_track_edits(true);
        let cursor = buf.edit_cursor();
        let tree = grammar.parse(&mut parser, buf, None)?;
        Some(Parsed { parser, tree: Some(tree), grammar, revision: buf.revision(), cursor })
    }
}

fn parse(parser: &mut Parser, buf: &Buffer, old: Option<&Tree>) -> Option<Tree> {
    parser.parse_with_options(&mut |byte, _| buf.bytes_from(byte), old, None)
}

fn point(p: (usize, usize)) -> Point {
    Point { row: p.0, column: p.1 }
}

impl From<TextEdit> for InputEdit {
    fn from(e: TextEdit) -> Self {
        InputEdit {
            start_byte: e.start_byte,
            old_end_byte: e.old_end_byte,
            new_end_byte: e.new_end_byte,
            start_position: point(e.start),
            old_end_position: point(e.old_end),
            new_end_position: point(e.new_end),
        }
    }
}

/// A document's syntax tree, kept up to date with its buffer.
pub struct Parsed {
    parser: Parser,
    tree: Option<Tree>,
    grammar: Rc<Grammar>,
    revision: u64,
    /// How far into the buffer's edit log the tree has been told.
    cursor: u64,
}

impl Parsed {
    /// Brings the tree up to date with the buffer: applies the recorded edits
    /// to the old tree and re-parses, reusing everything the edits didn't touch.
    pub fn update(&mut self, buf: &Buffer) {
        if self.revision == buf.revision() {
            return;
        }
        let mut old = self.tree.take();
        match buf.edits_since(self.cursor) {
            Some(edits) if !edits.is_empty() => {
                if let Some(tree) = old.as_mut() {
                    for e in edits {
                        tree.edit(&e.into());
                    }
                }
            }
            // Edits we weren't told about: the old tree can't be trusted.
            _ => old = None,
        }
        self.tree = self.grammar.parse(&mut self.parser, buf, old.as_ref());
        self.revision = buf.revision();
        self.cursor = buf.edit_cursor();
    }

    /// Coloured spans for lines `first..first + count` (a `Vec` per line).
    /// Call `update` first.
    pub fn line_spans(&self, buf: &Buffer, first: usize, count: usize) -> Vec<Vec<Span>> {
        let end_line = (first + count).min(buf.len_lines());
        if first >= end_line {
            return Vec::new();
        }
        let Some(tree) = &self.tree else { return vec![Vec::new(); end_line - first] };

        let start_byte = buf.line_to_byte(first);
        let end_byte = buf.line_to_byte(end_line);
        // One kind per byte; later captures paint over earlier ones, so a
        // nested capture (an escape inside a string) wins.
        let mut paint = vec![Kind::Plain; end_byte - start_byte];
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(start_byte..end_byte);
        let mut captures = cursor.captures(&self.grammar.query, tree.root_node(), |node: Node| {
            buf.byte_chunks(node.byte_range())
        });
        while let Some((m, index)) = captures.next() {
            let capture = m.captures()[*index];
            let kind = self.grammar.kinds[capture.index as usize];
            if kind == Kind::Plain {
                continue;
            }
            let range = capture.node.byte_range();
            let (a, b) = (range.start.max(start_byte), range.end.min(end_byte));
            if a < b {
                paint[a - start_byte..b - start_byte].fill(kind);
            }
        }

        (first..end_line)
            .map(|n| {
                let line_start = buf.line_to_byte(n) - start_byte;
                runs(&buf.line(n), &paint[line_start..])
            })
            .collect()
    }
}

/// Cuts one line into coloured runs. `paint` starts at the line's first byte.
fn runs(line: &str, paint: &[Kind]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut current: Option<(usize, usize, Kind)> = None; // (start col, start byte, kind)
    let flush = |out: &mut Vec<Span>, start: (usize, usize, Kind), end_byte: usize| {
        let piece = &line[start.1..end_byte];
        let trimmed = piece.trim_start();
        let lead = piece[..piece.len() - trimmed.len()].chars().count();
        let trimmed = trimmed.trim_end();
        if !trimmed.is_empty() {
            out.push(Span { col: start.0 + lead, text: trimmed.to_string(), kind: start.2 });
        }
    };
    for (col, (byte, _)) in line.char_indices().enumerate() {
        let kind = paint.get(byte).copied().unwrap_or(Kind::Plain);
        match current {
            Some((_, _, k)) if k == kind => {}
            Some(start) => {
                flush(&mut out, start, byte);
                current = Some((col, byte, kind));
            }
            None => current = Some((col, byte, kind)),
        }
    }
    if let Some(start) = current {
        flush(&mut out, start, line.len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Languages;

    fn lang(name: &str) -> std::sync::Arc<LanguageDef> {
        Languages::built_in().by_name(name).unwrap()
    }

    fn all_spans(parsed: &Parsed, buf: &Buffer) -> Vec<Vec<Span>> {
        parsed.line_spans(buf, 0, buf.len_lines())
    }

    #[test]
    fn rust_spans_per_line() {
        let buf = Buffer::new("fn main() {\n    let x = \"hi\"; // c\n}\n");
        let parsed = Highlighter::new().open(&lang("Rust"), &buf).unwrap();
        let lines = all_spans(&parsed, &buf);
        assert_eq!(lines.len(), 4);
        assert_eq!((lines[0][0].text.as_str(), lines[0][0].kind), ("fn", Kind::Keyword));
        let kinds: Vec<_> = lines[1].iter().map(|s| (s.col, s.kind)).collect();
        assert!(kinds.contains(&(4, Kind::Keyword)), "{kinds:?}");
        assert!(kinds.contains(&(12, Kind::String)), "{kinds:?}");
        assert!(lines[1].iter().any(|s| s.kind == Kind::Comment && s.col == 18));
    }

    #[test]
    fn multiline_block_comment_and_unicode_columns() {
        let buf = Buffer::new("/* a\n b */ let é = 1;");
        let parsed = Highlighter::new().open(&lang("Rust"), &buf).unwrap();
        let lines = all_spans(&parsed, &buf);
        assert_eq!(lines[0][0].kind, Kind::Comment);
        assert_eq!(lines[1][0].kind, Kind::Comment);
        // `1` is at char column 14 even though `é` is two bytes.
        let one = lines[1].iter().find(|s| s.text.contains('1')).unwrap();
        assert_eq!(one.kind, Kind::Constant);
        assert_eq!(one.col, 14);
    }

    #[test]
    fn visible_window_matches_full_document() {
        let src: String = (0..50).map(|i| format!("fn f{i}() {{ let s = \"x{i}\"; }}\n")).collect();
        let buf = Buffer::new(&src);
        let parsed = Highlighter::new().open(&lang("Rust"), &buf).unwrap();
        let full = all_spans(&parsed, &buf);
        assert_eq!(parsed.line_spans(&buf, 20, 5), full[20..25].to_vec());
    }

    #[test]
    fn every_built_in_language_loads() {
        let mut h = Highlighter::new();
        for def in Languages::built_in().all() {
            let buf = Buffer::new("x");
            assert!(h.is_ready(def) && h.open(def, &buf).is_some(), "{} failed to load", def.name);
        }
    }

    #[test]
    fn a_grammar_that_cannot_load_is_an_error_not_a_crash() {
        let dir = std::env::temp_dir().join(format!("forge_grammar_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wasm, query) = (dir.join("g.wasm"), dir.join("highlights.scm"));
        let def = |wasm: &std::path::Path, highlights: Option<&std::path::Path>| LanguageDef {
            name: "Fake".into(),
            suffixes: vec!["fake".into()],
            source: GrammarSource::Wasm { grammar: "fake".into(), wasm: wasm.to_owned(), highlights: highlights.map(ToOwned::to_owned), version: "1".into() },
        };
        // Missing files, garbage that is not wasm, and a missing query.
        assert!(load_grammar(&def(&wasm, Some(&query))).is_err());
        std::fs::write(&wasm, b"this is not wasm").unwrap();
        std::fs::write(&query, "(comment) @comment").unwrap();
        assert!(load_grammar(&def(&wasm, Some(&query))).is_err());
        assert!(load_grammar(&def(&wasm, None)).is_err());
        // An installed language is not ready, and does not open, until its grammar is installed.
        let mut h = Highlighter::new();
        let fake = def(&wasm, Some(&query));
        assert!(!h.is_ready(&fake));
        assert!(h.open(&fake, &Buffer::new("x")).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_loaded_grammar_can_cross_threads() {
        fn send<T: Send>() {}
        send::<LoadedGrammar>();
    }

    /// After every kind of edit, the incrementally updated tree must colour the
    /// document exactly as a fresh parse of the same text does.
    #[test]
    fn incremental_matches_fresh_parse() {
        let mut h = Highlighter::new();
        let mut buf = Buffer::new("fn main() {\n    let a = 1;\n    // note\n    println!(\"é{}\", a);\n}\n");
        let mut parsed = h.open(&lang("Rust"), &buf).unwrap();

        let check = |parsed: &mut Parsed, buf: &Buffer, step: &str| {
            parsed.update(buf);
            let fresh = Highlighter::new().open(&lang("Rust"), buf).unwrap();
            assert_eq!(all_spans(parsed, buf), all_spans(&fresh, buf), "after {step}");
        };

        buf.set_caret(1, 4, false);
        for c in "let b = \"ü\";".chars() {
            buf.type_char(c);
        }
        check(&mut parsed, &buf, "typing a statement");
        buf.newline();
        buf.insert("/* open");
        check(&mut parsed, &buf, "opening a block comment");
        buf.insert(" close */");
        check(&mut parsed, &buf, "closing it");
        buf.set_caret(0, 0, false);
        buf.set_caret(2, 0, true);
        buf.insert("");
        check(&mut parsed, &buf, "deleting two lines");
        buf.undo();
        check(&mut parsed, &buf, "undo");
        buf.redo();
        check(&mut parsed, &buf, "redo");
        buf.select_all();
        buf.toggle_comment("//");
        check(&mut parsed, &buf, "commenting everything");
        buf.undo();
        buf.set_caret(3, 5, false);
        buf.move_lines(1);
        check(&mut parsed, &buf, "moving a line");
        buf.select_all();
        buf.insert("struct S;\n");
        check(&mut parsed, &buf, "replacing everything");
    }

    /// Prints parse and highlight timings (`cargo test --release -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn timing() {
        let src = include_str!("buffer.rs").repeat(30);
        let mut buf = Buffer::new(&src);
        let mut h = Highlighter::new();
        let t = std::time::Instant::now();
        let mut parsed = h.open(&lang("Rust"), &buf).unwrap();
        println!("{} KB, {} lines: first parse {:?}", src.len() / 1024, buf.len_lines(), t.elapsed());

        buf.set_caret(5000, 4, false);
        let t = std::time::Instant::now();
        let n = 200;
        for _ in 0..n {
            buf.type_char('x');
            parsed.update(&buf);
            let _ = parsed.line_spans(&buf, 4980, 60);
        }
        println!("per keystroke (edit + reparse + 60 visible lines): {:?}", t.elapsed() / n);
        let t = std::time::Instant::now();
        let _ = parsed.line_spans(&buf, 4980, 60);
        println!("visible lines only (scroll/caret move): {:?}", t.elapsed());
    }
}
