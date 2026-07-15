//! surl 的 JS 运行时:QuickJS-NG(经 rquickjs)+ resource-table DOM 绑定。
//!
//! 一个 [`PageRuntime`] 对应一次页面加载:它拥有 DOM(`Rc<RefCell<Document>>`,
//! 也被各原生 op 闭包持有)与一个 QuickJS Context。JS 侧经 bootstrap.js 搭出
//! 的包装类访问 DOM,手里只有数字句柄——没有跨 GC 边界的对象图。

pub mod event_loop;
pub mod modules;
pub mod net;
mod ops;

use std::cell::{Ref, RefCell};
use std::rc::Rc;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use rquickjs::{CatchResultExt, CaughtError, Context, FromJs, Function, Runtime};
use surl_dom::Document;
use thiserror::Error;

use net::{HttpClient, HttpRequest, HttpResult};

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

/// 一次 run_scripts 的执行统计。
#[derive(Debug, Default)]
pub struct ScriptReport {
    pub executed: usize,
    pub skipped_external: usize,
    pub skipped_module: usize,
    pub errors: Vec<String>,
}

/// settle 的预算:防住 setInterval 永动机与无限自我调度。
#[derive(Debug, Clone, Copy)]
pub struct SettleOptions {
    /// 虚拟时间预算(毫秒)。超过它的定时器直接放弃并计数。
    pub max_virtual_time_ms: u64,
    /// 宏任务总数保险丝。
    pub max_tasks: usize,
}

impl Default for SettleOptions {
    fn default() -> Self {
        SettleOptions {
            max_virtual_time_ms: 30_000,
            max_tasks: 100_000,
        }
    }
}

#[derive(Debug, Default)]
pub struct SettleReport {
    /// 本次静止化推进了多少虚拟毫秒
    pub virtual_elapsed_ms: u64,
    pub timers_fired: usize,
    pub fetches: usize,
    /// 超出虚拟时间预算而放弃的定时器数
    pub abandoned_timers: usize,
    pub hit_task_limit: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ModuleReport {
    /// 预取进缓存的模块数
    pub prefetched: usize,
    /// 成功启动评估的入口数
    pub evaluated: usize,
    pub prefetch_errors: Vec<String>,
    /// 评估失败 + 模块 promise 拒绝
    pub errors: Vec<String>,
    /// 运行期动态 import 没命中缓存的 URL
    pub runtime_misses: Vec<String>,
}

#[derive(Debug, Default)]
pub struct LoadReport {
    pub scripts: ScriptReport,
    pub modules: ModuleReport,
    pub settle: SettleReport,
}

enum Script {
    Inline(String),
    External(String),
    /// type=module:src 外链或内联源码
    Module {
        src: Option<String>,
        source: String,
    },
}

/// 按文档序收集 `<script>`:内联 classic 取源码,src/module 只记类型。
fn collect_scripts(doc: &Document) -> Vec<Script> {
    doc.descendants(doc.root())
        .filter(|&n| {
            doc.element(n)
                .is_some_and(|el| el.is_html_element("script"))
        })
        .filter_map(|n| {
            let el = doc.element(n).expect("filtered to elements");
            let kind = el.attr("type").unwrap_or("").trim().to_ascii_lowercase();
            match kind.as_str() {
                "module" => Some(Script::Module {
                    src: el.attr("src").map(str::to_owned),
                    source: doc.text_content(n),
                }),
                "" | "text/javascript" | "application/javascript" => {
                    match el.attr("src") {
                        Some(src) => Some(Script::External(src.to_owned())),
                        None => Some(Script::Inline(doc.text_content(n))),
                    }
                }
                // JSON/模板等非可执行类型
                _ => None,
            }
        })
        .collect()
}

pub struct PageRuntime {
    // 字段序即析构序:先 ctx 后 rt;dom 的其余 Rc 引用在 ctx 的闭包里,
    // ctx 一掉引用就回收,take_document 依赖这一点。
    ctx: Context,
    rt: Runtime,
    dom: ops::SharedDom,
    console: ops::ConsoleSink,
    event_loop: ops::SharedEventLoop,
    base: Option<url::Url>,
    module_sources: modules::SourceCache,
    module_misses: modules::MissLog,
}

impl PageRuntime {
    /// 接管一棵文档树,建好 JS 世界(原生 op + bootstrap 的 DOM 包装层)。
    pub fn new(doc: Document) -> Result<Self, RuntimeError> {
        Self::with_base(doc, None)
    }

