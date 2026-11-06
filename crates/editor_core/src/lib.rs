//! UI-free editor core: a rope text buffer with selections and undo, plus a
//! small line highlighter. Buffer columns are `char` indices; `columns` maps them to screen cells (tabs span several).

mod buffer;
pub mod columns;
mod highlight;
mod syntax;

pub use buffer::{Buffer, Match, TextEdit};
pub use highlight::{Kind, Span, Syntax, spans};
pub use syntax::{Highlighter, Language, Parsed};
