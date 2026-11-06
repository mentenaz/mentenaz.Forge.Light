//! Completion: the request, tolerant parsing of the several shapes servers
//! answer in, snippet text reduced to plain text, and the local fuzzy
//! filtering that runs on every keystroke after the one request.

use crate::client::{Client, Error};
use crate::docs::ContentChange;
use crate::requests::plain_text;
use serde_json::{Value, json};

/// One suggestion, reduced to what the editor needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    /// LSP `CompletionItemKind` number (3 = function, 6 = variable, ...), 0 if unknown.
    pub kind: u8,
    pub detail: String,
    pub doc: String,
    pub sort_text: String,
    pub filter_text: String,
    /// The text to insert (snippet markers already removed).
    pub text: String,
    /// Where the caret goes inside `text`, in chars, and where its selection
    /// ends (the first placeholder's default text is selected so typing replaces it).
    pub cursor: usize,
    pub select_end: usize,
    /// The range `text` replaces, if the server named one: ((line, UTF-16 col), (line, UTF-16 col)).
    pub range: Option<((usize, usize), (usize, usize))>,
    /// Extra edits to apply with it (such as a `use` line), in the request's coordinates.
    pub extra: Vec<ContentChange>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionList {
    pub items: Vec<CompletionItem>,
    /// The server wants to be asked again as the word grows.
    pub incomplete: bool,
}

/// How completion was triggered: `Invoked` (typing or Ctrl+Space) or a trigger character.
pub enum Trigger {
    Invoked,
    Character(char),
}

pub async fn completion(client: &Client, uri: &str, pos: (usize, usize), trigger: Trigger) -> Result<CompletionList, Error> {
    let context = match trigger {
        Trigger::Invoked => json!({"triggerKind": 1}),
        Trigger::Character(c) => json!({"triggerKind": 2, "triggerCharacter": c.to_string()}),
    };
    let params = json!({
        "textDocument": {"uri": uri},
        "position": {"line": pos.0, "character": pos.1},
        "context": context,
    });
    let reply = client.request_raw("textDocument/completion", params).await?;
    Ok(parse_completion(&reply))
}

fn range_of(v: &Value) -> Option<((usize, usize), (usize, usize))> {
    let pos = |p: &Value| Some((p.get("line")?.as_u64()? as usize, p.get("character")?.as_u64()? as usize));
    Some((pos(v.get("start")?)?, pos(v.get("end")?)?))
}

fn doc_text(v: &Value) -> String {
    match v {
        Value::String(s) => plain_text(s),
        Value::Object(o) => o.get("value").and_then(Value::as_str).map(plain_text).unwrap_or_default(),
        _ => String::new(),
    }
}

fn parse_item(v: &Value) -> Option<CompletionItem> {
    let label = v.get("label")?.as_str()?.to_owned();
    let text_of = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);

    // `textEdit` is either a TextEdit or an InsertReplaceEdit (insert/replace ranges).
    let edit = v.get("textEdit");
    let range = edit.and_then(|e| e.get("range").or_else(|| e.get("insert")).or_else(|| e.get("replace"))).and_then(range_of);
    let raw = edit.and_then(|e| e.get("newText")).and_then(Value::as_str).map(str::to_owned).or_else(|| text_of("insertText")).unwrap_or_else(|| label.clone());
    let snippet = v.get("insertTextFormat").and_then(Value::as_u64) == Some(2);
    let (text, cursor, select_end) = if snippet {
        snippet_to_text(&raw)
    } else {
        let len = raw.chars().count();
        (raw, len, len)
    };

    let mut detail = text_of("detail").unwrap_or_default();
    if let Some(d) = v.get("labelDetails").and_then(|d| d.get("detail")).and_then(Value::as_str) {
        detail = if detail.is_empty() { d.trim().to_owned() } else { format!("{} {}", d.trim(), detail) };
    }
    let extra = v
        .get("additionalTextEdits")
        .and_then(Value::as_array)
        .map(|edits| {
            edits
                .iter()
                .filter_map(|e| {
                    let (start, end) = range_of(e.get("range")?)?;
                    Some(ContentChange { start, end, text: e.get("newText")?.as_str()?.to_owned() })
                })
                .collect()
        })
        .unwrap_or_default();

    Some(CompletionItem {
        kind: v.get("kind").and_then(Value::as_u64).unwrap_or(0) as u8,
        detail,
        doc: v.get("documentation").map(doc_text).unwrap_or_default(),
        sort_text: text_of("sortText").unwrap_or_else(|| label.clone()),
        filter_text: text_of("filterText").unwrap_or_else(|| label.clone()),
        text,
        cursor,
        select_end,
        range,
        extra,
        label,
    })
}

