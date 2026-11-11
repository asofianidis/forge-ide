//! Editor core: text buffers, open documents, syntax highlighting and the line-number gutter.

mod buffer;
mod document;
pub mod gutter;
mod highlight;
pub mod search;

pub use buffer::Buffer;
pub use document::{Diagnostic, Document, Severity};
pub use highlight::Highlighter;
