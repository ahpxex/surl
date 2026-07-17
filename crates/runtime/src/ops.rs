//! 原生 op 层:JS 世界能触到的全部 Rust 入口。
//!
//! 约定(resource-table 模式):
//! - 跨边界只传标量——节点是 f64 的句柄(NodeId 的 ffi 表示),0 表示 null;
//! - 这里不造 JS 对象、不持 JS 引用;DOM 的类层次由 bootstrap.js 在 JS 侧搭;
//! - 每个 op 短小、无重入(不回调 JS),borrow_mut 不会撞上自己。

use std::cell::RefCell;
use std::rc::Rc;

use rquickjs::{Ctx, Exception, Function, Object, Result};
use surl_dom::{Document, LocalName, NodeData, NodeId, QualName, ns};

use crate::ConsoleMessage;
use crate::event_loop::{EventLoopState, TimerId};
use crate::net::HttpRequest;

pub type SharedDom = Rc<RefCell<Document>>;
pub type ConsoleSink = Rc<RefCell<Vec<ConsoleMessage>>>;
pub type SharedEventLoop = Rc<RefCell<EventLoopState>>;

const NULL_ID: f64 = 0.0;

fn fid(id: NodeId) -> f64 {
    id.to_ffi() as f64
}

fn opt_fid(id: Option<NodeId>) -> f64 {
    id.map_or(NULL_ID, fid)
}

/// 校验句柄仍指向活节点,否则抛 JS 异常。
fn nid(ctx: &Ctx<'_>, doc: &Document, raw: f64) -> Result<NodeId> {
    let id = NodeId::from_ffi(raw as u64);
    if raw == NULL_ID || !doc.contains(id) {
        return Err(Exception::throw_type(ctx, "stale or null node handle"));
    }
    Ok(id)
}

