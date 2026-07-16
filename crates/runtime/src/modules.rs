//! ESM 支持:模块图预取 + 同步 loader。
//!
//! 矛盾点:rquickjs 的模块 loader 是同步回调(QuickJS 构建模块图时调用),
//! 而我们的网络是异步的。解法:
//! 1. 评估入口模块前,先扫源码里的字面量 import 说明符,递归预取整张图进缓存;
//! 2. 同步 loader 只读缓存;未命中记入 misses(静态图靠重试收敛,运行期的
//!    动态 import 未命中则单独 reject,不影响页面其余部分);
//! 3. 模块名 = 绝对 URL,import.meta.url 天然正确(Vite 产物依赖它)。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use futures_util::stream::{FuturesUnordered, StreamExt};
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::{Declared, Module};
use rquickjs::{Ctx, Error, Result};

use crate::net::{HttpClient, HttpRequest};

/// 绝对 URL → 模块源码。
pub type SourceCache = Rc<RefCell<HashMap<String, String>>>;
/// loader 没找到的模块(绝对 URL)。
pub type MissLog = Rc<RefCell<Vec<String>>>;

/// 说明符解析:相对/绝对路径经 url crate join;裸说明符(npm 包名)直接报错
/// ——bundler 产物不应出现,出现即环境缺口,错误信息要说人话。
pub struct UrlResolver;

impl Resolver for UrlResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> Result<String> {
        resolve_specifier(base, name).ok_or_else(|| Error::new_resolving(base, name))
    }
}

/// `base` 是导入方模块名(绝对 URL);`name` 是 import 的说明符。
/// 只认 http(s):`node:fs` 这类说明符是 bundler 产物里嵌的代码示例字符串
/// 被扫描器误捞的(tanstack.com 实测),不该变成警告噪音。
pub fn resolve_specifier(base: &str, name: &str) -> Option<String> {
    if let Ok(abs) = url::Url::parse(name) {
        return matches!(abs.scheme(), "http" | "https").then(|| abs.to_string());
    }
    // 相对形式必须以 ./ ../ / 开头;其余是裸说明符,拒绝
    if !(name.starts_with("./") || name.starts_with("../") || name.starts_with('/')) {
        return None;
    }
    let base = url::Url::parse(base).ok()?;
    Some(base.join(name).ok()?.to_string())
}

pub struct CacheLoader {
    pub sources: SourceCache,
    pub misses: MissLog,
}

impl Loader for CacheLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> Result<Module<'js, Declared>> {
        let source = self.sources.borrow().get(name).cloned();
        match source {
            Some(source) => {
                let module = Module::declare(ctx.clone(), name, source)?;
                // 裸引擎不设 import.meta.url,宿主职责(Vite 产物依赖它)
                module.meta()?.set("url", name)?;
                Ok(module)
            }
            None => {
                self.misses.borrow_mut().push(name.to_owned());
                Err(Error::new_loading_message(name, "module not in prefetch cache"))
            }
        }
    }
}

