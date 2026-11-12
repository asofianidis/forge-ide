//! Editor core: text buffers, open documents, syntax highlighting, the
//! editor widget and its views, and the line-number gutter.

mod buffer;
pub mod conflicts;
mod document;
pub mod find;
pub mod folding;
pub mod gutter;
mod highlight;
pub mod multi;
pub mod ops;
pub mod scm;
pub mod selection;
pub mod view;
pub mod widget;

pub use buffer::{Buffer, EditKind};
pub use document::{DiskEvent, DiskState, Document};
pub use highlight::{HighlightCache, Highlighter};
