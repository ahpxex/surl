//! surl 引擎核心。
//!
//! 目标形态:URL 进,渲染后的页面结构出,全程无浏览器依赖。
//! 分层:fetch(HTTP)→ dom(html5ever 树,见 surl-dom)→ runtime(QuickJS +
//! Web API,M1)→ 事件循环跑到静止(M2)→ semantic(语义树 IR)。

pub mod fetch;
pub mod net;
pub mod semantic;
