//! M2 集成测试:事件循环 + 虚拟时钟 + fetch 桥 + settledness。
//! 全部离线——网络是内存 mock,整套测试不碰真实时间也不碰真实网络。

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use surl_dom::parse_html;
use surl_runtime::net::{HttpClient, HttpRequest, HttpResponse, HttpResult, NoNetwork};
use surl_runtime::{PageRuntime, SettleOptions};

/// 内存假网络:url → (status, body),并记录请求顺序。
#[derive(Default)]
struct MockNet {
    routes: HashMap<String, (u16, String)>,
    log: Rc<RefCell<Vec<String>>>,
}

impl MockNet {
    fn route(mut self, url: &str, status: u16, body: &str) -> Self {
        self.routes.insert(url.to_owned(), (status, body.to_owned()));
        self
    }
}

impl HttpClient for MockNet {
    fn fetch<'a>(&'a self, req: HttpRequest) -> Pin<Box<dyn Future<Output = HttpResult> + 'a>> {
        self.log.borrow_mut().push(req.url.clone());
        let found = self.routes.get(&req.url).cloned();
        Box::pin(async move {
            match found {
                Some((status, body)) => Ok(HttpResponse {
                    status,
                    status_text: if status == 200 { "OK" } else { "" }.into(),
                    url: req.url,
                    headers: vec![("content-type".into(), "text/plain".into())],
                    body: body.into_bytes(),
                }),
                None => Err(format!("mock: no route for {}", req.url)),
            }
        })
    }
}

fn base() -> url::Url {
    url::Url::parse("https://app.test/").unwrap()
}

#[tokio::test]
async fn virtual_clock_fast_forwards_settimeout() {
    let rt = PageRuntime::new(parse_html(
        r#"<!doctype html><div id="x"></div>
        <script>
          setTimeout(() => { document.getElementById("x").textContent = "late"; }, 5000);
        </script>"#,
    ))
    .unwrap();
    let report = rt.load(&NoNetwork, SettleOptions::default()).await.unwrap();
    assert_eq!(report.settle.timers_fired, 1);
    assert!(report.settle.virtual_elapsed_ms >= 5000);
    assert!(rt.document().to_html().contains("late"));
}

#[tokio::test]
async fn timer_chain_and_clock_readback() {
    let rt = PageRuntime::new(parse_html(
        r#"<!doctype html><script>
          globalThis.ticks = [];
          function next(n) {
            if (n === 0) return;
            setTimeout(() => { ticks.push(Date.now()); next(n - 1); }, 100);
          }
          next(5);
        </script>"#,
    ))
    .unwrap();
    let report = rt.load(&NoNetwork, SettleOptions::default()).await.unwrap();
    assert_eq!(report.settle.timers_fired, 5);
    // 虚拟时钟按 100ms 步进,Date.now 读到的就是虚拟时刻
    rt.eval("if (ticks.join(',') !== '100,200,300,400,500') throw new Error(ticks.join(','))")
        .unwrap();
}

#[tokio::test]
async fn interval_fires_until_cleared() {
    let rt = PageRuntime::new(parse_html(
        r#"<!doctype html><script>
          globalThis.count = 0;
          const id = setInterval(() => {
            if (++count === 3) clearInterval(id);
          }, 10);
        </script>"#,
    ))
    .unwrap();
    let report = rt.load(&NoNetwork, SettleOptions::default()).await.unwrap();
    assert_eq!(report.settle.timers_fired, 3);
    rt.eval("if (count !== 3) throw new Error('count=' + count)").unwrap();
}

#[tokio::test]
async fn runaway_interval_is_abandoned_at_budget() {
    let rt = PageRuntime::new(parse_html(
        r#"<!doctype html><script>
          globalThis.count = 0;
          setInterval(() => count++, 1000); // 永不清除
        </script>"#,
    ))
    .unwrap();
    let opts = SettleOptions {
        max_virtual_time_ms: 5_000,
        ..Default::default()
    };
    let report = rt.load(&NoNetwork, opts).await.unwrap();
    // 5 秒预算内触发 5 次,然后放弃
    assert_eq!(report.settle.timers_fired, 5);
    assert_eq!(report.settle.abandoned_timers, 1);
}

