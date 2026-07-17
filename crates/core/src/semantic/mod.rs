//! 语义层:DOM → a11y snapshot 风格的 IR。

pub mod diff;
pub mod extract;
pub mod ir;

pub use extract::extract;
pub use ir::{Role, SemanticNode, Snapshot, State};