/// 从模块源码里扫出字面量说明符(静态 + 动态 import + re-export)。
/// 这是对 bundler 产物的实用近似,不是 JS 解析器;
/// 拼接出来的动态说明符扫不到,运行期 miss 由 CacheLoader 兜底。
pub fn scan_specifiers(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while let Some(pos) = source[i..].find("import").map(|p| p + i) {
        i = pos + "import".len();
        // 前一个字符不能是标识符的一部分(避免匹配 reimport 之类)
        if pos > 0 {
            let prev = bytes[pos - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'$' || prev == b'.' {
                continue;
            }
        }
        let rest = &source[i..];
        if let Some(spec) = scan_after_import(rest) {
            out.push(spec);
        }
    }
    // export ... from "..."
    let mut j = 0;
    while let Some(pos) = source[j..].find("export").map(|p| p + j) {
        j = pos + "export".len();
        if pos > 0 {
            let prev = bytes[pos - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'$' || prev == b'.' {
                continue;
            }
        }
        let rest = &source[j..];
        // 只在合理近距离内找 from "..."(跨 200 字符的 export 列表也够)
        let window = clamp_to_char_boundary(rest, 400);
        if let Some(fpos) = window.find(" from ") {
            if let Some(spec) = read_string_literal(window[fpos + 6..].trim_start()) {
                out.push(spec);
            }
        } else if let Some(fpos) = window.find("from\"")
            && let Some(spec) = read_string_literal(&window[fpos + 4..])
        {
            out.push(spec);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// `import` 关键字之后的部分:静态 import 找 from "...";
/// 侧效 import 直接跟字符串;动态 import 是 ("...")。
fn scan_after_import(rest: &str) -> Option<String> {
    let trimmed = rest.trim_start();
    // import("...") 动态
    if let Some(inner) = trimmed.strip_prefix('(') {
        return read_string_literal(inner.trim_start());
    }
    // import "..." 侧效
    if trimmed.starts_with('"') || trimmed.starts_with('\'') {
        return read_string_literal(trimmed);
    }
    // import ... from "..."(绑定列表里不含引号,窗口内找 from)
    let window = clamp_to_char_boundary(rest, 400);
    let fpos = window.find("from")?;
    // "from" 必须是独立词
    let after = &window[fpos + 4..];
    read_string_literal(after.trim_start())
}

/// 截断到不超过 max 字节,且落在字符边界上(压缩产物里常见 U+2060 之类的
/// 多字节字符,裸切片会 panic)。
fn clamp_to_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn read_string_literal(s: &str) -> Option<String> {
    let mut chars = s.chars();
    let quote = chars.next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest: &str = chars.as_str();
    let end = rest.find(quote)?;
    let lit = &rest[..end];
    // 说明符不该有换行/转义;有则说明扫串了
    if lit.contains('\n') || lit.contains('\\') {
        return None;
    }
    Some(lit.to_owned())
}

/// 从入口出发预取整张模块图进缓存。返回 (加载数, 错误列表)。
///
/// 图是边下载边发现的,但已知节点必须并发拉取:墙钟 ≈ 依赖深度 × RTT,
/// 而不是模块总数 × RTT(linear.app 有 308 个 chunk,串行时每次跑要 70s+)。
///
/// 错误分级:入口(script 标签明确引用)失败是错误;扫描发现的 hint
/// 失败只记 debug——扫描器是文本近似,bundle 里嵌的代码示例字符串会
/// 被捞成不存在的 URL(tanstack.com 一页 40+ 个 404 hint),不是页面的错。
pub async fn prefetch_graph(
    net: &dyn HttpClient,
    sources: &SourceCache,
    entries: &[String],
) -> (usize, Vec<String>) {
    const MAX_MODULES: usize = 512;
    const MAX_IN_FLIGHT: usize = 16;

    // (url, is_hint):入口 false,扫描发现的 true
    let mut queue: VecDeque<(String, bool)> =
        entries.iter().map(|u| (u.clone(), false)).collect();
    let mut seen: HashSet<String> = entries.iter().cloned().collect();
    let mut scheduled = 0usize;
    let mut capped = false;
    let mut loaded = 0;
    let mut errors = Vec::new();
    let mut in_flight = FuturesUnordered::new();

    let report_failure = |is_hint: bool, msg: String, errors: &mut Vec<String>| {
        if is_hint {
            tracing::debug!(target: "surl_js", "prefetch hint failed: {msg}");
        } else {
            errors.push(msg);
        }
    };

    loop {
        while !capped && in_flight.len() < MAX_IN_FLIGHT {
            let Some((url, is_hint)) = queue.pop_front() else {
                break;
            };
            if sources.borrow().contains_key(&url) {
                continue;
            }
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                // data:/blob: 等——目前不支持,记录跳过
                report_failure(is_hint, format!("unsupported module scheme: {url}"), &mut errors);
                continue;
            }
            if scheduled >= MAX_MODULES {
                errors.push(format!("module graph exceeds {MAX_MODULES} modules, stopping"));
                capped = true;
                break;
            }
            scheduled += 1;
            let req = HttpRequest {
                url: url.clone(),
                method: "GET".into(),
                headers: Vec::new(),
                body: None,
            };
            in_flight.push(async move { (url, is_hint, net.fetch(req).await) });
        }
        let Some((url, is_hint, result)) = in_flight.next().await else {
            break;
        };
        match result {
            Ok(resp) if (200..300).contains(&resp.status) => {
                let source = String::from_utf8_lossy(&resp.body).into_owned();
                for spec in scan_specifiers(&source) {
                    if let Some(abs) = resolve_specifier(&url, &spec)
                        && seen.insert(abs.clone())
                    {
                        queue.push_back((abs, true));
                    }
                }
                sources.borrow_mut().insert(url, source);
                loaded += 1;
            }
            Ok(resp) => {
                report_failure(is_hint, format!("module {url}: HTTP {}", resp.status), &mut errors)
            }
            Err(e) => report_failure(is_hint, format!("module {url}: {e}"), &mut errors),
        }
    }
    (loaded, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::Cell;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Poll, Waker};

    use crate::net::{HttpClient, HttpResponse, HttpResult};

    /// 到齐 `threshold` 个在飞请求前谁都不放行的 client:
    /// 串行实现第一个请求就永久挂起(测试超时),并发实现一轮就绪。
    /// 把「预取必须并发」从性能观感变成确定性断言。
    struct BarrierClient {
        started: Cell<usize>,
        wakers: RefCell<Vec<Waker>>,
        threshold: usize,
    }

    impl HttpClient for BarrierClient {
        fn fetch<'a>(&'a self, req: HttpRequest) -> Pin<Box<dyn Future<Output = HttpResult> + 'a>> {
            Box::pin(async move {
                let mut registered = false;
                std::future::poll_fn(|cx| {
                    if !registered {
                        registered = true;
                        self.started.set(self.started.get() + 1);
                    }
                    if self.started.get() >= self.threshold {
                        for w in self.wakers.borrow_mut().drain(..) {
                            w.wake();
                        }
                        Poll::Ready(())
                    } else {
                        self.wakers.borrow_mut().push(cx.waker().clone());
                        Poll::Pending
                    }
                })
                .await;
                Ok(HttpResponse {
                    status: 200,
                    status_text: "OK".into(),
                    url: req.url,
                    headers: Vec::new(),
                    body: Vec::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn prefetch_fetches_known_nodes_concurrently() {
        let client = BarrierClient {
            started: Cell::new(0),
            wakers: RefCell::new(Vec::new()),
            threshold: 2,
        };
        let sources: SourceCache = Rc::new(RefCell::new(HashMap::new()));
        let entries = vec![
            "https://t.test/a.js".to_string(),
            "https://t.test/b.js".to_string(),
        ];
        let (loaded, errors) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            prefetch_graph(&client, &sources, &entries),
        )
        .await
        .expect("prefetch deadlocked the barrier: known graph nodes must be fetched concurrently");
        assert_eq!(loaded, 2);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn scans_static_dynamic_and_reexport() {
        let src = r#"
            import{j as e}from"./chunk-abc.js";
            import De from "./react-vendor.js";
            import "./side-effect.js";
            export { x } from './reexport.js';
            export*from"./star.js";
            const lazy = () => import("./route-Home.js");
            notanimport("./nope.js");
            const s = "import('./string-trap.js')"; // 会被扫到:实用近似的代价
        "#;
        let specs = scan_specifiers(src);
        assert!(specs.contains(&"./chunk-abc.js".to_string()));
        assert!(specs.contains(&"./react-vendor.js".to_string()));
        assert!(specs.contains(&"./side-effect.js".to_string()));
        assert!(specs.contains(&"./reexport.js".to_string()));
        assert!(specs.contains(&"./star.js".to_string()));
        assert!(specs.contains(&"./route-Home.js".to_string()));
        assert!(!specs.contains(&"./nope.js".to_string()));
    }

    #[test]
    fn scan_survives_multibyte_chars_at_window_edge() {
        // linear.app 的 bundle 在扫描窗口边界处有 U+2060,裸切片会 panic
        let mut src = String::from("import ");
        src.push_str(&"\u{2060}".repeat(300));
        src.push_str(" x from \"./far.js\"; export ");
        src.push_str(&"\u{2060}".repeat(300));
        let _ = scan_specifiers(&src); // 不 panic 即通过
    }

    #[test]
    fn resolves_specifiers_against_module_url() {
        let base = "https://app.test/assets/index.js";
        assert_eq!(
            resolve_specifier(base, "./chunk.js").as_deref(),
            Some("https://app.test/assets/chunk.js")
        );
        assert_eq!(
            resolve_specifier(base, "../lib/x.js").as_deref(),
            Some("https://app.test/lib/x.js")
        );
        assert_eq!(
            resolve_specifier(base, "/root.js").as_deref(),
            Some("https://app.test/root.js")
        );
        assert_eq!(
            resolve_specifier(base, "https://cdn.test/dep.js").as_deref(),
            Some("https://cdn.test/dep.js")
        );
        // 裸说明符拒绝
        assert_eq!(resolve_specifier(base, "react"), None);
    }
}
