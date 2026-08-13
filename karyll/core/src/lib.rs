//! Text handling for karyll: the buffer, the edit model, and line breaking.
//!
//! Nothing here touches a font, a screen or a file, so all of it runs under
//! `cargo test` on a development machine rather than only on the device.

pub mod buffer;
pub mod doc;

pub mod script;
pub mod sentence;
pub mod undo;
pub mod word;
pub mod words;
pub mod wrap;

pub use buffer::Buffer;
pub use doc::Document;
pub use markdown::{
    Block, Continue, LineMarkup, Span, Style, analyze, continues, toggle_emphasis, toggle_heading,
};
pub use script::{Role, Script, is_invisible, role_for, runs, script_of};
pub use sentence::sentence_at;
pub use undo::{Edit, History};
pub use word::{word_at, word_end, word_start};
// `words` is not re-exported flat: `karyll_core::count` would say nothing about
// what is counted, and `words::count` says it at every call site.
pub use wrap::{Class, Line, can_break_between, classify, wrap};
