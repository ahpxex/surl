//! surl 的 DOM 层:arena 存储的文档树 + html5ever 解析/序列化。
//!
//! 设计要点(见 CLAUDE.md「绑定层」决策):节点全部活在 [`Document`] 内部的
//! slotmap 里,外界(包括将来的 JS 绑定)只持 [`NodeId`] 句柄。没有 Rc、没有
//! 跨堆指针,DOM 的唯一所有者是 Rust。

pub mod parser;
pub mod select;
pub mod serialize;
pub mod tree;

pub use parser::{parse_fragment, parse_html};
pub use tree::{Attr, Document, ElementData, Node, NodeData, NodeId};

// 下游 crate(语义提取、JS 绑定)需要用到的 html5ever 名字类型,统一从这里走,
// 避免各 crate 直接依赖 html5ever 造成版本漂移。
pub use html5ever::{LocalName, QualName, local_name, ns};
