//! html5ever → arena 的桥:直接给 `Document` 实现 TreeSink,不经过 RcDom。
//!
//! TreeSink 的方法全是 `&self`(0.29+ 改为内部可变性),所以用 RefCell 包一层;
//! 解析器是单线程同步跑完的,borrow 不会重入。

use std::borrow::Cow;
use std::cell::RefCell;

use html5ever::interface::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{Attribute, LocalName, Namespace, ParseOpts, QualName};

use crate::tree::{Attr, Document, ElementData, NodeData, NodeId};

/// 解析一段完整 HTML 文档。
pub fn parse_html(html: &str) -> Document {
    html5ever::parse_document(Sink::new(), ParseOpts::default()).one(StrTendril::from(html))
}

/// 片段解析:在 `context` 标签的语境下解析(innerHTML 语义)。
/// 返回文档与承载结果子节点的 Fragment 根。
pub fn parse_fragment(html: &str, context: &str) -> (Document, NodeId) {
    let ctx_name = QualName::new(None, html5ever::ns!(html), LocalName::from(context));
    let parser = html5ever::parse_fragment(
        Sink::new(),
        ParseOpts::default(),
        ctx_name,
        Vec::new(),
        false,
    );
    let doc = parser.one(StrTendril::from(html));
    // html5ever 片段解析的产物挂在 #document 下的第一个 <html> 元素里,
    // 把它的孩子搬到一个 Fragment 上,呈现 innerHTML 语义。
    let mut doc = doc;
    let fragment = doc.create_node(NodeData::Fragment);
    if let Some(html_el) = doc.document_element() {
        doc.reparent_children(html_el, fragment);
    }
    (doc, fragment)
}

impl Document {
    /// innerHTML setter:在 target 元素的语境下重入解析 html,替换其全部孩子。
    pub fn set_inner_html(&mut self, target: NodeId, html: &str) {
        let context = match self.element(target) {
            Some(el) => el.local_name().to_string(),
            None => "body".to_owned(),
        };
        let (frag_doc, frag_root) = parse_fragment(html, &context);
        let old = self.node(target).children.clone();
        for child in old {
            self.detach(child);
        }
        for &child in &frag_doc.node(frag_root).children {
            let imported = self.import_subtree(&frag_doc, child);
            self.append_child(target, imported);
        }
    }
}

pub struct Sink {
    doc: RefCell<Document>,
}

impl Sink {
    pub fn new() -> Self {
        Sink {
            doc: RefCell::new(Document::new()),
        }
    }
}

impl Default for Sink {
    fn default() -> Self {
        Self::new()
    }
}

/// TreeSink 要求返回可读元素名;arena 在 RefCell 里,借不出引用,返回 owned
/// 名字(两个 atom 的引用计数拷贝,廉价)。
pub struct OwnedElemName {
    ns: Namespace,
    local: LocalName,
}

impl ElemName for OwnedElemName {
    fn ns(&self) -> &Namespace {
        &self.ns
    }
    fn local_name(&self) -> &LocalName {
        &self.local
    }
}

impl std::fmt::Debug for OwnedElemName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.ns, self.local)
    }
}

fn convert_attrs(attrs: Vec<Attribute>) -> Vec<Attr> {
    attrs
        .into_iter()
        .map(|a| Attr {
            name: a.name,
            value: a.value.to_string(),
        })
        .collect()
}

impl TreeSink for Sink {
    type Handle = NodeId;
    type Output = Document;
    type ElemName<'a> = OwnedElemName;

