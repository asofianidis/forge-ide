//! Editor core: text buffers, open documents, syntax highlighting and the line-number gutter.

mod buffer;
mod document;
pub mod gutter;
mod highlight;

pub use buffer::Buffer;
pub use document::Document;
pub use highlight::Highlighter;