    /// 带页面 URL 的构造:location、fetch 相对地址、外链 script 都以它为基准。
    pub fn with_base(doc: Document, base: Option<url::Url>) -> Result<Self, RuntimeError> {
        let rt = Runtime::new()?;
        let ctx = Context::full(&rt)?;
        let dom: ops::SharedDom = Rc::new(RefCell::new(doc));
        let console: ops::ConsoleSink = Rc::new(RefCell::new(Vec::new()));
        let event_loop: ops::SharedEventLoop = Rc::new(RefCell::new(Default::default()));

        ctx.with(|ctx| -> Result<(), RuntimeError> {
            ops::install(
                &ctx,
                &dom,
                &console,
                &event_loop,
                base.as_ref().map(url::Url::as_str),
            )?;
            eval_caught(&ctx, include_str!("bootstrap.js"), "surl:bootstrap")?;
            Ok(())
        })?;

        let module_sources: modules::SourceCache = Rc::new(RefCell::new(Default::default()));
        let module_misses: modules::MissLog = Rc::new(RefCell::new(Vec::new()));
        rt.set_loader(
            modules::UrlResolver,
            modules::CacheLoader {
                sources: module_sources.clone(),
                misses: module_misses.clone(),
            },
        );

        Ok(PageRuntime {
            ctx,
            rt,
            dom,
            console,
            event_loop,
            base,
            module_sources,
            module_misses,
        })
    }

    /// 执行一段经典 script(非 module)。异常转成 `RuntimeError::Js`。
    pub fn eval(&self, source: &str) -> Result<(), RuntimeError> {
        self.ctx
            .with(|ctx| eval_caught(&ctx, source, "surl:script"))
    }

