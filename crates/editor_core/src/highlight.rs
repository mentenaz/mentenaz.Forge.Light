//! A small per-line tokenizer for the prototype. It is stateless (no block
//! comments across lines) and is meant to be replaced by tree-sitter.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Plain = 0,
    Keyword = 1,
    String = 2,
    Comment = 3,
    Number = 4,
    Type = 5,
    Function = 6,
    Property = 7,
    Constant = 8,
    Operator = 9,
    Punctuation = 10,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub col: usize,
    pub text: String,
    pub kind: Kind,
}

#[derive(Clone, Copy)]
pub struct Syntax {
    pub line_comment: &'static str,
    pub keywords: &'static [&'static str],
}

const RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self",
    "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while",
];
const SLINT: &[&str] = &[
    "component", "export", "import", "from", "inherits", "property", "callback", "function", "in", "out", "in-out",
    "private", "pure", "public", "for", "if", "else", "return", "root", "self", "parent", "global", "struct", "enum",
    "true", "false", "init", "changed",
];
const SCRIPT: &[&str] = &[
    "def", "class", "import", "from", "return", "if", "else", "elif", "for", "while", "function", "const", "let",
    "var", "true", "false", "null", "None", "True", "False", "export", "async", "await", "new",
];
const TOML: &[&str] = &["true", "false"];

impl Syntax {
    pub fn for_extension(ext: &str) -> Option<Self> {
        let (line_comment, keywords) = match ext {
            "rs" | "cs" | "c" | "cpp" | "h" | "go" => ("//", RUST),
            "slint" => ("//", SLINT),
            "js" | "ts" | "tsx" | "jsx" => ("//", SCRIPT),
            "py" | "sh" | "yml" | "yaml" => ("#", SCRIPT),
            "toml" => ("#", TOML),
            _ => return None,
        };
        Some(Self { line_comment, keywords })
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Tokenizes one line into coloured runs. Whitespace between plain tokens stays
/// inside one run, so a line is a handful of spans.
pub fn spans(line: &str, syntax: Option<Syntax>) -> Vec<Span> {
    let chars: Vec<char> = line.chars().collect();
    let Some(syntax) = syntax else {
        return if line.trim().is_empty() {
            Vec::new()
        } else {
            vec![Span { col: 0, text: line.to_string(), kind: Kind::Plain }]
        };
    };
    let comment: Vec<char> = syntax.line_comment.chars().collect();
    let mut tokens: Vec<(usize, usize, Kind)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if chars[i..].starts_with(&comment) {
            tokens.push((i, chars.len(), Kind::Comment));
            i = chars.len();
        } else if c == '"' || (c == '\'' && chars[i + 1..].iter().take(3).any(|&x| x == '\'')) {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                j += if chars[j] == '\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(chars.len());
            tokens.push((i, end, Kind::String));
            i = end;
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '.' || chars[j] == '_') {
                j += 1;
            }
            tokens.push((i, j, Kind::Number));
            i = j;
        } else if is_ident(c) {
            let mut j = i;
            while j < chars.len() && is_ident(chars[j]) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let kind = if syntax.keywords.contains(&word.as_str()) {
                Kind::Keyword
            } else if c.is_uppercase() {
                Kind::Type
            } else {
                Kind::Plain
            };
            tokens.push((i, j, kind));
            i = j;
        } else {
            tokens.push((i, i + 1, Kind::Plain));
            i += 1;
        }
    }

    // Merge neighbouring plain tokens (whitespace between them included).
    let mut merged: Vec<(usize, usize, Kind)> = Vec::new();
    for t in tokens {
        match merged.last_mut() {
            Some(last) if last.2 == Kind::Plain && t.2 == Kind::Plain => last.1 = t.1,
            _ => merged.push(t),
        }
    }
    merged
        .into_iter()
        .map(|(a, b, kind)| Span { col: a, text: chars[a..b].iter().collect(), kind })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_line() {
        let s = spans("let x = \"hi\"; // note", Syntax::for_extension("rs"));
        let kinds: Vec<_> = s.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [Kind::Keyword, Kind::Plain, Kind::String, Kind::Plain, Kind::Comment]);
        assert_eq!(s[2].col, 8);
    }

    #[test]
    fn lifetimes_are_not_strings() {
        let s = spans("fn f<'a>(x: &'a str)", Syntax::for_extension("rs"));
        assert!(s.iter().all(|s| s.kind != Kind::String));
    }
}