/// 把全部原生 op 注册到 `__surl_dom` 命名空间。
pub fn install(
    ctx: &Ctx<'_>,
    dom: &SharedDom,
    console: &ConsoleSink,
    event_loop: &SharedEventLoop,
    base_url: Option<&str>,
) -> Result<()> {
    let obj = Object::new(ctx.clone())?;

    macro_rules! op {
        ($name:literal, $f:expr) => {
            obj.set($name, Function::new(ctx.clone(), $f)?)?;
        };
    }

    // ---- 文档级 ----
    {
        let dom = dom.clone();
        op!("root", move || fid(dom.borrow().root()));
    }
    {
        let dom = dom.clone();
        op!("documentElement", move |ctx: Ctx<'_>, doc_id: f64| -> Result<f64> {
            let doc = dom.borrow();
            let doc_id = nid(&ctx, &doc, doc_id)?;
            Ok(opt_fid(first_element_child(&doc, doc_id)))
        });
    }
    {
        let dom = dom.clone();
        op!("body", move |ctx: Ctx<'_>, doc_id: f64| -> Result<f64> {
            let doc = dom.borrow();
            let doc_id = nid(&ctx, &doc, doc_id)?;
            Ok(opt_fid(find_html_child(&doc, doc_id, "body")))
        });
    }
    {
        let dom = dom.clone();
        op!("head", move |ctx: Ctx<'_>, doc_id: f64| -> Result<f64> {
            let doc = dom.borrow();
            let doc_id = nid(&ctx, &doc, doc_id)?;
            Ok(opt_fid(find_html_child(&doc, doc_id, "head")))
        });
    }
    {
        let dom = dom.clone();
        op!(
            "getElementById",
            move |ctx: Ctx<'_>, doc_id: f64, target: String| -> Result<f64> {
                let doc = dom.borrow();
                let doc_id = nid(&ctx, &doc, doc_id)?;
                let found = doc.descendants(doc_id).find(|&n| {
                    doc.element(n)
                        .is_some_and(|el| el.attr("id") == Some(target.as_str()))
                });
                Ok(opt_fid(found))
            }
        );
    }
    {
        let dom = dom.clone();
        // 同一 arena 里再造一棵文档树:多 document 支持的全部秘密
        op!("createHTMLDocument", move |title: String| {
            let mut guard = dom.borrow_mut();
            let doc = &mut *guard;
            let root = doc.create_node(NodeData::Document);
            let dt = doc.create_node(NodeData::Doctype {
                name: "html".into(),
                public_id: String::new(),
                system_id: String::new(),
            });
            doc.append_child(root, dt);
            let html = create_html_element(doc, "html");
            doc.append_child(root, html);
            let head = create_html_element(doc, "head");
            doc.append_child(html, head);
            let title_el = create_html_element(doc, "title");
            doc.append_child(head, title_el);
            if !title.is_empty() {
                doc.append_text(title_el, &title);
            }
            let body = create_html_element(doc, "body");
            doc.append_child(html, body);
            fid(root)
        });
    }
    {
        let dom = dom.clone();
        // 节点所在树的根若是 Document 节点则返回之(ownerDocument 的近似)
        op!("rootDocument", move |ctx: Ctx<'_>, id: f64| -> Result<f64> {
            let doc = dom.borrow();
            let mut cursor = nid(&ctx, &doc, id)?;
            while let Some(parent) = doc.node(cursor).parent {
                cursor = parent;
            }
            Ok(if matches!(doc.node(cursor).data, NodeData::Document) {
                fid(cursor)
            } else {
                NULL_ID
            })
        });
    }

    // ---- 节点构造 ----
    {
        let dom = dom.clone();
        op!("createElement", move |tag: String| {
            let name = QualName::new(None, ns!(html), LocalName::from(tag.to_ascii_lowercase()));
            fid(dom
                .borrow_mut()
                .create_node(NodeData::Element(surl_dom::ElementData {
                    name,
                    attrs: Vec::new(),
                    template_contents: None,
                })))
        });
    }
    {
        let dom = dom.clone();
        op!("createText", move |text: String| {
            fid(dom
                .borrow_mut()
                .create_node(NodeData::Text { contents: text }))
        });
    }
    {
        let dom = dom.clone();
        op!("createComment", move |text: String| {
            fid(dom
                .borrow_mut()
                .create_node(NodeData::Comment { contents: text }))
        });
    }
    {
        let dom = dom.clone();
        op!("createFragment", move || {
            fid(dom.borrow_mut().create_node(NodeData::Fragment))
        });
    }
    {
        let dom = dom.clone();
        op!("createPI", move |target: String, data: String| {
            fid(dom
                .borrow_mut()
                .create_node(NodeData::ProcessingInstruction { target, data }))
        });
    }
    {
        let dom = dom.clone();
        op!("doctypeMeta", move |ctx: Ctx<'_>, id: f64| -> Result<Vec<String>> {
            let doc = dom.borrow();
            Ok(match &doc.node(nid(&ctx, &doc, id)?).data {
                NodeData::Doctype { public_id, system_id, .. } => {
                    vec![public_id.clone(), system_id.clone()]
                }
                _ => vec![String::new(), String::new()],
            })
        });
    }
    {
        let dom = dom.clone();
        op!("createDoctype", move |name: String, public_id: String, system_id: String| {
            fid(dom.borrow_mut().create_node(NodeData::Doctype {
                name,
                public_id,
                system_id,
            }))
        });
    }
    {
        let dom = dom.clone();
        // template 的 content fragment:惰性创建(createElement 造的模板一开始没有)
        op!("templateContent", move |ctx: Ctx<'_>, id: f64| -> Result<f64> {
            let mut doc = dom.borrow_mut();
            let id = nid(&ctx, &doc, id)?;
            let existing = doc.element(id).and_then(|el| el.template_contents);
            let frag = match existing {
                Some(f) => f,
                None => {
                    let f = doc.create_node(surl_dom::NodeData::Fragment);
                    let Some(el) = doc.element_mut(id) else {
                        return Err(Exception::throw_type(&ctx, "templateContent on non-element"));
                    };
                    el.template_contents = Some(f);
                    f
                }
            };
            Ok(fid(frag))
        });
    }
    {
        let dom = dom.clone();
        op!("createBareDocument", move || {
            fid(dom.borrow_mut().create_node(NodeData::Document))
        });
    }
    {
        let dom = dom.clone();
        op!("createElementNS", move |ns_url: String, tag: String| {
            let ns = surl_dom::Namespace::from(ns_url);
            // 限定名拆前缀;保留大小写(SVG 的 viewBox、XML 等)
            let (prefix, local) = match tag.split_once(':') {
                Some((p, l)) => (Some(html5ever_prefix(p)), l.to_owned()),
                None => (None, tag),
            };
            let name = QualName::new(prefix, ns, LocalName::from(local));
            fid(dom
                .borrow_mut()
                .create_node(NodeData::Element(surl_dom::ElementData {
                    name,
                    attrs: Vec::new(),
                    template_contents: None,
                })))
        });
    }
    {
        let dom = dom.clone();
        op!(
            "cloneNode",
            move |ctx: Ctx<'_>, id: f64, deep: bool| -> Result<f64> {
                let mut doc = dom.borrow_mut();
                let id = nid(&ctx, &doc, id)?;
                if deep {
                    return Ok(fid(doc.clone_subtree(id)));
                }
                let shallow = {
                    let children_backup: Vec<NodeId> = doc.node(id).children.clone();
                    let cloned = doc.clone_subtree(id);
                    // 浅拷贝:把深拷贝的孩子拆掉(实现简单,页面脚本不在热路径)
                    let _ = children_backup;
                    let cloned_children: Vec<NodeId> = doc.node(cloned).children.clone();
                    for c in cloned_children {
                        doc.detach(c);
                    }
                    cloned
                };
                Ok(fid(shallow))
            }
        );
    }

    // ---- 结构读取 ----
    {
        let dom = dom.clone();
        op!("parent", move |ctx: Ctx<'_>, id: f64| -> Result<f64> {
            let doc = dom.borrow();
            Ok(opt_fid(doc.node(nid(&ctx, &doc, id)?).parent))
        });
    }
    {
        let dom = dom.clone();
        op!("childNodes", move |ctx: Ctx<'_>, id: f64| -> Result<Vec<f64>> {
            let doc = dom.borrow();
            let id = nid(&ctx, &doc, id)?;
            Ok(doc.node(id).children.iter().map(|&c| fid(c)).collect())
        });
    }
    {
        let dom = dom.clone();
        op!("firstChild", move |ctx: Ctx<'_>, id: f64| -> Result<f64> {
            let doc = dom.borrow();
            let id = nid(&ctx, &doc, id)?;
            Ok(opt_fid(doc.node(id).children.first().copied()))
        });
    }
    {
        let dom = dom.clone();
        op!("lastChild", move |ctx: Ctx<'_>, id: f64| -> Result<f64> {
            let doc = dom.borrow();
            let id = nid(&ctx, &doc, id)?;
            Ok(opt_fid(doc.node(id).children.last().copied()))
        });
    }
    {
        let dom = dom.clone();
        op!("nextSibling", move |ctx: Ctx<'_>, id: f64| -> Result<f64> {
            let doc = dom.borrow();
            Ok(opt_fid(sibling(&doc, nid(&ctx, &doc, id)?, 1)))
        });
    }
    {
        let dom = dom.clone();
        op!("prevSibling", move |ctx: Ctx<'_>, id: f64| -> Result<f64> {
            let doc = dom.borrow();
            Ok(opt_fid(sibling(&doc, nid(&ctx, &doc, id)?, -1)))
        });
    }
    {
        let dom = dom.clone();
        op!("nodeType", move |ctx: Ctx<'_>, id: f64| -> Result<i32> {
            let doc = dom.borrow();
            Ok(match doc.node(nid(&ctx, &doc, id)?).data {
                NodeData::Element(_) => 1,
                NodeData::Text { .. } => 3,
                NodeData::ProcessingInstruction { .. } => 7,
                NodeData::Comment { .. } => 8,
                NodeData::Document => 9,
                NodeData::Doctype { .. } => 10,
                NodeData::Fragment => 11,
            })
        });
    }
    {
        let dom = dom.clone();
        op!("nodeName", move |ctx: Ctx<'_>, id: f64| -> Result<String> {
            let doc = dom.borrow();
            Ok(match &doc.node(nid(&ctx, &doc, id)?).data {
                NodeData::Element(el) => element_tag_name(el),
                NodeData::Text { .. } => "#text".into(),
                NodeData::Comment { .. } => "#comment".into(),
                NodeData::Document => "#document".into(),
                NodeData::Fragment => "#document-fragment".into(),
                NodeData::Doctype { name, .. } => name.clone(),
                NodeData::ProcessingInstruction { target, .. } => target.clone(),
            })
        });
    }

    // ---- 结构修改 ----
    {
        let dom = dom.clone();
        op!(
            "appendChild",
            move |ctx: Ctx<'_>, parent: f64, child: f64| -> Result<()> {
                let mut doc = dom.borrow_mut();
                let parent = nid(&ctx, &doc, parent)?;
                let child = nid(&ctx, &doc, child)?;
                check_hierarchy(&ctx, &doc, parent, child)?;
                check_document_constraints(&ctx, &doc, parent, child, None)?;
                // DocumentFragment:移动其孩子,fragment 本身留空(DOM 语义)
                if matches!(doc.node(child).data, NodeData::Fragment) {
                    doc.reparent_children(child, parent);
                } else {
                    doc.append_child(parent, child);
                }
                Ok(())
            }
        );
    }
    {
        let dom = dom.clone();
        op!(
            "insertBefore",
            move |ctx: Ctx<'_>, parent: f64, node: f64, reference: f64| -> Result<()> {
                let mut doc = dom.borrow_mut();
                let parent = nid(&ctx, &doc, parent)?;
                let node = nid(&ctx, &doc, node)?;
                check_hierarchy(&ctx, &doc, parent, node)?;
                let is_fragment = matches!(doc.node(node).data, NodeData::Fragment);
                if reference == NULL_ID {
                    check_document_constraints(&ctx, &doc, parent, node, None)?;
                    if is_fragment {
                        doc.reparent_children(node, parent);
                    } else {
                        doc.append_child(parent, node);
                    }
                    return Ok(());
                }
                let mut reference = nid(&ctx, &doc, reference)?;
                if doc.node(reference).parent != Some(parent) {
                    return Err(Exception::throw_type(
                        &ctx,
                        "NotFoundError: reference node is not a child of parent",
                    ));
                }
                check_document_constraints(&ctx, &doc, parent, node, Some(reference))?;
                // 规范:reference 就是 node 本身时,以 node 的下一个兄弟为参照
                if reference == node {
                    match sibling(&doc, node, 1) {
                        Some(next) => reference = next,
                        None => {
                            doc.append_child(parent, node);
                            return Ok(());
                        }
                    }
                }
                if is_fragment {
                    let children: Vec<NodeId> = doc.node(node).children.clone();
                    for c in children {
                        doc.detach(c);
                        doc.insert_before(reference, c);
                    }
                } else {
                    doc.insert_before(reference, node);
                }
                Ok(())
            }
        );
    }
    {
        let dom = dom.clone();
        op!(
            "removeChild",
            move |ctx: Ctx<'_>, parent: f64, child: f64| -> Result<()> {
                let mut doc = dom.borrow_mut();
                let parent = nid(&ctx, &doc, parent)?;
                let child = nid(&ctx, &doc, child)?;
                if doc.node(child).parent != Some(parent) {
                    return Err(Exception::throw_type(
                        &ctx,
                        "NotFoundError: node is not a child of parent",
                    ));
                }
                doc.detach(child);
                Ok(())
            }
        );
    }

    // ---- 属性 ----
    {
        let dom = dom.clone();
        op!(
            "getAttribute",
            move |ctx: Ctx<'_>, id: f64, name: String| -> Result<Option<String>> {
                let doc = dom.borrow();
                let id = nid(&ctx, &doc, id)?;
                Ok(doc
                    .element(id)
                    .and_then(|el| el.attr(&name.to_ascii_lowercase()))
                    .map(str::to_owned))
            }
        );
    }
    {
        let dom = dom.clone();
        op!(
            "setAttribute",
            move |ctx: Ctx<'_>, id: f64, name: String, value: String| -> Result<()> {
                let mut doc = dom.borrow_mut();
                let id = nid(&ctx, &doc, id)?;
                let Some(el) = doc.element_mut(id) else {
                    return Err(Exception::throw_type(&ctx, "setAttribute on non-element"));
                };
                let name = name.to_ascii_lowercase();
                if let Some(attr) = el
                    .attrs
                    .iter_mut()
                    .find(|a| a.name.ns.is_empty() && *a.name.local == *name)
                {
                    attr.value = value;
                } else {
                    el.attrs.push(surl_dom::Attr {
                        name: QualName::new(None, ns!(), LocalName::from(name)),
                        value,
                    });
                }
                Ok(())
            }
        );
    }
    {
        let dom = dom.clone();
        op!(
            "removeAttribute",
            move |ctx: Ctx<'_>, id: f64, name: String| -> Result<()> {
                let mut doc = dom.borrow_mut();
                let id = nid(&ctx, &doc, id)?;
                if let Some(el) = doc.element_mut(id) {
                    let name = name.to_ascii_lowercase();
                    el.attrs
                        .retain(|a| !(a.name.ns.is_empty() && *a.name.local == *name));
                }
                Ok(())
            }
        );
    }
    {
        let dom = dom.clone();
        op!(
            "hasAttribute",
            move |ctx: Ctx<'_>, id: f64, name: String| -> Result<bool> {
                let doc = dom.borrow();
                let id = nid(&ctx, &doc, id)?;
                Ok(doc
                    .element(id)
                    .is_some_and(|el| el.attr(&name.to_ascii_lowercase()).is_some()))
            }
        );
    }
    {
        let dom = dom.clone();
        op!("tagName", move |ctx: Ctx<'_>, id: f64| -> Result<String> {
            let doc = dom.borrow();
            let id = nid(&ctx, &doc, id)?;
            match doc.element(id) {
                Some(el) => Ok(element_tag_name(el)),
                None => Err(Exception::throw_type(&ctx, "tagName on non-element")),
            }
        });
    }

    {
        let dom = dom.clone();
        // [namespaceURI, prefix, localName],空串表示无
        op!("elementMeta", move |ctx: Ctx<'_>, id: f64| -> Result<Vec<String>> {
            let doc = dom.borrow();
            let id = nid(&ctx, &doc, id)?;
            match doc.element(id) {
                Some(el) => Ok(vec![
                    el.name.ns.to_string(),
                    el.name.prefix.as_ref().map(|p| p.to_string()).unwrap_or_default(),
                    el.local_name().to_string(),
                ]),
                None => Err(Exception::throw_type(&ctx, "elementMeta on non-element")),
            }
        });
    }
    {
        let dom = dom.clone();
        // 每行 [namespaceURI, prefix, localName, value]
        op!(
            "attributes",
            move |ctx: Ctx<'_>, id: f64| -> Result<Vec<Vec<String>>> {
                let doc = dom.borrow();
                let id = nid(&ctx, &doc, id)?;
                Ok(doc
                    .element(id)
                    .map(|el| {
                        el.attrs
                            .iter()
                            .map(|a| {
                                vec![
                                    a.name.ns.to_string(),
                                    a.name
                                        .prefix
                                        .as_ref()
                                        .map(|p| p.to_string())
                                        .unwrap_or_default(),
                                    a.name.local.to_string(),
                                    a.value.clone(),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default())
            }
        );
    }

    // ---- 文本 ----
    {
        let dom = dom.clone();
        op!(
            "textContent",
            move |ctx: Ctx<'_>, id: f64| -> Result<Option<String>> {
                let doc = dom.borrow();
                let id = nid(&ctx, &doc, id)?;
                // 规范:CharacterData/PI 是自身 data;Document/DocumentType 是 null
                Ok(match &doc.node(id).data {
                    NodeData::Text { contents } | NodeData::Comment { contents } => {
                        Some(contents.clone())
                    }
                    NodeData::ProcessingInstruction { data, .. } => Some(data.clone()),
                    NodeData::Document | NodeData::Doctype { .. } => None,
                    _ => Some(doc.text_content(id)),
                })
            }
        );
    }
    {
        let dom = dom.clone();
        op!(
            "setTextContent",
            move |ctx: Ctx<'_>, id: f64, text: String| -> Result<()> {
                let mut doc = dom.borrow_mut();
                let id = nid(&ctx, &doc, id)?;
                match &mut doc.node_mut(id).data {
                    // CharacterData:直接改 data
                    NodeData::Text { contents } | NodeData::Comment { contents } => {
                        *contents = text;
                        return Ok(());
                    }
                    NodeData::ProcessingInstruction { data, .. } => {
                        *data = text;
                        return Ok(());
                    }
                    // Document / DocumentType:setter 是 no-op
                    NodeData::Document | NodeData::Doctype { .. } => return Ok(()),
                    _ => {}
                }
                let children = doc.node(id).children.clone();
                for child in children {
                    doc.detach(child);
                }
                if !text.is_empty() {
                    let t = doc.create_node(NodeData::Text { contents: text });
                    doc.append_child(id, t);
                }
                Ok(())
            }
        );
    }
    {
        let dom = dom.clone();
        op!(
            "nodeValue",
            move |ctx: Ctx<'_>, id: f64| -> Result<Option<String>> {
                let doc = dom.borrow();
                let id = nid(&ctx, &doc, id)?;
                Ok(match &doc.node(id).data {
                    NodeData::Text { contents } | NodeData::Comment { contents } => {
                        Some(contents.clone())
                    }
                    NodeData::ProcessingInstruction { data, .. } => Some(data.clone()),
                    _ => None,
                })
            }
        );
    }
    {
        let dom = dom.clone();
        op!(
            "setNodeValue",
            move |ctx: Ctx<'_>, id: f64, value: String| -> Result<()> {
                let mut doc = dom.borrow_mut();
                let id = nid(&ctx, &doc, id)?;
                match &mut doc.node_mut(id).data {
                    NodeData::Text { contents } | NodeData::Comment { contents } => {
                        *contents = value;
                        Ok(())
                    }
                    _ => Err(Exception::throw_type(&ctx, "setNodeValue on non-CharacterData")),
                }
            }
        );
    }

    // ---- 选择器 ----
    {
        let dom = dom.clone();
        op!(
            "querySelector",
            move |ctx: Ctx<'_>, scope: f64, sel: String| -> Result<f64> {
                let doc = dom.borrow();
                let scope = nid(&ctx, &doc, scope)?;
                match doc.query_selector(scope, &sel) {
                    Ok(hit) => Ok(opt_fid(hit)),
                    Err(e) => Err(Exception::throw_syntax(&ctx, &e.to_string())),
                }
            }
        );
    }
    {
        let dom = dom.clone();
        op!(
            "querySelectorAll",
            move |ctx: Ctx<'_>, scope: f64, sel: String| -> Result<Vec<f64>> {
                let doc = dom.borrow();
                let scope = nid(&ctx, &doc, scope)?;
                match doc.query_selector_all(scope, &sel) {
                    Ok(hits) => Ok(hits.into_iter().map(fid).collect()),
                    Err(e) => Err(Exception::throw_syntax(&ctx, &e.to_string())),
                }
            }
        );
    }
    {
        let dom = dom.clone();
        op!(
            "matches",
            move |ctx: Ctx<'_>, id: f64, sel: String| -> Result<bool> {
                let doc = dom.borrow();
                let id = nid(&ctx, &doc, id)?;
                doc.element_matches(id, &sel)
                    .map_err(|e| Exception::throw_syntax(&ctx, &e.to_string()))
            }
        );
    }

    // ---- innerHTML / outerHTML ----
    {
        let dom = dom.clone();
        op!("innerHTML", move |ctx: Ctx<'_>, id: f64| -> Result<String> {
            let doc = dom.borrow();
            let id = nid(&ctx, &doc, id)?;
            Ok(doc.inner_html(id))
        });
    }
    {
        let dom = dom.clone();
        op!("outerHTML", move |ctx: Ctx<'_>, id: f64| -> Result<String> {
            let doc = dom.borrow();
            let id = nid(&ctx, &doc, id)?;
            Ok(doc.serialize_subtree(id))
        });
    }
    {
        let dom = dom.clone();
        op!(
            "setInnerHTML",
            move |ctx: Ctx<'_>, id: f64, html: String| -> Result<()> {
                let mut doc = dom.borrow_mut();
                let id = nid(&ctx, &doc, id)?;
                doc.set_inner_html(id, &html);
                Ok(())
            }
        );
    }

    // ---- URL 解析(url crate;QuickJS 没有内置 URL)----
    op!("urlResolve", |input: String, base: String| -> String {
        let parsed = if base.is_empty() {
            url::Url::parse(&input)
        } else {
            url::Url::parse(&base).and_then(|b| b.join(&input))
        };
        match parsed {
            Ok(u) => {
                let origin = u.origin();
                let origin_str = match &origin {
                    url::Origin::Opaque(_) => "null".to_owned(),
                    o => o.ascii_serialization(),
                };
                serde_json::json!({
                    "href": u.as_str(),
                    "origin": origin_str,
                    "protocol": format!("{}:", u.scheme()),
                    "hostname": u.host_str().unwrap_or(""),
                    "host": match u.port() {
                        Some(p) => format!("{}:{}", u.host_str().unwrap_or(""), p),
                        None => u.host_str().unwrap_or("").to_owned(),
                    },
                    "port": u.port().map(|p| p.to_string()).unwrap_or_default(),
                    "pathname": u.path(),
                    "search": u.query().map(|q| format!("?{q}")).unwrap_or_default(),
                    "hash": u.fragment().map(|f| format!("#{f}")).unwrap_or_default(),
                })
                .to_string()
            }
            Err(_) => String::new(),
        }
    });

    // ---- 事件循环:定时器 / 虚拟时钟 / fetch 队列 ----
    obj.set("baseUrl", base_url.unwrap_or(""))?;
    {
        let el = event_loop.clone();
        op!("clockNow", move || el.borrow().now_ms as f64);
    }
    {
        let el = event_loop.clone();
        op!("timerSchedule", move |delay_ms: f64, repeating: bool| {
            let delay = if delay_ms.is_finite() && delay_ms > 0.0 {
                delay_ms as u64
            } else {
                0
            };
            el.borrow_mut().schedule_timer(delay, repeating).0 as f64
        });
    }
    {
        let el = event_loop.clone();
        op!("timerClear", move |id: f64| {
            el.borrow_mut().clear_timer(TimerId(id as u32));
        });
    }
    {
        let el = event_loop.clone();
        op!(
            "fetchStart",
            move |url: String,
                  method: String,
                  headers: Vec<Vec<String>>,
                  has_body: bool,
                  body: String| {
                let req = HttpRequest {
                    url,
                    method,
                    headers: headers
                        .into_iter()
                        .filter_map(|kv| {
                            let mut it = kv.into_iter();
                            Some((it.next()?, it.next()?))
                        })
                        .collect(),
                    body: has_body.then_some(body),
                };
                el.borrow_mut().queue_request(req) as f64
            }
        );
    }

    // ---- console ----
    {
        let console = console.clone();
        op!("consoleLog", move |level: String, message: String| {
            match level.as_str() {
                "error" => tracing::error!(target: "surl_js", "{message}"),
                "warn" => tracing::warn!(target: "surl_js", "{message}"),
                _ => tracing::debug!(target: "surl_js", "{message}"),
            }
            console.borrow_mut().push(ConsoleMessage { level, message });
        });
    }

    ctx.globals().set("__surl_dom", obj)
}

