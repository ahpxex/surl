//! surl 的 JS 运行时:QuickJS-NG(经 rquickjs)+ resource-table DOM 绑定。
//!
//! 一个 [`PageRuntime`] 对应一次页面加载:它拥有 DOM(`Rc<RefCell<Document>>`,
//! 也被各原生 op 闭包持有)与一个 QuickJS Context。JS 侧经 bootstrap.js 搭出
//! 的包装类访问 DOM,手里只有数字句柄——没有跨 GC 边界的对象图。

mod ops;

use std::cell::{Ref, RefCell};
use std::rc::Rc;

use rquickjs::{CatchResultExt, CaughtError, Context, Runtime};
use surl_dom::Document;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("js exception: {0}")]
    Js(String),
    #[error("engine error: {0}")]
    Engine(#[from] rquickjs::Error),
}

/// 页面 JS 打的 console 输出,测试与诊断用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleMessage {
    pub level: String,
    pub message: String,
}

pub struct PageRuntime {
    // 字段序即析构序:先 ctx 后 rt;dom 的其余 Rc 引用在 ctx 的闭包里,
    // ctx 一掉引用就回收,take_document 依赖这一点。
    ctx: Context,
    rt: Runtime,
    dom: ops::SharedDom,
    console: ops::ConsoleSink,
}

impl PageRuntime {
    /// 接管一棵文档树,建好 JS 世界(原生 op + bootstrap 的 DOM 包装层)。
    pub fn new(doc: Document) -> Result<Self, RuntimeError> {
        let rt = Runtime::new()?;
        let ctx = Context::full(&rt)?;
        let dom: ops::SharedDom = Rc::new(RefCell::new(doc));
        let console: ops::ConsoleSink = Rc::new(RefCell::new(Vec::new()));

        ctx.with(|ctx| -> Result<(), RuntimeError> {
            ops::install(&ctx, &dom, &console)?;
            eval_caught(&ctx, include_str!("bootstrap.js"), "surl:bootstrap")?;
            Ok(())
        })?;

        Ok(PageRuntime {
            ctx,
            rt,
            dom,
            console,
        })
    }

    /// 执行一段经典 script(非 module)。异常转成 `RuntimeError::Js`。
    pub fn eval(&self, source: &str) -> Result<(), RuntimeError> {
        self.ctx
            .with(|ctx| eval_caught(&ctx, source, "surl:script"))
    }

    /// 手动泵微任务队列直到清空(M2 事件循环的雏形)。
    /// 返回执行的 job 数。
    pub fn pump_jobs(&self) -> Result<usize, RuntimeError> {
        let mut executed = 0;
        while self.rt.is_job_pending() {
            match self.rt.execute_pending_job() {
                Ok(true) => executed += 1,
                Ok(false) => break,
                Err(_job_err) => {
                    // job 内的异常:M1 先吞掉计数,M2 事件循环统一上报
                    executed += 1;
                }
            }
        }
        Ok(executed)
    }

    /// 只读访问当前 DOM(语义提取等)。
    pub fn document(&self) -> Ref<'_, Document> {
        self.dom.borrow()
    }

    /// 拆掉 JS 世界,拿回文档树。
    pub fn take_document(self) -> Document {
        let PageRuntime { ctx, rt, dom, .. } = self;
        drop(ctx);
        drop(rt);
        Rc::try_unwrap(dom)
            .map(RefCell::into_inner)
            .unwrap_or_else(|rc| {
                // 理论上 ctx/rt 掉了就没有其他持有者;防御性兜底
                std::mem::take(&mut *rc.borrow_mut())
            })
    }

    pub fn console_messages(&self) -> Vec<ConsoleMessage> {
        self.console.borrow().clone()
    }
}

