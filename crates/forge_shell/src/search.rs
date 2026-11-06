//! Project-wide text search: walks the project (honouring `.gitignore`), looks
//! for a literal string line by line, and returns hits with a ready-made preview.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use ignore::WalkBuilder;
use lsp_client::path_key;

/// Directories never searched, whatever the ignore files say.
const SKIPPED_DIRS: [&str; 3] = [".git", "target", "node_modules"];
/// Characters of the line kept in a preview.
const PREVIEW_CHARS: usize = 200;
/// How much of the line before the match stays visible when the match is far in.
const PREVIEW_LEAD: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    pub line: usize,
    /// Char column and length of the match in the file's line.
    pub col: usize,
    pub len: usize,
    /// The line as shown in the panel, with the match at `preview_start..preview_end` (chars).
    pub preview: String,
    pub preview_start: usize,
    pub preview_end: usize,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub hits: Vec<Hit>,
    /// Files that have at least one hit.
    pub files: usize,
    /// The hit cap was reached, so there are more than `hits` holds.
    pub truncated: bool,
    pub cancelled: bool,
}

fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(a), None) => a,
        _ => c,
    }
}

/// Where `query` occurs in `line`, as (char column, char length), without overlaps.
pub fn find_in_line(line: &str, query: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let qlen = query.chars().count();
    // Fast paths work on bytes: the same text, or ASCII-lowercased text of the same length.
    let by_bytes = |hay: &str, needle: &str| -> Vec<(usize, usize)> {
        let ascii = line.is_ascii();
        let (mut last_byte, mut last_char) = (0, 0);
        hay.match_indices(needle)
            .map(|(b, _)| {
                last_char += if ascii { b - last_byte } else { line[last_byte..b].chars().count() };
                last_byte = b;
                (last_char, qlen)
            })
            .collect()
    };
    if case_sensitive {
        return by_bytes(line, query);
    }
    if line.is_ascii() && query.is_ascii() {
        return by_bytes(&line.to_ascii_lowercase(), &query.to_ascii_lowercase());
    }
    let hay: Vec<char> = line.chars().map(fold).collect();
    let needle: Vec<char> = query.chars().map(fold).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()] == needle[..] {
            out.push((i, needle.len()));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// The line trimmed and cut to a panel-sized preview, with the match located in it.
fn preview(line: &str, col: usize, len: usize) -> (String, usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    let indent = chars.iter().take_while(|c| c.is_whitespace()).count().min(col);
    // A match far into the line is shown with a short lead-in instead of being cut off.
    let from = if col - indent > PREVIEW_LEAD * 2 { col - PREVIEW_LEAD } else { indent };
    let to = (from + PREVIEW_CHARS).min(chars.len());
    let mut text = String::new();
    let mut shift = 0;
    if from > indent {
        text.push('…');
        shift = 1;
    }
    text.extend(chars[from..to].iter().map(|&c| if c == '\t' { ' ' } else { c }));
    if to < chars.len() {
        text.push('…');
    }
    (text, col - from + shift, (col + len).min(to) - from + shift)
}

/// Searches `root` for `query`. Files in `open` (keyed by `path_key`) are
/// searched as the editor holds them, not as they are on disk. Stops early
/// once `cancel` is set or `cap` hits are found.
pub fn search(root: &Path, query: &str, case_sensitive: bool, open: &HashMap<String, String>, max_bytes: u64, cancel: &AtomicBool, cap: usize) -> Outcome {
    let mut out = Outcome::default();
    if query.is_empty() || query.contains('\n') {
        return out;
    }
    let walk = WalkBuilder::new(root)
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .filter_entry(|e| !(e.file_type().is_some_and(|t| t.is_dir()) && e.file_name().to_str().is_some_and(|n| SKIPPED_DIRS.contains(&n))))
        .build();
    for entry in walk.flatten() {
        if cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            return out;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let text = match open.get(&path_key(path)) {
            Some(text) => text.clone(),
            None => {
                if entry.metadata().is_ok_and(|m| m.len() > max_bytes) {
                    continue;
                }
                let Ok(bytes) = fs::read(path) else { continue };
                // A NUL early on means a binary file.
                if bytes.iter().take(8000).any(|&b| b == 0) {
                    continue;
                }
                match String::from_utf8(bytes) {
                    Ok(text) => text,
                    Err(_) => continue,
                }
            }
        };
        let before = out.hits.len();
        for (n, line) in text.split('\n').enumerate() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            for (col, len) in find_in_line(line, query, case_sensitive) {
                let (preview, preview_start, preview_end) = preview(line, col, len);
                out.hits.push(Hit { path: path.to_path_buf(), line: n, col, len, preview, preview_start, preview_end });
                if out.hits.len() >= cap {
                    out.files += 1;
                    out.truncated = true;
                    return out;
                }
            }
        }
        if out.hits.len() > before {
            out.files += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forge_search_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run(dir: &Path, query: &str, case: bool) -> Outcome {
        search(dir, query, case, &HashMap::new(), 1 << 20, &AtomicBool::new(false), 1000)
    }

    #[test]
    fn finds_every_occurrence_with_char_columns() {
        assert_eq!(find_in_line("foo bar foo", "foo", true), [(0, 3), (8, 3)]);
        assert_eq!(find_in_line("Foo bar", "foo", true), []);
        assert_eq!(find_in_line("Foo bar FOO", "foo", false), [(0, 3), (8, 3)]);
        // Columns count chars, not bytes, after multi-byte text.
        assert_eq!(find_in_line("héllo wörld", "wörld", true), [(6, 5)]);
        assert_eq!(find_in_line("héllo WÖRLD", "wörld", false), [(6, 5)]);
        assert_eq!(find_in_line("😀 abc", "abc", true), [(2, 3)]);
        // Matches don't overlap.
        assert_eq!(find_in_line("aaaa", "aa", true), [(0, 2), (2, 2)]);
        assert!(find_in_line("abc", "", true).is_empty());
    }

    #[test]
    fn previews_trim_indent_and_locate_the_match() {
        let (text, a, b) = preview("\t\tlet value = 1;", 6, 5);
        assert_eq!(text, "let value = 1;");
        assert_eq!(text.chars().skip(a).take(b - a).collect::<String>(), "value");
        // A match far into a long line gets a lead-in and stays visible.
        let long = format!("{}needle{}", "x".repeat(300), "y".repeat(300));
        let (text, a, b) = preview(&long, 300, 6);
        assert!(text.starts_with('…') && text.ends_with('…'));
        assert_eq!(text.chars().skip(a).take(b - a).collect::<String>(), "needle");
        assert!(text.chars().count() <= PREVIEW_CHARS + 2);
    }

    #[test]
    fn walks_the_tree_and_skips_ignored_and_binary_files() {
        let dir = scratch("walk");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::create_dir_all(dir.join("gen")).unwrap();
        fs::write(dir.join("src/a.rs"), "fn needle() {}\r\nlet x = 1;\nneedle again needle\n").unwrap();
        fs::write(dir.join("src/b.txt"), "no match here\n").unwrap();
        fs::write(dir.join("target/c.rs"), "needle in target\n").unwrap();
        fs::write(dir.join("gen/d.rs"), "needle in an ignored dir\n").unwrap();
        fs::write(dir.join(".gitignore"), "gen/\n").unwrap();
        fs::write(dir.join("bin.dat"), b"needle\0binary").unwrap();
        fs::write(dir.join("latin1.txt"), b"needle \xff\xfe").unwrap();

        let out = run(&dir, "needle", true);
        let at: Vec<_> = out.hits.iter().map(|h| (h.path.file_name().unwrap().to_str().unwrap(), h.line, h.col)).collect();
        assert_eq!(at, [("a.rs", 0, 3), ("a.rs", 2, 0), ("a.rs", 2, 13)]);
        assert_eq!(out.files, 1);
        assert!(!out.truncated && !out.cancelled);
        // The carriage return of a CRLF line isn't part of it.
        assert_eq!(out.hits[0].preview, "fn needle() {}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_buffers_win_over_the_disk() {
        let dir = scratch("overlay");
        let file = dir.join("a.rs");
        fs::write(&file, "on disk only\n").unwrap();
        let open = HashMap::from([(path_key(&file), "typed but unsaved needle\n".to_string())]);
        let out = search(&dir, "needle", true, &open, 1 << 20, &AtomicBool::new(false), 1000);
        assert_eq!(out.hits.len(), 1);
        assert_eq!(out.hits[0].col, 18);
        let out = search(&dir, "on disk", true, &open, 1 << 20, &AtomicBool::new(false), 1000);
        assert!(out.hits.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cap_and_cancel_stop_the_walk() {
        let dir = scratch("cap");
        fs::write(dir.join("a.txt"), "x\n".repeat(50)).unwrap();
        let out = search(&dir, "x", true, &HashMap::new(), 1 << 20, &AtomicBool::new(false), 10);
        assert_eq!((out.hits.len(), out.truncated), (10, true));
        let out = search(&dir, "x", true, &HashMap::new(), 1 << 20, &AtomicBool::new(true), 10);
        assert!(out.cancelled && out.hits.is_empty());
        // Files over the size limit are skipped.
        let out = search(&dir, "x", true, &HashMap::new(), 10, &AtomicBool::new(false), 1000);
        assert!(out.hits.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