/// 规范:tagName 是限定名(prefix:local);HTML 命名空间大写,
/// 外来命名空间(SVG/MathML/自定义)保留原大小写。
fn element_tag_name(el: &surl_dom::ElementData) -> String {
    let qualified = match &el.name.prefix {
        Some(prefix) => format!("{}:{}", prefix, el.local_name()),
        None => el.local_name().to_string(),
    };
    if el.name.ns == ns!(html) {
        qualified.to_ascii_uppercase()
    } else {
        qualified
    }
}

fn first_element_child(doc: &Document, id: NodeId) -> Option<NodeId> {
    doc.node(id)
        .children
        .iter()
        .copied()
        .find(|&c| doc.element(c).is_some())
}

fn html5ever_prefix(p: &str) -> surl_dom::Prefix {
    surl_dom::Prefix::from(p)
}

fn find_html_child(doc: &Document, doc_id: NodeId, tag: &str) -> Option<NodeId> {
    let html = first_element_child(doc, doc_id)?;
    doc.node(html)
        .children
        .iter()
        .copied()
        .find(|&c| doc.element(c).is_some_and(|el| el.is_html_element(tag)))
}

fn create_html_element(doc: &mut Document, tag: &str) -> NodeId {
    let name = QualName::new(None, ns!(html), LocalName::from(tag));
    doc.create_node(NodeData::Element(surl_dom::ElementData {
        name,
        attrs: Vec::new(),
        template_contents: None,
    }))
}

