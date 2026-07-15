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
        op!("documentElement", move || opt_fid(
            dom.borrow().document_element()
        ));
    }
    {
        let dom = dom.clone();
        op!("body", move || {
            let doc = dom.borrow();
            opt_fid(find_html_child(&doc, "body"))
        });
    }
    {
        let dom = dom.clone();
        op!("head", move || {
            let doc = dom.borrow();
            opt_fid(find_html_child(&doc, "head"))
        });
    }
    {
        let dom = dom.clone();
        op!("getElementById", move |target: String| {
            let doc = dom.borrow();
            let found = doc.descendants(doc.root()).find(|&n| {
                doc.element(n)
                    .is_some_and(|el| el.attr("id") == Some(target.as_str()))
            });
            opt_fid(found)
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
        op!("createElementNS", move |ns_url: String, tag: String| {
            let ns = surl_dom::Namespace::from(ns_url);
            // 非 HTML 命名空间保留大小写(SVG 的 viewBox 等)
            let name = QualName::new(None, ns, LocalName::from(tag));
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
                NodeData::Element(el) => el.local_name().as_ref().to_ascii_uppercase(),
                NodeData::Text { .. } => "#text".into(),
                NodeData::Comment { .. } => "#comment".into(),
                NodeData::Document => "#document".into(),
                NodeData::Fragment => "#document-fragment".into(),
                NodeData::Doctype { name } => name.clone(),
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
                    if is_fragment {
                        doc.reparent_children(node, parent);
                    } else {
                        doc.append_child(parent, node);
                    }
                    return Ok(());
                }
                let reference = nid(&ctx, &doc, reference)?;
                if doc.node(reference).parent != Some(parent) {
                    return Err(Exception::throw_type(
                        &ctx,
                        "NotFoundError: reference node is not a child of parent",
                    ));
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
                Some(el) => Ok(el.local_name().as_ref().to_ascii_uppercase()),
                None => Err(Exception::throw_type(&ctx, "tagName on non-element")),
            }
        });
    }

    // ---- 文本 ----
    {
        let dom = dom.clone();
        op!("textContent", move |ctx: Ctx<'_>, id: f64| -> Result<String> {
            let doc = dom.borrow();
            let id = nid(&ctx, &doc, id)?;
            Ok(doc.text_content(id))
        });
    }
    {
        let dom = dom.clone();
        op!(
            "setTextContent",
            move |ctx: Ctx<'_>, id: f64, text: String| -> Result<()> {
                let mut doc = dom.borrow_mut();
                let id = nid(&ctx, &doc, id)?;
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

fn find_html_child(doc: &Document, tag: &str) -> Option<NodeId> {
    let html = doc.document_element()?;
    doc.node(html)
        .children
        .iter()
        .copied()
        .find(|&c| doc.element(c).is_some_and(|el| el.is_html_element(tag)))
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

/// 防环:child 不得是 parent 的祖先(或其自身)。
fn check_hierarchy(ctx: &Ctx<'_>, doc: &Document, parent: NodeId, child: NodeId) -> Result<()> {
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
