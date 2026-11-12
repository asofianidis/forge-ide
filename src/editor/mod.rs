//! Editor core: text buffers, open documents, syntax highlighting and the line-number gutter.

mod buffer;
pub mod conflicts;
mod document;
pub mod find;
pub mod gutter;
mod highlight;
pub mod ops;
pub mod scm;

pub use buffer::Buffer;
pub use document::{DiskEvent, DiskState, Document};
pub use highlight::Highlighter;