fn sibling(doc: &Document, id: NodeId, offset: isize) -> Option<NodeId> {
    let parent = doc.node(id).parent?;
    let children = &doc.node(parent).children;
    let idx = children.iter().position(|&c| c == id)? as isize + offset;
    if idx < 0 {
        return None;
    }
    children.get(idx as usize).copied()
}

/// 规范的 pre-insertion 校验(错误消息带 DOMException 名前缀,JS 边界会翻译)。
/// 基础段:父/子类型合法性 + 防环。位置相关规则见
/// [`check_document_constraints`](规范要求 NotFound 检查夹在两者之间)。
fn check_hierarchy(ctx: &Ctx<'_>, doc: &Document, parent: NodeId, child: NodeId) -> Result<()> {
    // 父节点必须是 Document / Element / Fragment
    match doc.node(parent).data {
        NodeData::Document | NodeData::Element(_) | NodeData::Fragment => {}
        _ => {
            return Err(Exception::throw_type(
                ctx,
                "HierarchyRequestError: parent is not a Document, Element, or DocumentFragment",
            ));
        }
    }
    // 防环:child 不得是 parent 的祖先(或其自身)
    let mut cursor = Some(parent);
    while let Some(n) = cursor {
        if n == child {
            return Err(Exception::throw_type(
                ctx,
                "HierarchyRequestError: new child is an ancestor of parent",
            ));
        }
        cursor = doc.node(n).parent;
    }
    Ok(())
}

