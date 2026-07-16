//! Arena DOM:所有节点活在 `Document` 内部的 slotmap 里,外部只持 `NodeId` 句柄。
//!
//! 这是 resource-table 模式的地基:将来 JS 侧拿到的也是这里的 NodeId(经绑定层
//! 包装),Rust 侧永远是唯一的所有者,不存在跨 GC 边界的对象环。

use html5ever::interface::QuirksMode;
use html5ever::{LocalName, QualName};
use slotmap::{SlotMap, new_key_type};

new_key_type! {
    /// DOM 节点句柄。Copy、代际安全(节点删除后旧句柄失效而非悬垂)。
    pub struct NodeId;
}

impl NodeId {
    /// 跨 FFI/JS 边界的数字表示。arena 槽位不复用时值 < 2^53,可安全放进
    /// JS Number。0 保留作 null 哨兵(slotmap 的占用版本号从 1 起)。
    pub fn to_ffi(self) -> u64 {
        use slotmap::Key;
        self.data().as_ffi()
    }

    pub fn from_ffi(raw: u64) -> NodeId {
        slotmap::KeyData::from_ffi(raw).into()
    }
}

/// 节点数据。Document/Fragment 是容器根,Element 带标签与属性,其余是叶子。
#[derive(Debug)]
pub enum NodeData {
    Document,
    /// DocumentFragment:template contents、innerHTML 重入解析等场景的挂载根。
    Fragment,
    Doctype {
        name: String,
        public_id: String,
        system_id: String,
    },
    Text {
        contents: String,
    },
    Comment {
        contents: String,
    },
    Element(ElementData),
    ProcessingInstruction {
        target: String,
        data: String,
    },
}

/// 属性:保留文档序,名字用 html5ever 的 QualName(atom 化,比较廉价)。
#[derive(Debug, Clone)]
pub struct Attr {
    pub name: QualName,
    pub value: String,
}

#[derive(Debug)]
pub struct ElementData {
    pub name: QualName,
    pub attrs: Vec<Attr>,
    /// 仅 `<template>` 有:其内容挂在一个独立的 Fragment 下,不在正常子树里。
    pub template_contents: Option<NodeId>,
}

impl ElementData {
    /// 无命名空间属性查值(HTML 属性的常规情况)。
    pub fn attr(&self, local: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.name.ns.is_empty() && *a.name.local == *local)
            .map(|a| a.value.as_str())
    }

    /// 元素本地名(html 命名空间下即标签名,已是小写)。
    pub fn local_name(&self) -> &LocalName {
        &self.name.local
    }

    pub fn is_html_element(&self, local: &str) -> bool {
        self.name.ns == html5ever::ns!(html) && *self.name.local == *local
    }
}

#[derive(Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub data: NodeData,
}

/// 一整棵文档树。节点增删改查全部经由这里——它就是 resource table。
pub struct Document {
    nodes: SlotMap<NodeId, Node>,
    root: NodeId,
    pub quirks_mode: QuirksMode,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    pub fn new() -> Self {
        let mut nodes = SlotMap::with_key();
        let root = nodes.insert(Node {
            parent: None,
            children: Vec::new(),
            data: NodeData::Document,
        });
        Document {
            nodes,
            root,
            quirks_mode: QuirksMode::NoQuirks,
        }
    }