    fn finish(self) -> Document {
        self.doc.into_inner()
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {
        // 容错解析是 HTML 的常态,解析“错误”不值得记日志。
    }

    fn get_document(&self) -> NodeId {
        self.doc.borrow().root()
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> OwnedElemName {
        let doc = self.doc.borrow();
        let el = doc.element(*target).expect("elem_name on non-element");
        OwnedElemName {
            ns: el.name.ns.clone(),
            local: el.name.local.clone(),
        }
    }

    fn create_element(
        &self,
        name: QualName,
        attrs: Vec<Attribute>,
        flags: ElementFlags,
    ) -> NodeId {
        let mut doc = self.doc.borrow_mut();
        let template_contents = flags.template.then(|| doc.create_node(NodeData::Fragment));
        doc.create_node(NodeData::Element(ElementData {
            name,
            attrs: convert_attrs(attrs),
            template_contents,
        }))
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.doc.borrow_mut().create_node(NodeData::Comment {
            contents: text.to_string(),
        })
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        self.doc
            .borrow_mut()
            .create_node(NodeData::ProcessingInstruction {
                target: target.to_string(),
                data: data.to_string(),
            })
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        let mut doc = self.doc.borrow_mut();
        match child {
            NodeOrText::AppendNode(n) => doc.append_child(*parent, n),
            NodeOrText::AppendText(t) => doc.append_text(*parent, &t),
        }
    }

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        let mut doc = self.doc.borrow_mut();
        match new_node {
            NodeOrText::AppendNode(n) => doc.insert_before(*sibling, n),
            NodeOrText::AppendText(t) => doc.insert_text_before(*sibling, &t),
        }
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        let has_parent = self.doc.borrow().node(*element).parent.is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let mut doc = self.doc.borrow_mut();
        let root = doc.root();
        let dt = doc.create_node(NodeData::Doctype {
            name: name.to_string(),
            public_id: public_id.to_string(),
            system_id: system_id.to_string(),
        });
        doc.append_child(root, dt);
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        self.doc
            .borrow()
            .element(*target)
            .and_then(|el| el.template_contents)
            .expect("get_template_contents on non-template")
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.doc.borrow_mut().quirks_mode = mode;
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        let mut doc = self.doc.borrow_mut();
        let el = doc
            .element_mut(*target)
            .expect("add_attrs_if_missing on non-element");
        for attr in attrs {
            if !el.attrs.iter().any(|a| a.name == attr.name) {
                el.attrs.push(Attr {
                    name: attr.name,
                    value: attr.value.to_string(),
                });
            }
        }
    }

    fn remove_from_parent(&self, target: &NodeId) {
        self.doc.borrow_mut().detach(*target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        self.doc.borrow_mut().reparent_children(*node, *new_parent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 找到第一个指定标签的元素。
    fn find(doc: &Document, tag: &str) -> Option<NodeId> {
        doc.descendants(doc.root())
            .find(|&n| doc.element(n).is_some_and(|el| el.is_html_element(tag)))
    }

    #[test]
    fn parses_minimal_document() {
        let doc = parse_html("<!doctype html><title>t</title><p>hello</p>");
        let html = doc.document_element().unwrap();
        assert!(doc.element(html).unwrap().is_html_element("html"));
        // 解析器自动补全 head/body
        assert!(find(&doc, "head").is_some());
        assert!(find(&doc, "body").is_some());
        let p = find(&doc, "p").unwrap();
        assert_eq!(doc.text_content(p), "hello");
    }

    #[test]
    fn records_quirks_mode() {
        let no_doctype = parse_html("<p>x</p>");
        assert_eq!(no_doctype.quirks_mode, QuirksMode::Quirks);
        let standards = parse_html("<!doctype html><p>x</p>");
        assert_eq!(standards.quirks_mode, QuirksMode::NoQuirks);
    }

    #[test]
    fn parses_attributes() {
        let doc = parse_html(r#"<!doctype html><a href="/x" class="big">go</a>"#);
        let a = find(&doc, "a").unwrap();
        let el = doc.element(a).unwrap();
        assert_eq!(el.attr("href"), Some("/x"));
        assert_eq!(el.attr("class"), Some("big"));
        assert_eq!(el.attr("nope"), None);
    }

    #[test]
    fn fixes_up_misnested_tags() {
        // 容错:未闭合的 <b> 跨越 </p>,html5ever 应重建树而不是崩
        let doc = parse_html("<!doctype html><p><b>bold<p>still bold");
        let body = find(&doc, "body").unwrap();
        assert_eq!(doc.text_content(body), "boldstill bold");
    }

    #[test]
    fn template_contents_not_in_main_tree() {
        let doc = parse_html("<!doctype html><template><p>inside</p></template>");
        let tpl = find(&doc, "template").unwrap();
        // 主树里 template 没有孩子
        assert!(doc.node(tpl).children.is_empty());
        // 内容活在 template_contents 的 Fragment 里
        let contents = doc.element(tpl).unwrap().template_contents.unwrap();
        assert_eq!(doc.text_content(contents), "inside");
    }

    #[test]
    fn svg_foreign_namespace() {
        let doc = parse_html("<!doctype html><svg><circle r='1'/></svg>");
        let svg = doc
            .descendants(doc.root())
            .find(|&n| {
                doc.element(n)
                    .is_some_and(|el| *el.local_name() == *"svg")
            })
            .unwrap();
        assert_eq!(doc.element(svg).unwrap().name.ns, html5ever::ns!(svg));
    }

    #[test]
    fn fragment_parsing_innerhtml_semantics() {
        let (doc, frag) = parse_fragment("<li>a</li><li>b</li>", "ul");
        let lis: Vec<_> = doc.node(frag).children.clone();
        assert_eq!(lis.len(), 2);
        assert_eq!(doc.text_content(frag), "ab");
    }

    #[test]
    fn set_inner_html_replaces_children() {
        let mut doc = parse_html(r#"<!doctype html><div id="x"><p>old</p></div>"#);
        let x = doc.query_selector(doc.root(), "#x").unwrap().unwrap();
        doc.set_inner_html(x, "<em>new</em> text <b>bold</b>");
        assert_eq!(
            doc.serialize_subtree(x),
            r#"<div id="x"><em>new</em> text <b>bold</b></div>"#
        );
        assert_eq!(doc.inner_html(x), "<em>new</em> text <b>bold</b>");
    }

    #[test]
    fn set_inner_html_is_context_aware() {
        // <tr> 只能在 table 语境下解析出来
        let mut doc = parse_html("<!doctype html><table><tbody id=b></tbody></table>");
        let b = doc.query_selector(doc.root(), "#b").unwrap().unwrap();
        doc.set_inner_html(b, "<tr><td>cell</td></tr>");
        assert!(doc.inner_html(b).contains("<tr><td>cell</td></tr>"));
    }

    #[test]
    fn clone_subtree_deep_and_detached() {
        let mut doc = parse_html(r#"<!doctype html><div id="a"><span class="s">x</span></div>"#);
        let a = doc.query_selector(doc.root(), "#a").unwrap().unwrap();
        let copy = doc.clone_subtree(a);
        assert_eq!(doc.node(copy).parent, None);
        assert_eq!(
            doc.serialize_subtree(copy),
            r#"<div id="a"><span class="s">x</span></div>"#
        );
        // 拷贝是独立的:改原树不影响副本
        let span = doc.query_selector(a, "span").unwrap().unwrap();
        doc.detach(span);
        assert!(doc.serialize_subtree(copy).contains("span"));
    }
}