/// Document 的子节点约束(规范 pre-insert 第 6 步):
/// 至多一个 element、至多一个 doctype、element 不得在 doctype 前、
/// doctype 不得在 element 后;fragment 展开后同样受限。
fn check_document_constraints(
    ctx: &Ctx<'_>,
    doc: &Document,
    parent: NodeId,
    child: NodeId,
    reference: Option<NodeId>,
) -> Result<()> {
    // 节点种类合法性(规范把它排在 NotFound 检查之后,所以不在 check_hierarchy 里)
    match doc.node(child).data {
        NodeData::Document => {
            return Err(Exception::throw_type(
                ctx,
                "HierarchyRequestError: a Document cannot be inserted",
            ));
        }
        NodeData::Text { .. } if matches!(doc.node(parent).data, NodeData::Document) => {
            return Err(Exception::throw_type(
                ctx,
                "HierarchyRequestError: a Text node cannot be a child of a Document",
            ));
        }
        NodeData::Doctype { .. } if !matches!(doc.node(parent).data, NodeData::Document) => {
            return Err(Exception::throw_type(
                ctx,
                "HierarchyRequestError: a doctype can only be a child of a Document",
            ));
        }
        _ => {}
    }
    if !matches!(doc.node(parent).data, NodeData::Document) {
        return Ok(());
    }
    let children = &doc.node(parent).children;
    let has_element = children
        .iter()
        .any(|&c| matches!(doc.node(c).data, NodeData::Element(_)));
    let doctype_pos = children
        .iter()
        .position(|&c| matches!(doc.node(c).data, NodeData::Doctype { .. }));
    let element_pos = children
        .iter()
        .position(|&c| matches!(doc.node(c).data, NodeData::Element(_)));
    let ref_pos = reference.and_then(|r| children.iter().position(|&c| c == r));

    let err = |msg: &str| Err(Exception::throw_type(ctx, msg));

    // fragment 展开后的等效子节点类别
    let (inserting_element, inserting_doctype) = match &doc.node(child).data {
        NodeData::Element(_) => (true, false),
        NodeData::Doctype { .. } => (false, true),
        NodeData::Fragment => {
            let frag_children = &doc.node(child).children;
            if frag_children
                .iter()
                .any(|&c| matches!(doc.node(c).data, NodeData::Text { .. }))
            {
                return err(
                    "HierarchyRequestError: fragment with a Text node cannot be inserted into a Document",
                );
            }
            let elements = frag_children
                .iter()
                .filter(|&&c| matches!(doc.node(c).data, NodeData::Element(_)))
                .count();
            if elements > 1 {
                return err(
                    "HierarchyRequestError: fragment with multiple elements cannot be inserted into a Document",
                );
            }
            (elements == 1, false)
        }
        _ => (false, false),
    };

    if inserting_element {
        if has_element {
            return err("HierarchyRequestError: Document already has a document element");
        }
        // element 不得插到 doctype 之前(即 doctype 位于插入点之后)
        if let (Some(dt), Some(rp)) = (doctype_pos, ref_pos)
            && dt >= rp
        {
            return err("HierarchyRequestError: element cannot be inserted before the doctype");
        }
    }
    if inserting_doctype {
        if doctype_pos.is_some() {
            return err("HierarchyRequestError: Document already has a doctype");
        }
        // doctype 不得落在 document element 之后
        match ref_pos {
            Some(rp) => {
                if let Some(ep) = element_pos
                    && ep < rp
                {
                    return err(
                        "HierarchyRequestError: doctype cannot be inserted after the document element",
                    );
                }
            }
            None => {
                if has_element {
                    return err(
                        "HierarchyRequestError: doctype cannot be appended after the document element",
                    );
                }
            }
        }
    }
    Ok(())
}
