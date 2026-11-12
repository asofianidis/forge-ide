//! Editor core: open documents, syntax highlighting and the line-number gutter.

pub mod conflicts;
mod document;
pub mod gutter;
mod highlight;
pub mod scm;

pub use document::{DiskEvent, DiskState, Document};
pub use highlight::Highlighter;
