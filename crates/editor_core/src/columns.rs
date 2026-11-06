//! Character columns versus visual columns. The buffer indexes by char; the
//! screen is a grid of cells, and a tab spans up to the next tab stop.

pub const TAB_WIDTH: usize = 4;

/// The cell a char column starts at.
pub fn visual_col(line: &str, col: usize) -> usize {
    let mut v = 0;
    for c in line.chars().take(col) {
        v += if c == '\t' { TAB_WIDTH - v % TAB_WIDTH } else { 1 };
    }
    // Past the end of the line the columns stay one-to-one.
    v + col.saturating_sub(line.chars().count())
}

/// The char column nearest to a cell. A cell inside a tab goes to whichever
/// edge of the tab is closer.
pub fn char_col(line: &str, vcol: usize) -> usize {
    let mut v = 0;
    let mut n = 0;
    for c in line.chars() {
        let w = if c == '\t' { TAB_WIDTH - v % TAB_WIDTH } else { 1 };
        if vcol < v + w {
            return if vcol - v > w / 2 { n + 1 } else { n };
        }
        v += w;
        n += 1;
    }
    n + (vcol - v)
}

/// The line as drawn: tabs expanded to spaces.
pub fn expand_tabs(line: &str) -> String {
    expand_tabs_at(line, 0)
}

/// `text` expanded as if it started at cell `start` of its line.
pub fn expand_tabs_at(text: &str, start: usize) -> String {
    if !text.contains('\t') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut v = start;
    for c in text.chars() {
        if c == '\t' {
            let w = TAB_WIDTH - v % TAB_WIDTH;
            out.extend(std::iter::repeat_n(' ', w));
            v += w;
        } else {
            out.push(c);
            v += 1;
        }
    }
    out
}

/// The line's width in cells.
pub fn width(line: &str) -> usize {
    visual_col(line, line.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_advance_to_the_next_stop() {
        assert_eq!(visual_col("\tx", 1), 4);
        assert_eq!(visual_col("ab\tx", 3), 4);
        assert_eq!(visual_col("abcd\tx", 5), 8);
        assert_eq!(visual_col("\t\tx", 2), 8);
        assert_eq!(visual_col("abc", 5), 5);
    }

    #[test]
    fn cells_map_back_to_the_nearest_edge() {
        let line = "a\tb";
        // 'a' at cell 0, the tab covers cells 1..4, 'b' at cell 4.
        assert_eq!([0, 1, 2, 3, 4, 5].map(|v| char_col(line, v)), [0, 1, 1, 2, 2, 3]);
        assert_eq!(char_col("", 3), 3);
        for n in 0..=line.chars().count() {
            assert_eq!(char_col(line, visual_col(line, n)), n);
        }
    }

    #[test]
    fn expansion_matches_the_columns() {
        assert_eq!(expand_tabs("a\tb"), "a   b");
        assert_eq!(expand_tabs("\t\tx"), "        x");
        assert_eq!(expand_tabs("plain"), "plain");
        assert_eq!(width("a\tb"), 5);
    }
}