    /// 求值一个表达式,结果转成 String 带回(测试与诊断用)。
    pub fn eval_string(&self, source: &str) -> Result<String, RuntimeError> {
        self.ctx.with(|ctx| {
            let result: Result<String, rquickjs::Error> = ctx.eval(source);
            result
                .catch(&ctx)
                .map_err(|caught| caught_to_error(caught, "surl:eval_string"))
        })
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

    /// 页面加载的同步编排(M1 遗留,测试友好):内联 classic script + 生命周期
    /// 事件。不碰网络、不跑事件循环——完整路径用 [`PageRuntime::load`]。
    pub fn run_scripts(&self) -> Result<ScriptReport, RuntimeError> {
        let mut report = ScriptReport::default();
        let scripts: Vec<Script> = {
            let doc = self.dom.borrow();
            collect_scripts(&doc)
        };
        for script in scripts {
            match script {
                Script::Inline(source) => self.eval_page_script(&source, &mut report),
                Script::External(src) => {
                    tracing::warn!(target: "surl_js", "run_scripts skips external script: {src}");
                    report.skipped_external += 1;
                }
                Script::Module { .. } => {
                    tracing::warn!(target: "surl_js", "run_scripts skips module script (use load)");
                    report.skipped_module += 1;
                }
            }
        }
        self.eval("__surl_fireReady()")?;
        self.pump_jobs()?;
        Ok(report)
    }

    /// 完整页面加载:script(内联 + 外链,文档序)→ DOMContentLoaded →
    /// 事件循环跑到静止 → load 事件 → 再次静止。
    ///
    /// 这就是「settledness 是事实」的实现:微任务空 + 无就绪宏任务 +
    /// 无在途网络 + 剩余定时器超出预算 ⇒ 页面完成。
    pub async fn load(
        &self,
        net: &dyn HttpClient,
        opts: SettleOptions,
    ) -> Result<LoadReport, RuntimeError> {
        let mut report = LoadReport::default();

        let scripts: Vec<Script> = {
            let doc = self.dom.borrow();
            collect_scripts(&doc)
        };
        // 第一遍:classic script(同步阻塞语义,文档序)
        let mut module_scripts: Vec<Script> = Vec::new();
        for script in scripts {
            match script {
                Script::Inline(source) => self.eval_page_script(&source, &mut report.scripts),
                Script::External(src) => {
                    // classic 外链脚本是阻塞语义:取回来立刻按序执行
                    let resolved = self.resolve_url(&src);
                    let request = HttpRequest {
                        url: resolved.clone(),
                        method: "GET".into(),
                        headers: Vec::new(),
                        body: None,
                    };
                    match net.fetch(request).await {
                        Ok(resp) if (200..300).contains(&resp.status) => {
                            let source = String::from_utf8_lossy(&resp.body).into_owned();
                            self.eval_page_script(&source, &mut report.scripts);
                        }
                        Ok(resp) => {
                            let msg = format!("script {resolved}: HTTP {}", resp.status);
                            tracing::warn!(target: "surl_js", "{msg}");
                            report.scripts.errors.push(msg);
                        }
                        Err(e) => {
                            let msg = format!("script {resolved}: {e}");
                            tracing::warn!(target: "surl_js", "{msg}");
                            report.scripts.errors.push(msg);
                        }
                    }
                    self.pump_jobs()?;
                }
                module @ Script::Module { .. } => module_scripts.push(module),
            }
        }

        // 第二遍:module script(defer 语义——classic 全部跑完后执行)
        if !module_scripts.is_empty() {
            // 预取整张模块图:入口 src + 内联源码里的说明符 + modulepreload 提示
            let mut entries: Vec<String> = Vec::new();
            // <link rel=modulepreload> 是打包器对动态 import chunk 的显式清单,
            // 计算型说明符(字面量扫描看不见的)全靠它
            {
                let doc = self.dom.borrow();
                for n in doc.descendants(doc.root()) {
                    if let Some(el) = doc.element(n)
                        && el.is_html_element("link")
                        && el.attr("rel").is_some_and(|rel| {
                            rel.split_ascii_whitespace()
                                .any(|t| t.eq_ignore_ascii_case("modulepreload"))
                        })
                        && let Some(href) = el.attr("href")
                    {
                        entries.push(self.resolve_url(href));
                    }
                }
            }
            for script in &module_scripts {
                if let Script::Module { src, source } = script {
                    match src {
                        Some(src) => entries.push(self.resolve_url(src)),
                        None => {
                            for spec in modules::scan_specifiers(source) {
                                if let Some(abs) =
                                    modules::resolve_specifier(&self.inline_module_name(), &spec)
                                {
                                    entries.push(abs);
                                }
                            }
                        }
                    }
                }
            }
            let (loaded, prefetch_errors) =
                modules::prefetch_graph(net, &self.module_sources, &entries).await;
            report.modules.prefetched = loaded;
            report.modules.prefetch_errors = prefetch_errors;

            for (idx, script) in module_scripts.iter().enumerate() {
                let Script::Module { src, source } = script else {
                    unreachable!()
                };
                let result = match src {
                    Some(src) => {
                        let abs = self.resolve_url(src);
                        self.import_module(&abs)
                    }
                    None => {
                        let name = format!("{}#inline-module-{idx}", self.inline_module_name());
                        self.evaluate_inline_module(&name, source)
                    }
                };
                match result {
                    Ok(()) => report.modules.evaluated += 1,
                    Err(e) => {
                        tracing::warn!(target: "surl_js", "module error: {e}");
                        report.modules.errors.push(e.to_string());
                    }
                }
                self.pump_jobs()?;
            }
        }

        self.eval("__surl_fireReady()")?;
        self.settle_into(net, &opts, &mut report.settle).await?;

        // 运行期动态 import 的 miss 与模块 promise 的拒绝,一并入报告
        report.modules.runtime_misses = self.module_misses.borrow().clone();
        report.modules.errors.extend(self.take_module_failures()?);
        Ok(report)
    }

    /// 以 loader 路径导入模块(入口有绝对 URL 的情形)。
    fn import_module(&self, url: &str) -> Result<(), RuntimeError> {
        self.ctx.with(|ctx| {
            let result: Result<(), rquickjs::Error> = (|| {
                let promise = rquickjs::Module::import(&ctx, url)?;
                let track: Function = ctx.globals().get("__surl_trackModule")?;
                track.call((promise,))
            })();
            result
                .catch(&ctx)
                .map_err(|caught| caught_to_error(caught, url))
        })
    }

    /// 内联 module:直接以合成名评估源码,其 import 仍走 loader。
    fn evaluate_inline_module(&self, name: &str, source: &str) -> Result<(), RuntimeError> {
        self.ctx.with(|ctx| {
            let result: Result<(), rquickjs::Error> = (|| {
                let module = rquickjs::Module::declare(ctx.clone(), name, source)?;
                module.meta()?.set("url", name)?;
                let (_evaluated, promise) = module.eval()?;
                let track: Function = ctx.globals().get("__surl_trackModule")?;
                track.call((promise,))
            })();
            result
                .catch(&ctx)
                .map_err(|caught| caught_to_error(caught, name))
        })
    }

    /// 收集 JS 侧记录的模块 promise 拒绝。
    fn take_module_failures(&self) -> Result<Vec<String>, RuntimeError> {
        self.ctx.with(|ctx| {
            let failures: Vec<String> = ctx
                .globals()
                .get::<_, rquickjs::Value>("__surl_moduleFailures")
                .ok()
                .and_then(|v| Vec::<String>::from_js(&ctx, v).ok())
                .unwrap_or_default();
            let _ = ctx.globals().set("__surl_moduleFailures", Vec::<String>::new());
            Ok(failures)
        })
    }

    /// 内联模块的基准名:有 base 用 base,否则用一个稳定的假 URL。
    fn inline_module_name(&self) -> String {
        self.base
            .as_ref()
            .map(|u| u.to_string())
            .unwrap_or_else(|| "https://surl.invalid/".to_owned())
    }

    /// 事件循环跑到静止。可单独调用(比如 eval 一段代码后收尾)。
    pub async fn settle(
        &self,
        net: &dyn HttpClient,
        opts: SettleOptions,
    ) -> Result<SettleReport, RuntimeError> {
        let mut report = SettleReport::default();
        self.settle_into(net, &opts, &mut report).await?;
        Ok(report)
    }

    async fn settle_into(
        &self,
        net: &dyn HttpClient,
        opts: &SettleOptions,
        report: &mut SettleReport,
    ) -> Result<(), RuntimeError> {
        let start_ms = self.event_loop.borrow().now_ms;
        let deadline = start_ms + opts.max_virtual_time_ms;
        let mut inflight: FuturesUnordered<_> = FuturesUnordered::new();
        let mut tasks: usize = 0;

        loop {
            self.pump_jobs()?;

            if tasks >= opts.max_tasks {
                report.hit_task_limit = true;
                break;
            }

            // 新排队的请求先起飞(不阻塞后续定时器判断)
            for (id, req) in self.event_loop.borrow_mut().take_requests() {
                report.fetches += 1;
                inflight.push(async move { (id, net.fetch(req).await) });
            }

            // 1. 到点的定时器
            let ready = self.event_loop.borrow_mut().pop_ready_timer();
            if let Some(timer) = ready {
                tasks += 1;
                report.timers_fired += 1;
                if let Err(e) = self.call_trampoline("__surl_runTimer", (timer.0,)) {
                    tracing::warn!(target: "surl_js", "timer callback error: {e}");
                    report.errors.push(e.to_string());
                }
                continue;
            }

            // 2. 在途网络:确定性契约——网络完成优先于时钟快进
            if !inflight.is_empty() {
                let (id, result) = inflight.next().await.expect("non-empty inflight");
                tasks += 1;
                if let Err(e) = self.deliver_fetch(id, result) {
                    tracing::warn!(target: "surl_js", "fetch delivery error: {e}");
                    report.errors.push(e.to_string());
                }
                continue;
            }

            // 3. 时钟快进到下一个定时器
            let next_at = self.event_loop.borrow().next_timer_at();
            if let Some(at) = next_at {
                if at > deadline {
                    report.abandoned_timers = self.event_loop.borrow().timers_remaining();
                    break;
                }
                self.event_loop.borrow_mut().now_ms = at;
                continue;
            }

            break; // 静止:所有队列空、无网络、无定时器
        }

        report.virtual_elapsed_ms = self.event_loop.borrow().now_ms - start_ms;
        Ok(())
    }

    /// fetch 完成 → 调 JS 侧蹦床,resolve 对应 Promise。
    fn deliver_fetch(&self, id: u32, result: HttpResult) -> Result<(), RuntimeError> {
        match result {
            Ok(resp) => {
                let body = String::from_utf8_lossy(&resp.body).into_owned();
                let headers: Vec<Vec<String>> = resp
                    .headers
                    .into_iter()
                    .map(|(k, v)| vec![k, v])
                    .collect();
                self.call_trampoline(
                    "__surl_fetchDone",
                    (
                        id,
                        rquickjs::Undefined,
                        resp.status,
                        resp.status_text,
                        resp.url,
                        headers,
                        body,
                    ),
                )
            }
            Err(message) => self.call_trampoline("__surl_fetchDone", (id, message)),
        }
    }

    fn call_trampoline<A>(&self, name: &str, args: A) -> Result<(), RuntimeError>
    where
        A: for<'js> rquickjs::function::IntoArgs<'js>,
    {
        self.ctx.with(|ctx| {
            let func: Function = ctx.globals().get(name)?;
            let result: Result<(), rquickjs::Error> = func.call(args);
            result.catch(&ctx).map_err(|caught| caught_to_error(caught, name))
        })
    }

    fn eval_page_script(&self, source: &str, report: &mut ScriptReport) {
        match self.eval(source) {
            Ok(()) => report.executed += 1,
            Err(e) => {
                // 浏览器语义:单个脚本抛错不中止页面
                tracing::warn!(target: "surl_js", "script error: {e}");
                report.errors.push(e.to_string());
            }
        }
        if let Err(e) = self.pump_jobs() {
            report.errors.push(e.to_string());
        }
    }

    fn resolve_url(&self, raw: &str) -> String {
        match &self.base {
            Some(base) => base
                .join(raw)
                .map(|u| u.to_string())
                .unwrap_or_else(|_| raw.to_owned()),
            None => raw.to_owned(),
        }
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
    result
        .catch(ctx)
        .map_err(|caught| caught_to_error(caught, label))
}

fn caught_to_error(caught: CaughtError<'_>, label: &str) -> RuntimeError {
    match caught {
        CaughtError::Exception(ex) => {
            let msg = ex.message().unwrap_or_else(|| "<no message>".into());
            let stack = ex.stack().map(|s| format!("\n{s}")).unwrap_or_default();
            RuntimeError::Js(format!("{label}: {msg}{stack}"))
        }
        CaughtError::Value(v) => RuntimeError::Js(format!("{label}: threw {v:?}")),
        CaughtError::Error(e) => RuntimeError::Engine(e),
    }
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
        rt.eval(
            r#"
            const a = document.getElementById("a");
            const b = document.getElementById("b");
            let name = "(no throw)";
            try {
                b.appendChild(a);
            } catch (e) {
                name = e.name;
                if (!(e instanceof DOMException)) name += " (not a DOMException)";
            }
            if (name !== "HierarchyRequestError") throw new Error("got: " + name);
        "#,
        )
        .unwrap();
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
    fn query_selector_from_js() {
        let rt = runtime(concat!(
            r#"<!doctype html><nav><a href="/a" class="x">A</a><a href="/b">B</a></nav>"#,
            r#"<div id="app"><p class="x">p</p></div>"#,
        ));
        rt.eval(
            r##"
            if (document.querySelector("nav a.x").textContent !== "A") throw new Error("qs broken");
            if (document.querySelectorAll("a").length !== 2) throw new Error("qsa broken");
            const app = document.getElementById("app");
            if (app.querySelectorAll(".x").length !== 1) throw new Error("scoped qsa broken");
            if (!app.querySelector("p").matches("p.x")) throw new Error("matches broken");
            if (app.querySelector("p").closest("#app") !== app) throw new Error("closest broken");
            let syntaxError = false;
            try { document.querySelector("p["); } catch (e) { syntaxError = true; }
            if (!syntaxError) throw new Error("invalid selector should throw");
        "##,
        )
        .unwrap();
    }

    #[test]
    fn inner_html_roundtrip() {
        let rt = runtime(r#"<!doctype html><div id="app"><p>old</p></div>"#);
        rt.eval(
            r#"
            const app = document.getElementById("app");
            if (app.innerHTML !== "<p>old</p>") throw new Error("get broken: " + app.innerHTML);
            app.innerHTML = "<ul><li>a</li><li>b</li></ul>";
            if (app.querySelectorAll("li").length !== 2) throw new Error("set broken");
            if (app.outerHTML.indexOf('<div id="app">') !== 0) throw new Error("outer broken");
        "#,
        )
        .unwrap();
        assert!(rt.document().to_html().contains("<li>a</li><li>b</li>"));
    }

    #[test]
    fn document_fragment_moves_children() {
        let rt = runtime(r#"<!doctype html><div id="app"></div>"#);
        rt.eval(
            r#"
            const frag = document.createDocumentFragment();
            if (frag.nodeType !== 11) throw new Error("bad fragment type");
            for (const t of ["a", "b", "c"]) {
                const li = document.createElement("li");
                li.textContent = t;
                frag.appendChild(li);
            }
            const app = document.getElementById("app");
            app.appendChild(frag);
            if (app.childNodes.length !== 3) throw new Error("children not moved");
            if (frag.childNodes.length !== 0) throw new Error("fragment not emptied");
        "#,
        )
        .unwrap();
    }

    #[test]
    fn clone_node_shallow_and_deep() {
        let rt = runtime(r#"<!doctype html><div id="a" class="c"><span>x</span></div>"#);
        rt.eval(
            r#"
            const a = document.getElementById("a");
            const shallow = a.cloneNode(false);
            if (shallow.childNodes.length !== 0) throw new Error("shallow has children");
            if (shallow.className !== "c") throw new Error("attrs not cloned");
            const deep = a.cloneNode(true);
            if (deep.querySelector("span").textContent !== "x") throw new Error("deep broken");
            if (deep === a) throw new Error("clone is same node");
        "#,
        )
        .unwrap();
    }

    #[test]
    fn events_capture_target_bubble() {
        let rt = runtime(r#"<!doctype html><div id="outer"><button id="btn">go</button></div>"#);
        rt.eval(
            r#"
            const order = [];
            const outer = document.getElementById("outer");
            const btn = document.getElementById("btn");
            outer.addEventListener("ping", () => order.push("capture"), true);
            outer.addEventListener("ping", () => order.push("bubble"));
            btn.addEventListener("ping", () => order.push("target"));
            btn.dispatchEvent(new Event("ping", { bubbles: true }));
            if (order.join(",") !== "capture,target,bubble")
                throw new Error("phase order broken: " + order.join(","));

            // once + removeEventListener
            let count = 0;
            const fn = () => count++;
            btn.addEventListener("x", fn, { once: true });
            btn.dispatchEvent(new Event("x"));
            btn.dispatchEvent(new Event("x"));
            if (count !== 1) throw new Error("once broken");

            // preventDefault
            btn.addEventListener("y", (e) => e.preventDefault());
            const notCancelled = btn.dispatchEvent(new Event("y", { cancelable: true }));
            if (notCancelled !== false) throw new Error("preventDefault broken");
        "#,
        )
        .unwrap();
    }

    #[test]
    fn class_list_and_style() {
        let rt = runtime(r#"<!doctype html><div id="x" class="a"></div>"#);
        rt.eval(
            r#"
            const x = document.getElementById("x");
            x.classList.add("b", "c");
            x.classList.remove("a");
            if (!x.classList.contains("b")) throw new Error("classList broken");
            if (x.className !== "b c") throw new Error("className sync broken: " + x.className);
            x.classList.toggle("b");
            if (x.classList.contains("b")) throw new Error("toggle broken");

            x.style.backgroundColor = "red";
            x.style.setProperty("display", "none");
            if (x.style.backgroundColor !== "red") throw new Error("style get broken");
            if (x.getAttribute("style") !== "background-color: red; display: none")
                throw new Error("style attr broken: " + x.getAttribute("style"));
            x.style.removeProperty("background-color");
            if (x.style.cssText !== "display: none") throw new Error("remove broken");
        "#,
        )
        .unwrap();
    }

    #[test]
    fn run_scripts_executes_in_document_order() {
        let doc = parse_html(concat!(
            "<!doctype html><head><script>globalThis.trace = ['head'];</script></head>",
            "<body><div id='app'></div>",
            "<script>trace.push('body'); document.getElementById('app').textContent = 'ran';</script>",
            "<script type='module'>trace.push('nope-module');</script>",
            "<script src='/ext.js'></script>",
            "<script type='application/json'>{\"not\": \"code\"}</script>",
            "<script>trace.push('last');</script></body>",
        ));
        let rt = PageRuntime::new(doc).unwrap();
        let report = rt.run_scripts().unwrap();
        assert_eq!(report.executed, 3);
        assert_eq!(report.skipped_module, 1);
        assert_eq!(report.skipped_external, 1);
        assert!(report.errors.is_empty());
        rt.eval("if (trace.join(',') !== 'head,body,last') throw new Error(trace.join(','))")
            .unwrap();
        assert!(rt.document().to_html().contains(r#"<div id="app">ran</div>"#));
    }

    #[test]
    fn run_scripts_survives_throwing_script() {
        let doc = parse_html(concat!(
            "<!doctype html><script>throw new Error('bad script')</script>",
            "<script>globalThis.alive = true;</script>",
        ));
        let rt = PageRuntime::new(doc).unwrap();
        let report = rt.run_scripts().unwrap();
        assert_eq!(report.executed, 1);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("bad script"));
        rt.eval("if (!globalThis.alive) throw new Error('second script never ran')")
            .unwrap();
    }

    #[test]
    fn lifecycle_events_fire() {
        let doc = parse_html(concat!(
            "<!doctype html><script>",
            "globalThis.events = [];",
            "if (document.readyState !== 'loading') events.push('bad-initial-state');",
            "document.addEventListener('DOMContentLoaded', () => events.push('dcl'));",
            "window.addEventListener('load', () => events.push('load'));",
            "</script>",
        ));
        let rt = PageRuntime::new(doc).unwrap();
        rt.run_scripts().unwrap();
        rt.eval(
            r#"
            if (events.join(",") !== "dcl,load") throw new Error("lifecycle broken: " + events.join(","));
            if (document.readyState !== "complete") throw new Error("readyState broken");
        "#,
        )
        .unwrap();
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