/// A `CompletionItem[]` or a `CompletionList` (or null).
pub fn parse_completion(v: &Value) -> CompletionList {
    let (items, incomplete) = match v {
        Value::Array(items) => (items.as_slice(), false),
        Value::Object(o) => (o.get("items").and_then(Value::as_array).map_or(&[][..], Vec::as_slice), o.get("isIncomplete").and_then(Value::as_bool).unwrap_or(false)),
        _ => (&[][..], false),
    };
    CompletionList { items: items.iter().filter_map(parse_item).collect(), incomplete }
}

/// Snippet syntax as plain text plus the span (in chars) to select afterwards:
/// placeholders become their default text, and the first tab stop (else `$0`,
/// else the end) is where the caret goes, selecting that placeholder's text.
pub fn snippet_to_text(snippet: &str) -> (String, usize, usize) {
    let chars: Vec<char> = snippet.chars().collect();
    let mut out = String::new();
    let mut out_len = 0;
    // (tab stop number, start offset, end offset).
    let mut stops: Vec<(u32, usize, usize)> = Vec::new();
    let mut i = 0;
    let push = |out: &mut String, c: char, len: &mut usize| {
        out.push(c);
        *len += 1;
    };
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() && matches!(chars[i + 1], '$' | '}' | '\\') => {
                push(&mut out, chars[i + 1], &mut out_len);
                i += 2;
            }
            '$' if i + 1 < chars.len() && chars[i + 1].is_ascii_digit() => {
                let mut j = i + 1;
                let mut n = 0u32;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    n = n * 10 + chars[j].to_digit(10).unwrap();
                    j += 1;
                }
                stops.push((n, out_len, out_len));
                i = j;
            }
            '$' if i + 1 < chars.len() && chars[i + 1] == '{' => {
                // ${n:default} or ${n|a,b|}: find the matching close brace.
                let mut j = i + 2;
                let mut n = 0u32;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    n = n * 10 + chars[j].to_digit(10).unwrap();
                    j += 1;
                }
                let stop_start = out_len;
                let mut depth = 1;
                let mut body = String::new();
                let mut k = j;
                while k < chars.len() {
                    match chars[k] {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        c => body.push(c),
                    }
                    k += 1;
                }
                let default = if let Some(rest) = body.strip_prefix(':') {
                    rest.to_owned()
                } else if let Some(choices) = body.strip_prefix('|') {
                    choices.trim_end_matches('|').split(',').next().unwrap_or_default().to_owned()
                } else {
                    String::new()
                };
                for c in default.chars() {
                    push(&mut out, c, &mut out_len);
                }
                stops.push((n, stop_start, out_len));
                i = k + 1;
            }
            c => {
                push(&mut out, c, &mut out_len);
                i += 1;
            }
        }
    }
    let first = stops
        .iter()
        .filter(|(n, _, _)| *n > 0)
        .min_by_key(|(n, _, _)| *n)
        .or_else(|| stops.iter().find(|(n, _, _)| *n == 0))
        .map_or((out_len, out_len), |&(_, start, end)| (start, end));
    (out, first.0, first.1)
}

