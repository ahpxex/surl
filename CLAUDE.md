# surl

curl 给你字节，浏览器给你像素，**surl 给你结构**。输入一个 SPA 的 URL，输出 JS 执行完成后的页面语义结构。全程无浏览器——不依赖 Chrome / Playwright / CDP，这是一个 Rust 手搓的 browser-lite。练手项目：允许高难度，不考虑商业闭环；代码由 agent 编写，用户在架构与审查层参与。

## 产品形态（最终形态与定位）

- **产物**：单二进制 CLI `surl`，像 curl 一样管道友好；一个 URL 进，结构出。
- **输出模式**：`--tree`（语义大纲：landmark/heading/link/role，人和 agent 都直接读，默认输出）、`--dom`（JS 执行后的序列化 HTML）、`--json`（完整 IR）、`--md`（readability 式正文提取）。
- **IR 设计原则**：对齐 a11y snapshot 风格（role/name/state/href + 稳定 uid）——让 agent 在「真浏览器工具」和 surl 之间无感切换；稳定节点身份（跨次渲染 uid 不漂移）是 diff 的前提，本身是树匹配难题。
- **后续形态**（主线通了再做，方向已定）：`surl diff`（结构 diff）+ 内容寻址快照存储（个人 Wayback，`surl <url> @yesterday`）、watch 模式。**不做 MCP server**（2026-07-15 定）：CLI 本身就是 agent 的接口，管道友好即可被直接调用，再包一层 MCP 纯属 overengineering。
- **存在理由（差异化）**：对 curl——能执行 JS，拿到真实结构；对 headless Chrome——① settledness 是事实不是 heuristic（自有事件循环，队列清空即完成）；② 虚拟时钟 + 确定性可复现（真浏览器给不了）；③ 单二进制、零浏览器依赖、体积小。
- **第一用户**：作者自己的 agent 工作流——联网抓取/读链接/部署验证类任务全是场景；项目起源即第一条真实需求（2026-07-15 裸 curl 验证 SPA 部署产生误报）。
- **项目性质与成功标准**：练手项目，目标是 stretch 技术（async Rust、FFI/GC 边界、浏览器内部、规范阅读、在压缩产物里排障），不考虑商业闭环。成功标准两条：readaware.app 在自研运行时里 hydrate 出含 `discord.gg/whDrKXwHWU` 的树；以及学习确实发生（boss fight 的排障沉淀成 writeup）。

## 已定决策（勿重开讨论）

### JS 引擎：quickjs-ng，经 rquickjs

- rquickjs 是 **FFI 绑定不是 port**：C 引擎经 cc 编入二进制，unsafe 圈在绑定层内。
- 选型理由（2026-07 定）：
  1. **显式 job queue**（`JS_ExecutePendingJob` 手动泵）与自研事件循环 / 虚拟时钟天然契合，引擎不会背着宿主偷跑任务；
  2. 引用计数 GC 让 DOM↔JS 对象图的设计难度比 V8 移动式 GC + 不保证执行的 finalizer 小一个量级；
  3. 体积 +1~2MB vs V8 +35MB；可编译到 WASM，未来形态不封死。
- 用 quickjs-ng 这条社区 fork（test262 CI、ES2023+），不用 Bellard 原版线。
- **换 V8 的触发器**（届时走 deno_core，不裸用 rusty_v8）：
  1. corpus 出现成群的引擎级兼容 bug 且上游短期不修；
  2. hydration 排障靠 printf 考古不可忍——V8 inspector 可挂真 DevTools，是换引擎的正当理由。

### 绑定层：resource-table 模式（Deno 的教训，引擎无关）

JS 侧只持有句柄/ID，真实 DOM 状态全部活在 Rust 侧的表里（arena/slotmap）。**不做跨 GC 边界的裸指针互持**——跨堆环是泄漏的根源，句柄表从根上消除它，同时把将来换引擎的成本压到最低。

### 其他选型

- HTML 解析：html5ever；选择器匹配：Servo 的 `selectors` crate。`innerHTML` 需要重入解析器，是已知硬点。
- HTTP：reqwest，**不自研**（与学习目标无关的浪漫，主线通了再说）。
- 语言 Rust（edition 2024），cargo workspace；新模块（dom / runtime）按需在 `crates/` 下开新 crate。

## 范围边界

- **只做结构**：不渲染像素，不做 CSS cascade。样式表可见性（`.hidden` 类等）先不做；若 golden corpus 显示伤害大，再补最小子集（inline style + `hidden` 属性 + `aria-hidden`），永远不做完整 cascade。
- **settledness 是事实不是猜测**：自有事件循环 → 宏任务队列空 + 微任务清空 + 无挂起网络 + DOM 静默 = 渲染完成，不需要 heuristic。
- **虚拟时钟**：`setTimeout(5000)` 直接快进；整次执行确定性、可复现。
- 不做反爬 / 反检测军备竞赛。

## 验证（同样无浏览器）

- **WPT 切片**（M4 起已落地）：官方测试文件 vendor 在 `crates/runtime/tests/wpt/`（commit 钉在 `resources/WPT-COMMIT.txt`），经 `FsHttpClient`（目录即网站）走真实 load 管线跑。expectations.json 是**双向棘轮**：新失败=回归、新通过=必须重新 bless。工具：`SURL_WPT_BLESS=1` 更新、`SURL_WPT_FILTER=<name>` 单文件、`SURL_WPT_VERBOSE=1` 排障。扩切片=往 wpt/ 目录扔文件。
- **golden corpus**：真实页面快照回归。第一条用例即项目起源与验收标准：readaware.app 的产物冻结在 `crates/runtime/tests/corpus/`，离线渲染出的树必须包含 `discord.gg/whDrKXwHWU`，且两次渲染逐字节一致（2026-07-15 用裸 curl 验证 SPA 部署产生误报，本项目由此而来；M3 已通关，见 docs/m3-boss-fight.md）。
- 可选：与 jsdom / happy-dom 差分——它们是独立实现，分歧点即 bug 高发区。

## 工作方式

- **无 spec 文档流程**：架构在对话里定，代码即设计。本文件只记录已关闭的决策，防止重开。
- 产物是单个 CLI 二进制 `surl`。
- 路线概要：M0 无 JS 语义树（html5ever + `--tree`）→ M1 嵌 QuickJS + DOM 绑定核心 → M2 fetch 桥 + 事件循环 + settledness → M3 ESM + React hydration（boss fight）→ M4 WPT/corpus 回归常态化（M0–M4 已完成）→ M5 结构 diff、虚拟时钟外露（不做 MCP server，见「后续形态」）。
