//! Editor core: open documents, syntax highlighting and the line-number gutter.

mod document;
pub mod find;
pub mod gutter;
mod highlight;
pub mod ops;

pub use document::Document;
pub use highlight::Highlighter;