/// eval + 把 JS 异常(含 message/stack)转成可读错误。
fn eval_caught(
    ctx: &rquickjs::Ctx<'_>,
    source: &str,
    label: &str,
) -> Result<(), RuntimeError> {
    let result: Result<(), rquickjs::Error> = ctx.eval::<(), _>(source);
    result.catch(ctx).map_err(|caught| match caught {
        CaughtError::Exception(ex) => {
            let msg = ex.message().unwrap_or_else(|| "<no message>".into());
            let stack = ex.stack().map(|s| format!("\n{s}")).unwrap_or_default();
            RuntimeError::Js(format!("{label}: {msg}{stack}"))
        }
        CaughtError::Value(v) => RuntimeError::Js(format!("{label}: threw {v:?}")),
        CaughtError::Error(e) => RuntimeError::Engine(e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use surl_dom::parse_html;

    fn runtime(html: &str) -> PageRuntime {
        PageRuntime::new(parse_html(html)).expect("runtime boots")
    }

    #[test]
    fn boots_and_evaluates() {
        let rt = runtime("<!doctype html><p>x</p>");
        rt.eval("globalThis.__t = 1 + 1").unwrap();
        rt.eval("if (__t !== 2) throw new Error('math broke')").unwrap();
    }

    #[test]
    fn document_globals_exist() {
        let rt = runtime("<!doctype html><title>T</title><p>x</p>");
        rt.eval(
            r#"
            if (typeof document !== "object") throw new Error("no document");
            if (document.nodeType !== 9) throw new Error("bad nodeType");
            if (document.documentElement.tagName !== "HTML") throw new Error("bad html");
            if (document.body.tagName !== "BODY") throw new Error("bad body");
            if (document.head.tagName !== "HEAD") throw new Error("bad head");
            if (window !== globalThis) throw new Error("window not global");
        "#,
        )
        .unwrap();
    }

    #[test]
    fn get_element_by_id_and_text() {
        let rt = runtime(r#"<!doctype html><div id="app">hello <b>world</b></div>"#);
        rt.eval(
            r#"
            const app = document.getElementById("app");
            if (!app) throw new Error("not found");
            if (app.tagName !== "DIV") throw new Error("wrong tag");
            if (app.textContent !== "hello world") throw new Error("wrong text: " + app.textContent);
            if (document.getElementById("nope") !== null) throw new Error("ghost element");
        "#,
        )
        .unwrap();
    }

    #[test]
    fn wrapper_identity_is_stable() {
        let rt = runtime(r#"<!doctype html><div id="a"></div>"#);
        rt.eval(
            r#"
            const one = document.getElementById("a");
            const two = document.getElementById("a");
            if (one !== two) throw new Error("identity broken");
            if (one.parentNode !== document.body) throw new Error("parent identity broken");
        "#,
        )
        .unwrap();
    }

    #[test]
    fn dom_mutation_reaches_rust() {
        let rt = runtime(r#"<!doctype html><div id="root"></div>"#);
        rt.eval(
            r#"
            const root = document.getElementById("root");
            const h = document.createElement("h1");
            h.appendChild(document.createTextNode("built by js"));
            root.appendChild(h);
            const a = document.createElement("a");
            a.setAttribute("href", "/from-js");
            a.textContent = "click";
            root.appendChild(a);
        "#,
        )
        .unwrap();
        let doc = rt.take_document();
        let html = doc.to_html();
        assert!(html.contains("<h1>built by js</h1>"), "{html}");
        assert!(html.contains(r#"<a href="/from-js">click</a>"#), "{html}");
    }

    #[test]
    fn semantic_tree_sees_js_mutations() {
        let rt = runtime(r#"<!doctype html><title>app</title><div id="root"></div>"#);
        rt.eval(
            r#"
            const nav = document.createElement("nav");
            const a = document.createElement("a");
            a.setAttribute("href", "https://discord.gg/test");
            a.textContent = "Join us";
            nav.appendChild(a);
            document.getElementById("root").appendChild(nav);
        "#,
        )
        .unwrap();
        let doc = rt.document();
        let snapshot = surl_core::semantic::extract(&doc, None);
        let tree = snapshot.to_tree_string();
        assert!(
            tree.contains("link \"Join us\" -> https://discord.gg/test"),
            "{tree}"
        );
    }

    #[test]
    fn traversal_and_siblings() {
        let rt = runtime("<!doctype html><ul><li>a</li><li>b</li><li>c</li></ul>");
        rt.eval(
            r#"
            const ul = document.body.firstChild;
            if (ul.tagName !== "UL") throw new Error("expected UL, got " + ul.nodeName);
            const kids = ul.childNodes;
            if (kids.length !== 3) throw new Error("expected 3 children");
            if (kids[0].nextSibling !== kids[1]) throw new Error("nextSibling broken");
            if (kids[2].previousSibling !== kids[1]) throw new Error("previousSibling broken");
            if (ul.firstChild !== kids[0] || ul.lastChild !== kids[2]) throw new Error("ends broken");
            if (kids[0].textContent !== "a") throw new Error("text broken");
            if (!document.body.contains(kids[1])) throw new Error("contains broken");
        "#,
        )
        .unwrap();
    }

    #[test]
    fn insert_before_and_remove_child() {
        let rt = runtime(r#"<!doctype html><div id="x"><span id="s">old</span></div>"#);
        rt.eval(
            r#"
            const x = document.getElementById("x");
            const s = document.getElementById("s");
            const first = document.createElement("em");
            x.insertBefore(first, s);
            x.removeChild(s);
        "#,
        )
        .unwrap();
        let doc = rt.take_document();
        let html = doc.to_html();
        assert!(html.contains(r#"<div id="x"><em></em></div>"#), "{html}");
    }

    #[test]
    fn set_text_content_replaces_subtree() {
        let rt = runtime(r#"<!doctype html><div id="x"><b>old</b><i>stuff</i></div>"#);
        rt.eval(r#"document.getElementById("x").textContent = "fresh";"#)
            .unwrap();
        let doc = rt.take_document();
        assert!(doc.to_html().contains(r#"<div id="x">fresh</div>"#));
    }

    #[test]
    fn attributes_roundtrip() {
        let rt = runtime(r#"<!doctype html><div id="x" data-a="1"></div>"#);
        rt.eval(
            r#"
            const x = document.getElementById("x");
            if (x.getAttribute("data-a") !== "1") throw new Error("read broken");
            if (!x.hasAttribute("data-a")) throw new Error("has broken");
            x.setAttribute("data-a", "2");
            if (x.getAttribute("data-a") !== "2") throw new Error("update broken");
            x.removeAttribute("data-a");
            if (x.getAttribute("data-a") !== null) throw new Error("remove broken");
            x.className = "big red";
            if (x.className !== "big red") throw new Error("className broken");
            x.id = "y";
        "#,
        )
        .unwrap();
        let doc = rt.take_document();
        assert!(doc.to_html().contains(r#"id="y""#));
    }

    #[test]
    fn hierarchy_cycle_rejected() {
        let rt = runtime(r#"<!doctype html><div id="a"><div id="b"></div></div>"#);
        let err = rt
            .eval(
                r#"
                const a = document.getElementById("a");
                const b = document.getElementById("b");
                b.appendChild(a);
            "#,
            )
            .unwrap_err();
        assert!(err.to_string().contains("HierarchyRequestError"), "{err}");
    }

    #[test]
    fn js_exception_surfaces_with_message() {
        let rt = runtime("<!doctype html>");
        let err = rt.eval("throw new Error('boom')").unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[test]
    fn console_is_captured() {
        let rt = runtime("<!doctype html>");
        rt.eval(r#"console.log("hi", 42, {a: 1}); console.error("bad");"#)
            .unwrap();
        let msgs = rt.console_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].level, "log");
        assert_eq!(msgs[0].message, r#"hi 42 {"a":1}"#);
        assert_eq!(msgs[1].level, "error");
    }

    #[test]
    fn microtask_pump_runs_promise_jobs() {
        let rt = runtime("<!doctype html>");
        rt.eval("globalThis.done = false; Promise.resolve().then(() => { globalThis.done = true; });")
            .unwrap();
        rt.eval("if (globalThis.done) throw new Error('microtask ran too early')")
            .unwrap();
        let executed = rt.pump_jobs().unwrap();
        assert!(executed >= 1);
        rt.eval("if (!globalThis.done) throw new Error('microtask never ran')")
            .unwrap();
    }
}
