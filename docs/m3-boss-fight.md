# M3 Boss Fight:让 readaware.app 在自研运行时里活过来

2026-07-15。验收标准:`surl https://readaware.app --tree` 的输出必须包含
`discord.gg/whDrKXwHWU`。这个标准的来源:同一天早些时候,用裸 curl 验证这个
React SPA 的部署,拿到 200 OK 和一个空壳 `<div id="root">`,误报「部署成功、
内容正常」。surl 项目由此而生。

## 战前建设(M0–M2 的地基,按依赖顺序)

打 boss 之前,这些必须先立着:

1. **arena DOM**(M0):节点活在 slotmap 里,JS 只拿数字句柄。没有跨 GC 边界
   的对象图,这是后面一切不泄漏、不悬垂的前提。
2. **resource-table 绑定**(M1):Rust 侧一层扁平 op(标量进标量出),JS 侧
   bootstrap 搭 DOM 类层次。包装对象按句柄缓存,`el === el` 成立——React
   的 reconciler 全程依赖节点身份。
3. **事件循环 + 虚拟时钟**(M2):宏任务调度表在 Rust,回调本体在 JS,到点
   经蹦床调回。settledness 是事实:微任务空 + 无就绪宏任务 + 无在途网络 +
   定时器超预算 = 完成。React 的 scheduler 在无 MessageChannel 环境下回落到
   setTimeout——正好落在我们的虚拟时钟上,整个渲染调度零真实等待。
4. **ESM 管线**(M3 前半):rquickjs 的模块 loader 是同步回调,网络是异步的。
   解法是评估前预取整张模块图(扫字面量 import 说明符,递归抓取),同步
   loader 只读缓存。模块以绝对 URL 命名,宿主自己设 `import.meta.url`
   ——裸引擎不管这个,而 Vite 产物离了它活不了。

## 战斗记录(grind 回合)

打法:跑真站 → 看第一个异常 → 补最小缺口 → 重跑。

### 回合 1:`URLSearchParams is not defined`

模块评估直接 reject。QuickJS 是纯 ES 引擎,URL/URLSearchParams 是 Web API。
URL 已经有了(Rust 侧 url crate 做解析,JS 侧薄壳),补一个纯 JS 的
URLSearchParams,挂上 `URL.prototype.searchParams`。

### 回合 2:`invalid 'instanceof' right operand`

这回合有意思。栈指向 react-dom 内部(压缩后的 `Zr`/`xp`/`ih`),触发点是
定时器回调——说明 React 已经在我们的事件循环上跑起来了,渲染进行到一半。

`instanceof` 右操作数无效 = 某个全局类不存在。react-dom 的 `getActiveElement`
会做 `x instanceof HTMLIFrameElement` 来穿透 iframe 找焦点元素。我们只有
`Element` 一个类。

**陷阱**:最省事的修法是把所有 `HTML*Element` 都 alias 到 `Element`——
但那样 `document.body instanceof HTMLIFrameElement` 会是 true,React 会把
body 当 iframe 去取 `.contentDocument`,直接死循环或崩溃。正确修法是按标签
建真类层次:`wrap()` 按 tagName 选类,h1–h6 共享 HTMLHeadingElement,
td/th 共享 HTMLTableCellElement,iframe 有自己的类(contentWindow 返回 null)。
instanceof 语义从「全真」变成「按标签为真」,React 的探测逻辑就都对了。

### 回合 3:通关

```
banner
  link "ReadAware" -> https://readaware.app/#top
  navigation
    link "Download" -> https://readaware.app/#download
    link "GitHub" -> https://github.com/ahpxex/read-aware
    link "Discord" -> https://discord.gg/whDrKXwHWU   ← 验收标准
main
  heading[1] "Reading that remembers"
  ...(完整落地页:hero、三段 feature、下载列表、页脚)
```

报告:2 个模块预取(index bundle + 一个 chunk),4 次定时器(React scheduler
的 setTimeout 回落),1 次 fetch(页面自己发的),虚拟时间 0ms——整个 React
渲染没有消耗一毫秒真实等待。全程 3 秒(几乎全是网络)。

## 学到的东西(与预期的对照)

- **预期中的硬仗没打**:事先以为 hydration 的 DOM 一致性(节点身份、兄弟
  遍历顺序、文本合并语义)会是主要伤亡来源。实际一个都没炸——因为
  readaware 的壳是空的,React 走的是 `createRoot().render()` 纯客户端渲染,
  不是 hydrateRoot。真正的 hydration 一致性测试要等一个 SSR 站点的 corpus。
- **实际的伤亡全在环境面**:两个 boss 级异常都不是引擎、不是 DOM、不是事件
  循环,而是「浏览器全局对象的长尾」。这验证了 veneer(环境垫片)作为独立
  层的设计:崩一个补一个,不碰核心。
- **instanceof 的教训值得记住**:stub 不是越宽越好。「让检查通过」和「让
  检查给出正确答案」是两回事,getActiveElement 那种探测型代码需要后者。
- **确定性时钟在真实站点上成立**:React scheduler + 页面自己的 setTimeout
  全部落在虚拟时钟上,virtual_ms=0,跑几次输出逐字节一致。

## 未竟事项(M4+ 的输入)

- SSR + hydrateRoot 的 corpus 用例(真 hydration 一致性)。
- `<link rel="modulepreload">` 提示尚未利用(现在靠扫 import,已够用)。
- 动态 import 的运行期 miss 只会 reject 那一个 promise;如果 corpus 显示
  路由级代码分割普遍受伤,考虑把 miss 变成「挂起 + 事件循环补载 + 重试」。
- MessageChannel 故意不实现(让 scheduler 走 setTimeout);如果哪个框架
  硬依赖,补一个基于微任务的实现。