    /// `#document` 根节点。
    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id]
    }

    /// 句柄是否仍然有效(节点未被移除)。
    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(id)
    }

    pub fn create_node(&mut self, data: NodeData) -> NodeId {
        self.nodes.insert(Node {
            parent: None,
            children: Vec::new(),
            data,
        })
    }

    pub fn element(&self, id: NodeId) -> Option<&ElementData> {
        match &self.node(id).data {
            NodeData::Element(el) => Some(el),
            _ => None,
        }
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut ElementData> {
        match &mut self.node_mut(id).data {
            NodeData::Element(el) => Some(el),
            _ => None,
        }
    }

    /// 文档元素,通常是 `<html>`。
    pub fn document_element(&self) -> Option<NodeId> {
        self.node(self.root)
            .children
            .iter()
            .copied()
            .find(|&c| matches!(self.node(c).data, NodeData::Element(_)))
    }

    // ---- 结构修改 ----

    /// 把 child 追加为 parent 的最后一个孩子。child 若已有父节点则先摘下。
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        debug_assert_ne!(parent, child);
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
    }

    /// 在 sibling 之前插入 new_node。sibling 必须有父节点。
    pub fn insert_before(&mut self, sibling: NodeId, new_node: NodeId) {
        let parent = self.nodes[sibling]
            .parent
            .expect("insert_before: sibling has no parent");
        self.detach(new_node);
        let idx = self.nodes[parent]
            .children
            .iter()
            .position(|&c| c == sibling)
            .expect("insert_before: sibling not in parent's children");
        self.nodes[parent].children.insert(idx, new_node);
        self.nodes[new_node].parent = Some(parent);
    }

    /// 把节点从其父节点上摘下(节点本身与子树保留)。
    pub fn detach(&mut self, id: NodeId) {
        if let Some(parent) = self.nodes[id].parent.take() {
            self.nodes[parent].children.retain(|&c| c != id);
        }
    }

    /// 把 from 的所有孩子按序移动到 to 下面。
    pub fn reparent_children(&mut self, from: NodeId, to: NodeId) {
        let children = std::mem::take(&mut self.nodes[from].children);
        for &c in &children {
            self.nodes[c].parent = Some(to);
        }
        self.nodes[to].children.extend(children);
    }

    /// 追加文本:若最后一个孩子已是文本节点则并入(解析器契约要求相邻文本合并)。
    pub fn append_text(&mut self, parent: NodeId, text: &str) {
        if let Some(&last) = self.nodes[parent].children.last()
            && let NodeData::Text { contents } = &mut self.nodes[last].data
        {
            contents.push_str(text);
            return;
        }
        let t = self.create_node(NodeData::Text {
            contents: text.to_owned(),
        });
        self.append_child(parent, t);
    }

    /// 在 sibling 前插入文本:若前一个兄弟是文本节点则并入。
    pub fn insert_text_before(&mut self, sibling: NodeId, text: &str) {
        let parent = self.nodes[sibling]
            .parent
            .expect("insert_text_before: sibling has no parent");
        let idx = self.nodes[parent]
            .children
            .iter()
            .position(|&c| c == sibling)
            .expect("insert_text_before: sibling not in parent's children");
        if idx > 0 {
            let prev = self.nodes[parent].children[idx - 1];
            if let NodeData::Text { contents } = &mut self.nodes[prev].data {
                contents.push_str(text);
                return;
            }
        }
        let t = self.create_node(NodeData::Text {
            contents: text.to_owned(),
        });
        self.insert_before(sibling, t);
    }

    // ---- 遍历与读取 ----

    /// 先序遍历 id 的整棵子树(含自身)。不进入 template contents。
    pub fn descendants(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let mut stack = vec![id];
        std::iter::from_fn(move || {
            let next = stack.pop()?;
            stack.extend(self.node(next).children.iter().rev());
            Some(next)
        })
    }

    /// 深拷贝本文档内的一棵子树(含 template contents),返回新根。
    /// 新子树是游离的(无父节点)。
    pub fn clone_subtree(&mut self, node: NodeId) -> NodeId {
        let (data, children) = {
            let n = self.node(node);
            (self.duplicate_data(&n.data), n.children.clone())
        };
        let new = self.create_node(data);
        // template contents 也要深拷贝(duplicate_data 里只留了 None)
        if let NodeData::Element(el) = &self.node(node).data
            && let Some(contents) = el.template_contents
        {
            let new_contents = self.clone_subtree(contents);
            if let Some(el) = self.element_mut(new) {
                el.template_contents = Some(new_contents);
            }
        }
        for child in children {
            let c = self.clone_subtree(child);
            self.append_child(new, c);
        }
        new
    }

    /// 从另一棵文档树深拷贝子树进来(innerHTML 的片段移植)。
    pub fn import_subtree(&mut self, src: &Document, node: NodeId) -> NodeId {
        let data = self.duplicate_data(&src.node(node).data);
        let new = self.create_node(data);
        if let NodeData::Element(el) = &src.node(node).data
            && let Some(contents) = el.template_contents
        {
            let new_contents = self.import_subtree(src, contents);
            if let Some(el) = self.element_mut(new) {
                el.template_contents = Some(new_contents);
            }
        }
        for &child in &src.node(node).children {
            let c = self.import_subtree(src, child);
            self.append_child(new, c);
        }
        new
    }

    /// 复制节点数据(template_contents 置空,由调用方另行深拷贝)。
    fn duplicate_data(&self, data: &NodeData) -> NodeData {
        match data {
            NodeData::Document => NodeData::Document,
            NodeData::Fragment => NodeData::Fragment,
            NodeData::Doctype { name, public_id, system_id } => NodeData::Doctype {
                name: name.clone(),
                public_id: public_id.clone(),
                system_id: system_id.clone(),
            },
            NodeData::Text { contents } => NodeData::Text {
                contents: contents.clone(),
            },
            NodeData::Comment { contents } => NodeData::Comment {
                contents: contents.clone(),
            },
            NodeData::ProcessingInstruction { target, data } => NodeData::ProcessingInstruction {
                target: target.clone(),
                data: data.clone(),
            },
            NodeData::Element(el) => NodeData::Element(ElementData {
                name: el.name.clone(),
                attrs: el.attrs.clone(),
                template_contents: None,
            }),
        }
    }

    /// 子树内所有文本节点内容拼接(近似 DOM textContent)。
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        for n in self.descendants(id) {
            if let NodeData::Text { contents } = &self.node(n).data {
                out.push_str(contents);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(doc: &mut Document, tag: &str) -> NodeId {
        let name = QualName::new(None, html5ever::ns!(html), LocalName::from(tag));
        doc.create_node(NodeData::Element(ElementData {
            name,
            attrs: Vec::new(),
            template_contents: None,
        }))
    }

    #[test]
    fn append_and_detach() {
        let mut doc = Document::new();
        let html = el(&mut doc, "html");
        let body = el(&mut doc, "body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);

        assert_eq!(doc.node(body).parent, Some(html));
        assert_eq!(doc.document_element(), Some(html));

        doc.detach(body);
        assert_eq!(doc.node(body).parent, None);
        assert!(doc.node(html).children.is_empty());
    }

    #[test]
    fn text_merging_on_append() {
        let mut doc = Document::new();
        let p = el(&mut doc, "p");
        doc.append_child(doc.root(), p);
        doc.append_text(p, "hello ");
        doc.append_text(p, "world");
        assert_eq!(doc.node(p).children.len(), 1);
        assert_eq!(doc.text_content(p), "hello world");
    }

    #[test]
    fn insert_before_and_text_merge() {
        let mut doc = Document::new();
        let p = el(&mut doc, "p");
        let span = el(&mut doc, "span");
        doc.append_child(doc.root(), p);
        doc.append_text(p, "aa");
        doc.append_child(p, span);
        // span 前插文本,应并入前面的文本节点
        doc.insert_text_before(span, "bb");
        assert_eq!(doc.node(p).children.len(), 2);
        assert_eq!(doc.text_content(p), "aabb");
    }

    #[test]
    fn reparent_children_moves_all() {
        let mut doc = Document::new();
        let a = el(&mut doc, "div");
        let b = el(&mut doc, "div");
        let x = el(&mut doc, "span");
        let y = el(&mut doc, "span");
        doc.append_child(a, x);
        doc.append_child(a, y);
        doc.reparent_children(a, b);
        assert!(doc.node(a).children.is_empty());
        assert_eq!(doc.node(b).children, vec![x, y]);
        assert_eq!(doc.node(x).parent, Some(b));
    }

    #[test]
    fn descendants_preorder() {
        let mut doc = Document::new();
        let html = el(&mut doc, "html");
        let head = el(&mut doc, "head");
        let body = el(&mut doc, "body");
        let p = el(&mut doc, "p");
        doc.append_child(doc.root(), html);
        doc.append_child(html, head);
        doc.append_child(html, body);
        doc.append_child(body, p);

        let order: Vec<NodeId> = doc.descendants(doc.root()).collect();
        assert_eq!(order, vec![doc.root(), html, head, body, p]);
    }
}