#[tokio::test]
async fn fetch_updates_dom_then_settles() {
    let net = MockNet::default().route(
        "https://app.test/api/items",
        200,
        r#"{"items":["alpha","beta"]}"#,
    );
    let rt = PageRuntime::with_base(
        parse_html(
            r#"<!doctype html><ul id="list"></ul>
            <script>
              fetch("/api/items")
                .then((r) => r.json())
                .then((data) => {
                  const ul = document.getElementById("list");
                  for (const item of data.items) {
                    const li = document.createElement("li");
                    li.textContent = item;
                    ul.appendChild(li);
                  }
                });
            </script>"#,
        ),
        Some(base()),
    )
    .unwrap();
    let report = rt.load(&net, SettleOptions::default()).await.unwrap();
    assert!(report.scripts.errors.is_empty(), "{:?}", report.scripts.errors);
    assert!(report.settle.errors.is_empty(), "{:?}", report.settle.errors);
    assert_eq!(report.settle.fetches, 1);
    let html = rt.document().to_html();
    assert!(html.contains("<li>alpha</li><li>beta</li>"), "{html}");
}

#[tokio::test]
async fn fetch_failure_rejects_promise() {
    let rt = PageRuntime::with_base(
        parse_html(
            r#"<!doctype html><div id="x"></div>
            <script>
              fetch("/nope")
                .then(() => { document.getElementById("x").textContent = "unexpected"; })
                .catch((e) => { document.getElementById("x").textContent = "rejected"; });
            </script>"#,
        ),
        Some(base()),
    )
    .unwrap();
    let report = rt
        .load(&MockNet::default(), SettleOptions::default())
        .await
        .unwrap();
    assert_eq!(report.settle.fetches, 1);
    assert!(rt.document().to_html().contains("rejected"));
}

#[tokio::test]
async fn network_beats_clock_fast_forward() {
    // 确定性契约:有在途网络时时钟不快进 → fetch 回调先于 5s 定时器
    let net = MockNet::default().route("https://app.test/data", 200, "net");
    let rt = PageRuntime::with_base(
        parse_html(
            r#"<!doctype html><script>
              globalThis.order = [];
              setTimeout(() => order.push("timer"), 5000);
              fetch("/data").then(() => order.push("fetch"));
            </script>"#,
        ),
        Some(base()),
    )
    .unwrap();
    rt.load(&net, SettleOptions::default()).await.unwrap();
    rt.eval("if (order.join(',') !== 'fetch,timer') throw new Error(order.join(','))")
        .unwrap();
}

#[tokio::test]
async fn external_scripts_run_in_document_order() {
    let net = MockNet::default()
        .route("https://app.test/a.js", 200, "globalThis.trace.push('ext-a');")
        .route("https://app.test/deep/b.js", 200, "trace.push('ext-b');");
    let rt = PageRuntime::with_base(
        parse_html(concat!(
            "<!doctype html>",
            "<script>globalThis.trace = ['inline-1'];</script>",
            "<script src='/a.js'></script>",
            "<script>trace.push('inline-2');</script>",
            "<script src='https://app.test/deep/b.js'></script>",
        )),
        Some(base()),
    )
    .unwrap();
    let report = rt.load(&net, SettleOptions::default()).await.unwrap();
    assert_eq!(report.scripts.executed, 4);
    rt.eval(
        "if (trace.join(',') !== 'inline-1,ext-a,inline-2,ext-b') throw new Error(trace.join(','))",
    )
    .unwrap();
}

