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

