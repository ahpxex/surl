//! surl 引擎核心。
//!
//! 目标形态:URL 进,渲染后的页面结构出,全程无浏览器依赖。
//! 规划中的分层:fetch(HTTP)→ dom(html5ever 树)→ runtime(QuickJS + Web API)
//! → 事件循环跑到静止 → 语义树 IR。当前只有 fetch 骨架。

pub mod fetch;