#[tokio::test]
async fn missing_external_script_does_not_abort_page() {
    let rt = PageRuntime::with_base(
        parse_html(concat!(
            "<!doctype html>",
            "<script src='/gone.js'></script>",
            "<script>globalThis.alive = true;</script>",
        )),
        Some(base()),
    )
    .unwrap();
    let report = rt
        .load(&MockNet::default(), SettleOptions::default())
        .await
        .unwrap();
    assert_eq!(report.scripts.executed, 1);
    assert_eq!(report.scripts.errors.len(), 1);
    rt.eval("if (!globalThis.alive) throw new Error('page died')").unwrap();
}

#[tokio::test]
async fn spa_like_flow_end_to_end() {
    // 组合拳:外链脚本 + fetch + 定时器 + 微任务,最后语义树must见到全部产物
    let net = MockNet::default()
        .route(
            "https://app.test/app.js",
            200,
            r#"
            const root = document.getElementById("root");
            root.innerHTML = "<main><h1>Dashboard</h1><p id='status'>loading…</p></main>";
            fetch("/api/user")
              .then((r) => r.json())
              .then((user) => {
                document.getElementById("status").textContent = "hello " + user.name;
                setTimeout(() => {
                  const nav = document.createElement("nav");
                  const a = document.createElement("a");
                  a.setAttribute("href", "/settings");
                  a.textContent = "Settings";
                  nav.appendChild(a);
                  root.appendChild(nav);
                }, 1000);
              });
            "#,
        )
        .route("https://app.test/api/user", 200, r#"{"name":"ada"}"#);
    let rt = PageRuntime::with_base(
        parse_html(
            r#"<!doctype html><title>App</title><div id="root"></div><script src="/app.js"></script>"#,
        ),
        Some(base()),
    )
    .unwrap();
    let report = rt.load(&net, SettleOptions::default()).await.unwrap();
    assert!(report.scripts.errors.is_empty(), "{:?}", report.scripts.errors);
    assert!(report.settle.errors.is_empty(), "{:?}", report.settle.errors);

    let doc = rt.document();
    let tree = surl_core::semantic::extract(&doc, None).to_tree_string();
    assert!(tree.contains("heading[1] \"Dashboard\""), "{tree}");
    assert!(tree.contains("text \"hello ada\""), "{tree}");
    assert!(tree.contains("link \"Settings\" -> /settings"), "{tree}");
}

#[tokio::test]
async fn location_and_url_are_available() {
    let rt = PageRuntime::with_base(
        parse_html("<!doctype html>"),
        Some(url::Url::parse("https://app.test/x/page?q=1#top").unwrap()),
    )
    .unwrap();
    rt.eval(
        r#"
        if (location.hostname !== "app.test") throw new Error("hostname: " + location.hostname);
        if (location.pathname !== "/x/page") throw new Error("pathname: " + location.pathname);
        if (location.search !== "?q=1") throw new Error("search: " + location.search);
        const u = new URL("../other", location.href);
        if (u.pathname !== "/other") throw new Error("URL join: " + u.pathname);
        if (u.origin !== "https://app.test") throw new Error("origin: " + u.origin);
    "#,
    )
    .unwrap();
}

#[tokio::test]
async fn module_graph_static_imports() {
    let net = MockNet::default()
        .route(
            "https://app.test/assets/index.js",
            200,
            r#"import { render } from "./render.js";
               import "./side.js";
               render("from-module");"#,
        )
        .route(
            "https://app.test/assets/render.js",
            200,
            r#"import { h1 } from "../lib/h.js";
               export function render(text) { document.body.appendChild(h1(text)); }"#,
        )
        .route(
            "https://app.test/lib/h.js",
            200,
            r#"export function h1(text) {
                 const el = document.createElement("h1");
                 el.textContent = text;
                 return el;
               }"#,
        )
        .route("https://app.test/assets/side.js", 200, "globalThis.sideRan = true;");
    let rt = PageRuntime::with_base(
        parse_html(r#"<!doctype html><script type="module" src="/assets/index.js"></script>"#),
        Some(base()),
    )
    .unwrap();
    let report = rt.load(&net, SettleOptions::default()).await.unwrap();
    assert_eq!(report.modules.prefetched, 4, "{:?}", report.modules);
    assert_eq!(report.modules.evaluated, 1);
    assert!(report.modules.errors.is_empty(), "{:?}", report.modules.errors);
    rt.eval("if (!globalThis.sideRan) throw new Error('side-effect import missing')").unwrap();
    assert!(rt.document().to_html().contains("<h1>from-module</h1>"));
}

#[tokio::test]
async fn module_dynamic_import_and_meta_url() {
    let net = MockNet::default()
        .route(
            "https://app.test/main.js",
            200,
            r#"globalThis.metaUrl = import.meta.url;
               import("./lazy.js").then((m) => m.go());"#,
        )
        .route(
            "https://app.test/lazy.js",
            200,
            r#"export function go() { document.body.textContent = "lazy loaded"; }"#,
        );
    let rt = PageRuntime::with_base(
        parse_html(r#"<!doctype html><script type="module" src="/main.js"></script>"#),
        Some(base()),
    )
    .unwrap();
    let report = rt.load(&net, SettleOptions::default()).await.unwrap();
    assert!(report.modules.errors.is_empty(), "{:?}", report.modules.errors);
    assert!(report.modules.runtime_misses.is_empty(), "{:?}", report.modules.runtime_misses);
    rt.eval(r#"if (metaUrl !== "https://app.test/main.js") throw new Error("meta.url: " + metaUrl)"#)
        .unwrap();
    assert!(rt.document().to_html().contains("lazy loaded"));
}

#[tokio::test]
async fn inline_module_with_import() {
    let net = MockNet::default().route(
        "https://app.test/dep.js",
        200,
        "export const word = 'inline-module-dep';",
    );
    let rt = PageRuntime::with_base(
        parse_html(concat!(
            r#"<!doctype html><div id="x"></div>"#,
            r#"<script type="module">"#,
            r#"import { word } from "/dep.js";"#,
            r#"document.getElementById("x").textContent = word;"#,
            r#"</script>"#,
        )),
        Some(base()),
    )
    .unwrap();
    let report = rt.load(&net, SettleOptions::default()).await.unwrap();
    assert!(report.modules.errors.is_empty(), "{:?}", report.modules.errors);
    assert!(rt.document().to_html().contains("inline-module-dep"));
}

#[tokio::test]
async fn missing_module_reports_but_page_survives() {
    let rt = PageRuntime::with_base(
        parse_html(concat!(
            r#"<!doctype html>"#,
            r#"<script type="module" src="/gone.js"></script>"#,
            r#"<script>globalThis.classicRan = true;</script>"#,
        )),
        Some(base()),
    )
    .unwrap();
    let report = rt.load(&MockNet::default(), SettleOptions::default()).await.unwrap();
    assert!(!report.modules.prefetch_errors.is_empty() || !report.modules.errors.is_empty());
    rt.eval("if (!globalThis.classicRan) throw new Error('classic script skipped')").unwrap();
}
#[tokio::test]
async fn environment_veneer_surfaces() {
    let rt = PageRuntime::with_base(parse_html("<!doctype html><div id=x></div>"), Some(base())).unwrap();
    rt.eval(
        r#"
        localStorage.setItem("k", "v");
        if (localStorage.getItem("k") !== "v") throw new Error("storage");
        if (matchMedia("(prefers-color-scheme: dark)").matches !== false) throw new Error("matchMedia");
        new IntersectionObserver(() => {}).observe(document.body);
        new MutationObserver(() => {}).observe(document.body, { childList: true });
        if (getComputedStyle(document.body).getPropertyValue("color") !== "") throw new Error("gcs");
        const r = document.getElementById("x").getBoundingClientRect();
        if (r.width !== 0 || r.top !== 0) throw new Error("rect");

        const el = document.getElementById("x");
        el.dataset.userId = "42";
        if (el.getAttribute("data-user-id") !== "42") throw new Error("dataset write");
        if (el.dataset.userId !== "42") throw new Error("dataset read");
        el.tabIndex = 3;
        if (el.getAttribute("tabindex") !== "3") throw new Error("tabIndex");
        if (!el.isConnected) throw new Error("isConnected");

        const input = document.createElement("input");
        input.value = "typed";
        if (input.getAttribute("value") !== "typed") throw new Error("value prop");
        input.checked = true;
        if (!input.hasAttribute("checked")) throw new Error("checked prop");

        el.insertAdjacentHTML("beforeend", "<em>adj</em>");
        if (!el.innerHTML.includes("<em>adj</em>")) throw new Error("insertAdjacentHTML");
        el.append("txt", document.createElement("b"));
        if (el.childNodes.length !== 3) throw new Error("append");

        if (btoa("hi") !== "aGk=") throw new Error("btoa: " + btoa("hi"));
        if (atob("aGk=") !== "hi") throw new Error("atob");
        const enc = new TextEncoder().encode("héllo");
        if (enc.length !== 6) throw new Error("TextEncoder len " + enc.length);
        if (new TextDecoder().decode(enc) !== "héllo") throw new Error("TextDecoder");

        const ac = new AbortController();
        let aborted = false;
        ac.signal.addEventListener("abort", () => { aborted = true; });
        ac.abort();
        if (!aborted || !ac.signal.aborted) throw new Error("abort");

        const u1 = crypto.randomUUID();
        if (!/^[0-9a-f]{8}-[0-9a-f]{4}-/.test(u1)) throw new Error("uuid: " + u1);
        document.cookie = "a=1; path=/";
        if (document.cookie !== "a=1") throw new Error("cookie: " + document.cookie);
    "#,
    )
    .unwrap();
}

#[tokio::test]
async fn crypto_is_deterministic_across_runs() {
    let uuid = |_: ()| async {
        let rt = PageRuntime::new(parse_html("<!doctype html>")).unwrap();
        rt.eval("globalThis.u = crypto.randomUUID()").unwrap();
        rt.console_messages();
        rt.eval("console.log(u)").unwrap();
        rt.console_messages().last().unwrap().message.clone()
    };
    let a = uuid(()).await;
    let b = uuid(()).await;
    assert_eq!(a, b, "randomUUID must be deterministic across runs");
}


#[tokio::test]
async fn infinite_loop_script_is_interrupted_not_hung() {
    // surl 必须永远能终止:死循环脚本被 interrupt handler 掐断,
    // 页面其余部分照常执行
    let mut rt = PageRuntime::new(parse_html(concat!(
        "<!doctype html><div id='x'></div>",
        "<script>for(;;){}</script>",
        "<script>document.getElementById('x').textContent = 'survived';</script>",
    )))
    .unwrap();
    rt.set_script_wall_budget(std::time::Duration::from_millis(300));
    let t0 = std::time::Instant::now();
    let report = rt.load(&NoNetwork, SettleOptions::default()).await.unwrap();
    assert!(t0.elapsed() < std::time::Duration::from_secs(5), "took {:?}", t0.elapsed());
    assert_eq!(report.scripts.errors.len(), 1, "{:?}", report.scripts.errors);
    assert_eq!(report.scripts.executed, 1);
    assert!(rt.document().to_html().contains("survived"));
}

#[tokio::test]
async fn current_script_and_resolved_src() {
    let net = MockNet::default().route(
        "https://app.test/js/app.js",
        200,
        r#"globalThis.seenSrc = document.currentScript && document.currentScript.src;"#,
    );
    let rt = PageRuntime::with_base(
        parse_html(concat!(
            "<!doctype html>",
            "<script src='/js/app.js'></script>",
            "<script>globalThis.afterSrc = document.currentScript ? 'set' : 'null';</script>",
        )),
        Some(base()),
    )
    .unwrap();
    rt.load(&net, SettleOptions::default()).await.unwrap();
    rt.eval(concat!(
        "if (seenSrc !== 'https://app.test/js/app.js') throw new Error('src: ' + seenSrc);",
        "if (afterSrc !== 'set') throw new Error('inline currentScript missing');",
        "if (document.currentScript !== null) throw new Error('currentScript leaked');",
    ))
    .unwrap();
}

#[tokio::test]
async fn xhr_over_fetch_bridge() {
    let net = MockNet::default().route("https://app.test/api", 200, r#"{"n":7}"#);
    let rt = PageRuntime::with_base(
        parse_html(
            r#"<!doctype html><script>
              globalThis.states = [];
              const xhr = new XMLHttpRequest();
              xhr.open("GET", "/api");
              xhr.onreadystatechange = () => states.push(xhr.readyState);
              xhr.onload = () => {
                globalThis.result = JSON.parse(xhr.responseText).n;
                globalThis.hdr = xhr.getResponseHeader("content-type");
              };
              xhr.send();
            </script>"#,
        ),
        Some(base()),
    )
    .unwrap();
    let report = rt.load(&net, SettleOptions::default()).await.unwrap();
    assert_eq!(report.settle.fetches, 1);
    rt.eval(concat!(
        "if (globalThis.result !== 7) throw new Error('result: ' + globalThis.result);",
        "if (hdr !== 'text/plain') throw new Error('hdr: ' + hdr);",
        "if (states.join(',') !== '2,3,4') throw new Error('states: ' + states.join(','));",
    ))
    .unwrap();
}

#[tokio::test]
async fn custom_elements_registry() {
    let rt = PageRuntime::new(parse_html("<!doctype html>")).unwrap();
    rt.eval(
        r#"
        globalThis.defined = [];
        customElements.whenDefined("x-a").then(() => defined.push("x-a"));
        class XA extends HTMLElement {}
        customElements.define("x-a", XA);
        if (customElements.get("x-a") !== XA) throw new Error("get broken");
        let dup = false;
        try { customElements.define("x-a", XA); } catch (e) { dup = e.name === "NotSupportedError"; }
        if (!dup) throw new Error("duplicate define should throw");
    "#,
    )
    .unwrap();
    rt.pump_jobs().unwrap();
    rt.eval("if (defined.join(',') !== 'x-a') throw new Error('whenDefined broken')")
        .unwrap();
}

/// quickjs-ng(rquickjs-sys 0.12.1 vendor)的 Iterator.prototype.find/filter
/// 对谓词不命中的对象漏 JS_FreeValue,teardown 时 JS_FreeRuntime 直接 abort
/// (astro.build 实测)。bootstrap 用 JS 实现覆盖止血;此测试钉住它——
/// 若止血块被删而引擎未修,这里会 SIGABRT 炸掉整个测试进程,足够醒目。
#[tokio::test]
async fn iterator_find_filter_do_not_leak_on_teardown() {
    let rt = PageRuntime::new(parse_html(
        r#"<!doctype html><p>a</p><p>b</p><p>c</p>
        <script>
          // find:命中不在第 0 个,前面被扫过的对象曾经泄漏
          document.querySelectorAll("p").values().find((e) => e.textContent === "b");
          [{ a: 1 }, { a: 2 }, { a: 3 }].values().find((x) => x.a === 2);
          // filter:不命中的对象同病
          [{}, { a: 1 }, {}].values().filter((x) => x.a === 1).toArray();
        </script>"#,
    ))
    .unwrap();
    rt.load(&NoNetwork, SettleOptions::default()).await.unwrap();
    drop(rt); // 泄漏在这里引爆(JS_FreeRuntime assert)
}