/// How well `query` matches `candidate` as a case-insensitive subsequence;
/// `None` if it doesn't. Prefixes, word starts and runs score higher.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().collect();
    let c: Vec<char> = candidate.chars().collect();
    let mut score = 0;
    let mut qi = 0;
    let mut prev: Option<usize> = None;
    for (i, &ch) in c.iter().enumerate() {
        if qi == q.len() {
            break;
        }
        if ch.to_lowercase().eq(q[qi].to_lowercase()) {
            score += 1;
            if ch == q[qi] {
                score += 1;
            }
            let boundary = i == 0 || c[i - 1] == '_' || (c[i - 1].is_lowercase() && ch.is_uppercase());
            if boundary {
                score += 8;
            }
            match prev {
                Some(p) if p + 1 == i => score += 5,
                Some(p) => score -= ((i - p - 1) as i32).min(6),
                None => score -= (i as i32).min(6),
            }
            prev = Some(i);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    let (lq, lc) = (query.to_lowercase(), candidate.to_lowercase());
    if lc == lq {
        score += 100;
    } else if lc.starts_with(&lq) {
        score += 50;
    }
    Some(score)
}

/// Indices into `items` that match `query`, best first (at most 200).
pub fn filter_and_rank(items: &[CompletionItem], query: &str) -> Vec<usize> {
    let mut scored: Vec<(i32, usize)> = items.iter().enumerate().filter_map(|(i, it)| fuzzy_score(query, &it.filter_text).map(|s| (s, i))).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| items[a.1].sort_text.cmp(&items[b.1].sort_text)).then_with(|| items[a.1].label.cmp(&items[b.1].label)));
    scored.truncate(200);
    scored.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, sort: &str) -> CompletionItem {
        CompletionItem {
            label: label.into(),
            kind: 3,
            detail: String::new(),
            doc: String::new(),
            sort_text: sort.into(),
            filter_text: label.into(),
            text: label.into(),
            cursor: label.chars().count(),
            select_end: label.chars().count(),
            range: None,
            extra: Vec::new(),
        }
    }

    #[test]
    fn snippets_become_text_with_a_caret() {
        assert_eq!(snippet_to_text("answer($0)"), ("answer()".to_string(), 7, 7));
        assert_eq!(snippet_to_text("foo(${1:a}, ${2:b})$0"), ("foo(a, b)".to_string(), 4, 5), "the first placeholder is selected");
        assert_eq!(snippet_to_text("for ${1:i} in ${2:iter} {\n\t$0\n}").0, "for i in iter {\n\t\n}");
        assert_eq!(snippet_to_text("x($1)"), ("x()".to_string(), 2, 2));
        assert_eq!(snippet_to_text(r"cost \$5 ${1|a,b|}").0, "cost $5 a");
        assert_eq!(snippet_to_text("plain"), ("plain".to_string(), 5, 5));
    }

    #[test]
    fn parses_lists_text_edits_and_extras() {
        let v = json!({"isIncomplete": true, "items": [
            {"label": "answer", "kind": 3, "detail": "fn() -> u8", "sortText": "0001",
             "documentation": {"kind": "markdown", "value": "The **answer**."},
             "textEdit": {"range": {"start": {"line": 3, "character": 14}, "end": {"line": 3, "character": 16}}, "newText": "answer($0)"},
             "insertTextFormat": 2,
             "additionalTextEdits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "use a::b;\n"}]},
            {"label": "plain", "insertText": "plain_text"},
            {"label": "replace", "textEdit": {"newText": "r", "insert": {"start": {"line": 1, "character": 1}, "end": {"line": 1, "character": 2}},
                                               "replace": {"start": {"line": 1, "character": 1}, "end": {"line": 1, "character": 5}}}},
            {"nolabel": true}
        ]});
        let list = parse_completion(&v);
        assert!(list.incomplete);
        assert_eq!(list.items.len(), 3, "an item without a label is skipped");
        let a = &list.items[0];
        assert_eq!((a.text.as_str(), a.cursor, a.select_end), ("answer()", 7, 7));
        assert_eq!(a.range, Some(((3, 14), (3, 16))));
        assert_eq!(a.extra.len(), 1);
        assert_eq!(a.doc, "The answer.");
        assert_eq!(a.detail, "fn() -> u8");
        assert_eq!(list.items[1].text, "plain_text");
        assert_eq!(list.items[2].range, Some(((1, 1), (1, 2))), "the insert range is used");
        assert!(parse_completion(&json!([{"label": "x"}])).items.len() == 1);
        assert!(parse_completion(&Value::Null).items.is_empty());
    }

    #[test]
    fn fuzzy_matching_and_ranking() {
        assert!(fuzzy_score("hm", "HashMap").is_some());
        assert!(fuzzy_score("zz", "HashMap").is_none());
        assert!(fuzzy_score("an", "answer").unwrap() > fuzzy_score("an", "scan").unwrap(), "a prefix beats a late match");
        assert!(fuzzy_score("ans", "answer").unwrap() > fuzzy_score("ans", "a_n_s").unwrap(), "a run beats scattered letters");
        assert!(fuzzy_score("answer", "answer").unwrap() > fuzzy_score("answer", "answerer").unwrap());
        assert_eq!(fuzzy_score("", "anything"), Some(0));

        let items = vec![item("scan", "1"), item("answer", "3"), item("another", "2")];
        let order = filter_and_rank(&items, "an");
        let labels: Vec<_> = order.iter().map(|&i| items[i].label.as_str()).collect();
        assert_eq!(labels, ["another", "answer", "scan"], "prefix matches first, then server order");
        let all = filter_and_rank(&items, "");
        assert_eq!(all, [0, 2, 1], "an empty query keeps the server's sort order");
    }
}
