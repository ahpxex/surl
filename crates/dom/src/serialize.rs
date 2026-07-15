//! DOM → HTML 字符串,走 html5ever 的标准 HtmlSerializer(转义规则、void
//! 元素等都由它处理)。`--dom` 输出模式的实现。

use std::io;

use html5ever::serialize::{Serialize, SerializeOpts, Serializer, TraversalScope};

use crate::tree::{Document, NodeData, NodeId};

/// 把某个节点(含子树)借给 html5ever 的序列化器。
struct SerializableNode<'a> {
    doc: &'a Document,
    id: NodeId,
}

impl SerializableNode<'_> {
    fn serialize_node<S: Serializer>(&self, id: NodeId, serializer: &mut S) -> io::Result<()> {
        let node = self.doc.node(id);
        match &node.data {
            NodeData::Document | NodeData::Fragment => {
                for &child in &node.children {
                    self.serialize_node(child, serializer)?;
                }
            }
            NodeData::Doctype { name } => serializer.write_doctype(name)?,
            NodeData::Text { contents } => serializer.write_text(contents)?,
            NodeData::Comment { contents } => serializer.write_comment(contents)?,
            NodeData::ProcessingInstruction { target, data } => {
                serializer.write_processing_instruction(target, data)?
            }
            NodeData::Element(el) => {
                serializer.start_elem(
                    el.name.clone(),
                    el.attrs.iter().map(|a| (&a.name, a.value.as_str())),
                )?;
                // template 序列化其 contents,与浏览器 outerHTML 行为一致
                let children: &[NodeId] = match el.template_contents {
                    Some(contents) => &self.doc.node(contents).children,
                    None => &node.children,
                };
                for &child in children {
                    self.serialize_node(child, serializer)?;
                }
                serializer.end_elem(el.name.clone())?;
            }
        }
        Ok(())
    }
}

impl Serialize for SerializableNode<'_> {
    fn serialize<S: Serializer>(
        &self,
        serializer: &mut S,
        traversal_scope: TraversalScope,
    ) -> io::Result<()> {
        match traversal_scope {
            TraversalScope::IncludeNode => self.serialize_node(self.id, serializer),
            TraversalScope::ChildrenOnly(_) => {
                for &child in &self.doc.node(self.id).children {
                    self.serialize_node(child, serializer)?;
                }
                Ok(())
            }
        }
    }
}

impl Document {
    /// 整个文档序列化为 HTML(doctype + 树)。
    pub fn to_html(&self) -> String {
        self.serialize_subtree(self.root())
    }

    /// innerHTML getter:只序列化孩子,不含节点自身。
    pub fn inner_html(&self, id: NodeId) -> String {
        let mut buf = Vec::new();
        html5ever::serialize(
            &mut buf,
            &SerializableNode { doc: self, id },
            SerializeOpts {
                traversal_scope: TraversalScope::ChildrenOnly(None),
                ..Default::default()
            },
        )
        .expect("serialize to Vec cannot fail");
        String::from_utf8(buf).expect("serializer emits valid utf-8")
    }

    /// 序列化某节点。元素节点含自身(outerHTML 语义),容器根只序列化孩子。
    pub fn serialize_subtree(&self, id: NodeId) -> String {
        let traversal_scope = match self.node(id).data {
            NodeData::Document | NodeData::Fragment => TraversalScope::ChildrenOnly(None),
            _ => TraversalScope::IncludeNode,
        };
        let mut buf = Vec::new();
        html5ever::serialize(
            &mut buf,
            &SerializableNode { doc: self, id },
            SerializeOpts {
                traversal_scope,
                ..Default::default()
            },
        )
        .expect("serialize to Vec cannot fail");
        String::from_utf8(buf).expect("serializer emits valid utf-8")
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::parse_html;

    #[test]
    fn roundtrip_basic() {
        let doc = parse_html("<!doctype html><p class=\"a\">hi &amp; bye</p>");
        let html = doc.to_html();
        assert_eq!(
            html,
            "<!DOCTYPE html><html><head></head><body><p class=\"a\">hi &amp; bye</p></body></html>"
        );
    }

    #[test]
    fn void_elements_not_closed() {
        let doc = parse_html("<!doctype html><br><img src='x'>");
        let html = doc.to_html();
        assert!(html.contains("<br>"));
        assert!(html.contains("<img src=\"x\">"));
        assert!(!html.contains("</br>"));
        assert!(!html.contains("</img>"));
    }

    #[test]
    fn escapes_attr_and_text() {
        let doc = parse_html(r#"<!doctype html><a title='a"b'>x < y</a>"#);
        let html = doc.to_html();
        assert!(html.contains(r#"title="a&quot;b""#));
        assert!(html.contains("x &lt; y"));
    }

    #[test]
    fn script_content_not_escaped() {
        let doc = parse_html("<!doctype html><script>if (a < b) {}</script>");
        let html = doc.to_html();
        assert!(html.contains("<script>if (a < b) {}</script>"));
    }

    #[test]
    fn template_serializes_contents() {
        let doc = parse_html("<!doctype html><template><p>in</p></template>");
        let html = doc.to_html();
        assert!(html.contains("<template><p>in</p></template>"));
    }
}
