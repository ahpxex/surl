// surl bootstrap:在 QuickJS 全局里搭出 DOM 的 JS 面孔。
// 原则:所有真实状态在 Rust 侧(经 __surl_dom 句柄 op 访问),这里只有
// 包装对象与缓存。同一节点永远返回同一个包装对象(=== 身份,hydration 依赖)。
"use strict";
(function (g) {
  const dom = g.__surl_dom;

  // id -> wrapper。arena 槽位不复用,句柄不会二义。
  const cache = new Map();

  function wrap(id) {
    if (!id) return null;
    let node = cache.get(id);
    if (node) return node;
    const type = dom.nodeType(id);
    if (type === 1) node = new (elementClassFor(dom.tagName(id)))(id);
    else if (type === 3) node = new Text(id);
    else if (type === 8) node = new Comment(id);
    else if (type === 9) node = new DocumentNode(id);
    else if (type === 11) node = new DocumentFragment(id);
    else node = new Node(id);
    cache.set(id, node);
    return node;
  }

  function unwrap(node, what) {
    if (node instanceof Node) return node._id;
    throw new TypeError((what || "argument") + " is not a Node");
  }

  // ---- 事件系统(纯 JS 侧:监听器不跨 FFI)----

  class Event {
    constructor(type, init) {
      init = init || {};
      this.type = String(type);
      this.bubbles = !!init.bubbles;
      this.cancelable = !!init.cancelable;
      this.defaultPrevented = false;
      this.target = null;
      this.currentTarget = null;
      this.eventPhase = 0;
      this._stopped = false;
      this._immediateStopped = false;
      this.isTrusted = false;
      this.timeStamp = 0;
    }
    stopPropagation() {
      this._stopped = true;
    }
    stopImmediatePropagation() {
      this._stopped = true;
      this._immediateStopped = true;
    }
    preventDefault() {
      if (this.cancelable) this.defaultPrevented = true;
    }
  }
  class CustomEvent extends Event {
    constructor(type, init) {
      super(type, init);
      this.detail = (init && init.detail) !== undefined ? init.detail : null;
    }
  }

  class EventTarget {
    _ensureListeners() {
      if (!this._listeners) this._listeners = new Map();
      return this._listeners;
    }
    addEventListener(type, callback, options) {
      if (typeof callback !== "function") return;
      const capture = typeof options === "boolean" ? options : !!(options && options.capture);
      const once = !!(options && options.once);
      const list = this._ensureListeners();
      const key = String(type);
      if (!list.has(key)) list.set(key, []);
      const entries = list.get(key);
      if (entries.some((e) => e.callback === callback && e.capture === capture)) return;
      entries.push({ callback, capture, once });
    }
    removeEventListener(type, callback, options) {
      const capture = typeof options === "boolean" ? options : !!(options && options.capture);
      const entries = this._listeners && this._listeners.get(String(type));
      if (!entries) return;
      const i = entries.findIndex((e) => e.callback === callback && e.capture === capture);
      if (i >= 0) entries.splice(i, 1);
    }
    _invokeListeners(event, phase) {
      const entries = this._listeners && this._listeners.get(event.type);
      if (!entries) return;
      // 快照:监听器里增删不影响本次派发
      for (const entry of entries.slice()) {
        if (event._immediateStopped) break;
        // capture 阶段只跑 capture 监听器,bubble/target 阶段只跑非 capture
        if (phase === 1 && !entry.capture) continue;
        if (phase === 3 && entry.capture) continue;
        if (entry.once) this.removeEventListener(event.type, entry.callback, entry.capture);
        try {
          entry.callback.call(this, event);
        } catch (e) {
          console.error("uncaught listener error:", e && e.message ? e.message : String(e));
        }
      }
    }
    dispatchEvent(event) {
      event.target = this;
      // 组装祖先链(仅 Node 有;window 单独处理)
      const path = [];
      if (this instanceof Node) {
        let p = this.parentNode;
        while (p) {
          path.push(p);
          p = p.parentNode;
        }
      }
      event.eventPhase = 1; // CAPTURING_PHASE
      for (let i = path.length - 1; i >= 0 && !event._stopped; i--) {
        event.currentTarget = path[i];
        path[i]._invokeListeners(event, 1);
      }
      if (!event._stopped) {
        event.eventPhase = 2; // AT_TARGET
        event.currentTarget = this;
        this._invokeListeners(event, 2);
      }
      if (event.bubbles) {
        event.eventPhase = 3; // BUBBLING_PHASE
        for (let i = 0; i < path.length && !event._stopped; i++) {
          event.currentTarget = path[i];
          path[i]._invokeListeners(event, 3);
        }
      }
      event.eventPhase = 0;
      event.currentTarget = null;
      return !event.defaultPrevented;
    }
  }

  class Node extends EventTarget {
    constructor(id) {
      super();
      this._id = id;
    }
    get nodeType() {
      return dom.nodeType(this._id);
    }
    get nodeName() {
      return dom.nodeName(this._id);
    }
    get parentNode() {
      return wrap(dom.parent(this._id));
    }
    get parentElement() {
      const p = this.parentNode;
      return p && p.nodeType === 1 ? p : null;
    }
    get childNodes() {
      return dom.childNodes(this._id).map(wrap);
    }
    get firstChild() {
      return wrap(dom.firstChild(this._id));
    }
    get lastChild() {
      return wrap(dom.lastChild(this._id));
    }
    get nextSibling() {
      return wrap(dom.nextSibling(this._id));
    }
    get previousSibling() {
      return wrap(dom.prevSibling(this._id));
    }
    get ownerDocument() {
      return g.document || null;
    }
    hasChildNodes() {
      return dom.firstChild(this._id) !== 0;
    }
    appendChild(child) {
      dom.appendChild(this._id, unwrap(child, "child"));
      return child;
    }
    insertBefore(node, reference) {
      dom.insertBefore(
        this._id,
        unwrap(node, "node"),
        reference == null ? 0 : unwrap(reference, "reference"),
      );
      return node;
    }
    removeChild(child) {
      dom.removeChild(this._id, unwrap(child, "child"));
      return child;
    }
    replaceChild(newChild, oldChild) {
      this.insertBefore(newChild, oldChild);
      this.removeChild(oldChild);
      return oldChild;
    }
    cloneNode(deep) {
      return wrap(dom.cloneNode(this._id, !!deep));
    }
    remove() {
      const p = this.parentNode;
      if (p) p.removeChild(this);
    }
    contains(other) {
      let cursor = other;
      while (cursor) {
        if (cursor === this) return true;
        cursor = cursor.parentNode;
      }
      return false;
    }
    get textContent() {
      return dom.textContent(this._id);
    }
    set textContent(value) {
      dom.setTextContent(this._id, value == null ? "" : String(value));
    }
    get nodeValue() {
      const v = dom.nodeValue(this._id);
      return v == null ? null : v;
    }
    set nodeValue(value) {
      dom.setNodeValue(this._id, String(value));
    }
  }
  Node.ELEMENT_NODE = 1;
  Node.TEXT_NODE = 3;
  Node.COMMENT_NODE = 8;
  Node.DOCUMENT_NODE = 9;
  Node.DOCUMENT_FRAGMENT_NODE = 11;

  class DocumentFragment extends Node {
    get nodeName() {
      return "#document-fragment";
    }
    querySelector(sel) {
      return wrap(dom.querySelector(this._id, String(sel)));
    }
    querySelectorAll(sel) {
      return dom.querySelectorAll(this._id, String(sel)).map(wrap);
    }
  }

  class CharacterData extends Node {
    get data() {
      return this.nodeValue;
    }
    set data(value) {
      this.nodeValue = value;
    }
    get length() {
      return this.nodeValue.length;
    }
  }
  class Text extends CharacterData {}
  class Comment extends CharacterData {}

  class Element extends Node {
    get tagName() {
      return dom.tagName(this._id);
    }
    get localName() {
      return dom.tagName(this._id).toLowerCase();
    }
    getAttribute(name) {
      // 原生 op 的 None 过来是 undefined,DOM 规范要求 null
      const v = dom.getAttribute(this._id, String(name));
      return v == null ? null : v;
    }
    setAttribute(name, value) {
      dom.setAttribute(this._id, String(name), String(value));
    }
    removeAttribute(name) {
      dom.removeAttribute(this._id, String(name));
    }
    hasAttribute(name) {
      return dom.hasAttribute(this._id, String(name));
    }
    get id() {
      return this.getAttribute("id") || "";
    }
    set id(value) {
      this.setAttribute("id", value);
    }
    get className() {
      return this.getAttribute("class") || "";
    }
    set className(value) {
      this.setAttribute("class", value);
    }
    get children() {
      return this.childNodes.filter((n) => n.nodeType === 1);
    }
    get firstElementChild() {
      return this.children[0] || null;
    }
    get lastElementChild() {
      const c = this.children;
      return c[c.length - 1] || null;
    }
    get innerHTML() {
      return dom.innerHTML(this._id);
    }
    set innerHTML(html) {
      dom.setInnerHTML(this._id, html == null ? "" : String(html));
    }
    get outerHTML() {
      return dom.outerHTML(this._id);
    }
    matches(sel) {
      return dom.matches(this._id, String(sel));
    }
    closest(sel) {
      let cursor = this;
      while (cursor && cursor.nodeType === 1) {
        if (cursor.matches(sel)) return cursor;
        cursor = cursor.parentNode;
      }
      return null;
    }
    querySelector(sel) {
      return wrap(dom.querySelector(this._id, String(sel)));
    }
    querySelectorAll(sel) {
      return dom.querySelectorAll(this._id, String(sel)).map(wrap);
    }
    getElementsByTagName(tag) {
      tag = String(tag).toLowerCase();
      return this.querySelectorAll(tag === "*" ? "*" : tag);
    }
    getElementsByClassName(names) {
      const classes = String(names).split(/\s+/).filter(Boolean);
      return this.querySelectorAll("*").filter((el) =>
        classes.every((c) => el.classList.contains(c)),
      );
    }
    get classList() {
      const self = this;
      return {
        _all() {
          return (self.getAttribute("class") || "").split(/\s+/).filter(Boolean);
        },
        get length() {
          return this._all().length;
        },
        item(i) {
          return this._all()[i] || null;
        },
        contains(token) {
          return this._all().includes(String(token));
        },
        add(...tokens) {
          const all = this._all();
          for (const t of tokens) if (!all.includes(String(t))) all.push(String(t));
          self.setAttribute("class", all.join(" "));
        },
        remove(...tokens) {
          const drop = tokens.map(String);
          self.setAttribute(
            "class",
            this._all().filter((c) => !drop.includes(c)).join(" "),
          );
        },
        toggle(token, force) {
          token = String(token);
          const has = this.contains(token);
          const want = force === undefined ? !has : !!force;
          if (want && !has) this.add(token);
          if (!want && has) this.remove(token);
          return want;
        },
        toString() {
          return self.getAttribute("class") || "";
        },
      };
    }
    get style() {
      if (!this._style) this._style = makeStyle(this);
      return this._style;
    }
  }

  // style 属性 <-> 内联 style attribute 的极简桥。支持 el.style.color = "red"
  // (camelCase 转 kebab)与 setProperty/getPropertyValue/removeProperty/cssText。
  function makeStyle(el) {
    function parse() {
      const out = [];
      const raw = el.getAttribute("style") || "";
      for (const part of raw.split(";")) {
        const i = part.indexOf(":");
        if (i < 0) continue;
        const k = part.slice(0, i).trim();
        const v = part.slice(i + 1).trim();
        if (k) out.push([k, v]);
      }
      return out;
    }
    function write(entries) {
      const css = entries.map(([k, v]) => k + ": " + v).join("; ");
      if (css) el.setAttribute("style", css);
      else el.removeAttribute("style");
    }
    function toKebab(prop) {
      return String(prop).replace(/[A-Z]/g, (m) => "-" + m.toLowerCase());
    }
    const base = {
      setProperty(prop, value) {
        prop = toKebab(prop);
        const entries = parse().filter(([k]) => k !== prop);
        if (value !== "" && value != null) entries.push([prop, String(value)]);
        write(entries);
      },
      getPropertyValue(prop) {
        prop = toKebab(prop);
        const hit = parse().find(([k]) => k === prop);
        return hit ? hit[1] : "";
      },
      removeProperty(prop) {
        const value = this.getPropertyValue(prop);
        write(parse().filter(([k]) => k !== toKebab(prop)));
        return value;
      },
      get cssText() {
        return el.getAttribute("style") || "";
      },
      set cssText(v) {
        if (v) el.setAttribute("style", String(v));
        else el.removeAttribute("style");
      },
    };
    return new Proxy(base, {
      get(target, prop) {
        if (prop in target || typeof prop === "symbol") return target[prop];
        return target.getPropertyValue(prop);
      },
      set(target, prop, value) {
        if (typeof prop === "symbol" || prop === "cssText") {
          target[prop] = value;
        } else {
          target.setProperty(prop, value);
        }
        return true;
      },
    });
  }

  class DocumentNode extends Node {
    get documentElement() {
      return wrap(dom.documentElement());
    }
    get body() {
      return wrap(dom.body());
    }
    get head() {
      return wrap(dom.head());
    }
    get nodeName() {
      return "#document";
    }
    getElementById(id) {
      return wrap(dom.getElementById(String(id)));
    }
    createElement(tag) {
      return wrap(dom.createElement(String(tag)));
    }
    createElementNS(ns, tag) {
      return wrap(dom.createElementNS(ns == null ? "" : String(ns), String(tag)));
    }
    createTextNode(text) {
      return wrap(dom.createText(String(text)));
    }
    createComment(text) {
      return wrap(dom.createComment(String(text)));
    }
    createDocumentFragment() {
      return wrap(dom.createFragment());
    }
    createEvent() {
      return new Event("");
    }
    querySelector(sel) {
      return wrap(dom.querySelector(this._id, String(sel)));
    }
    querySelectorAll(sel) {
      return dom.querySelectorAll(this._id, String(sel)).map(wrap);
    }
    getElementsByTagName(tag) {
      return this.documentElement ? this.documentElement.getElementsByTagName(tag) : [];
    }
    getElementsByClassName(names) {
      return this.documentElement ? this.documentElement.getElementsByClassName(names) : [];
    }
    get title() {
      const t = this.head && this.head.querySelector("title");
      return t ? t.textContent : "";
    }
    set title(value) {
      let t = this.head && this.head.querySelector("title");
      if (!t && this.head) {
        t = this.createElement("title");
        this.head.appendChild(t);
      }
      if (t) t.textContent = value;
    }
    get defaultView() {
      return g;
    }
  }
  DocumentNode.prototype.readyState = "loading";

  // 生命周期:script 全部跑完后由宿主调用
  function fireDocumentReady() {
    DocumentNode.prototype.readyState = "interactive";
    const dcl = new Event("DOMContentLoaded", { bubbles: true });
    g.document.dispatchEvent(dcl);
    DocumentNode.prototype.readyState = "complete";
    const load = new Event("load");
    load.target = g;
    windowTarget._invokeListeners(load, 2);
    if (typeof g.onload === "function") {
      try {
        g.onload(load);
      } catch (e) {
        console.error("onload error:", e && e.message ? e.message : String(e));
      }
    }
  }
  Object.defineProperty(g, "__surl_fireReady", {
    value: fireDocumentReady,
    enumerable: false,
  });

  // HTML*Element 层次:instanceof 必须按标签区分——React 的
  // getActiveElement 靠 `x instanceof HTMLIFrameElement` 判断 iframe,
  // 全部 alias 到 Element 会让 body 误判成 iframe 而死循环。
  class HTMLElement extends Element {}
  class HTMLIFrameElement extends HTMLElement {
    get contentWindow() {
      return null;
    }
    get contentDocument() {
      return null;
    }
  }
  const TAG_CLASS_NAMES = {
    a: "HTMLAnchorElement", area: "HTMLAreaElement", audio: "HTMLAudioElement",
    body: "HTMLBodyElement", br: "HTMLBRElement", button: "HTMLButtonElement",
    canvas: "HTMLCanvasElement", div: "HTMLDivElement", form: "HTMLFormElement",
    h1: "HTMLHeadingElement", h2: "HTMLHeadingElement", h3: "HTMLHeadingElement",
    h4: "HTMLHeadingElement", h5: "HTMLHeadingElement", h6: "HTMLHeadingElement",
    head: "HTMLHeadElement", hr: "HTMLHRElement", html: "HTMLHtmlElement",
    img: "HTMLImageElement", input: "HTMLInputElement", label: "HTMLLabelElement",
    li: "HTMLLIElement", link: "HTMLLinkElement", meta: "HTMLMetaElement",
    ol: "HTMLOListElement", option: "HTMLOptionElement", p: "HTMLParagraphElement",
    pre: "HTMLPreElement", script: "HTMLScriptElement", select: "HTMLSelectElement",
    span: "HTMLSpanElement", style: "HTMLStyleElement", table: "HTMLTableElement",
    td: "HTMLTableCellElement", th: "HTMLTableCellElement", template: "HTMLTemplateElement",
    textarea: "HTMLTextAreaElement", tr: "HTMLTableRowElement", ul: "HTMLUListElement",
    video: "HTMLVideoElement",
  };
  const tagClassCache = new Map([["iframe", HTMLIFrameElement]]);
  function elementClassFor(tagName) {
    const tag = String(tagName).toLowerCase();
    let cls = tagClassCache.get(tag);
    if (cls) return cls;
    const name = TAG_CLASS_NAMES[tag];
    if (!name) return HTMLElement;
    cls = g[name] || class extends HTMLElement {};
    tagClassCache.set(tag, cls);
    return cls;
  }
  // 把类名挂到全局(同名标签共享一个类,如 h1-h6 / td-th)
  for (const name of new Set(Object.values(TAG_CLASS_NAMES))) {
    if (!g[name]) g[name] = class extends HTMLElement {};
  }
  for (const [tag, name] of Object.entries(TAG_CLASS_NAMES)) {
    tagClassCache.set(tag, g[name]);
  }

  g.EventTarget = EventTarget;
  g.Event = Event;
  g.CustomEvent = CustomEvent;
  g.Node = Node;
  g.CharacterData = CharacterData;
  g.Text = Text;
  g.Comment = Comment;
  g.Element = Element;
  g.HTMLElement = HTMLElement;
  g.HTMLIFrameElement = HTMLIFrameElement;
  g.SVGElement = Element;
  g.Document = DocumentNode;
  g.DocumentFragment = DocumentFragment;
  g.HTMLDocument = DocumentNode;

  g.document = wrap(dom.root());
  g.window = g;
  g.self = g;

  // window 作为事件目标(globalThis 不是 Node,单独给一个 target)
  const windowTarget = new EventTarget();
  g.addEventListener = windowTarget.addEventListener.bind(windowTarget);
  g.removeEventListener = windowTarget.removeEventListener.bind(windowTarget);
  g.dispatchEvent = function (event) {
    event.target = g;
    event.currentTarget = g;
    windowTarget._invokeListeners(event, 2);
    event.currentTarget = null;
    return !event.defaultPrevented;
  };

  // ---- 定时器:回调存 JS 侧,Rust 只管调度表,到点经蹦床调回 ----

  const timerCallbacks = new Map();

  g.setTimeout = function (fn, delay) {
    const args = Array.prototype.slice.call(arguments, 2);
    const id = dom.timerSchedule(Number(delay) || 0, false);
    timerCallbacks.set(id, { fn, args });
    return id;
  };
  g.setInterval = function (fn, delay) {
    const args = Array.prototype.slice.call(arguments, 2);
    const id = dom.timerSchedule(Number(delay) || 0, true);
    timerCallbacks.set(id, { fn, args, repeating: true });
    return id;
  };
  g.clearTimeout = g.clearInterval = function (id) {
    if (id == null) return;
    dom.timerClear(Number(id));
    timerCallbacks.delete(Number(id));
  };
  g.queueMicrotask = function (fn) {
    Promise.resolve().then(() => fn());
  };
  g.requestAnimationFrame = function (fn) {
    return g.setTimeout(() => fn(dom.clockNow()), 16);
  };
  g.cancelAnimationFrame = g.clearTimeout;

  // 宿主事件循环的蹦床:执行到点的定时器回调
  Object.defineProperty(g, "__surl_runTimer", {
    enumerable: false,
    value: function (id) {
      const entry = timerCallbacks.get(id);
      if (!entry) return;
      if (!entry.repeating) timerCallbacks.delete(id);
      if (typeof entry.fn === "function") entry.fn.apply(g, entry.args);
      else if (typeof entry.fn === "string") (0, eval)(entry.fn);
    },
  });

  // 确定性时钟:Date.now / performance.now 全部走虚拟时钟
  Date.now = function () {
    return dom.clockNow();
  };
  g.performance = {
    now() {
      return dom.clockNow();
    },
    timeOrigin: 0,
    mark() {},
    measure() {},
  };

  // ---- URL(QuickJS 无内置;解析走 Rust 的 url crate)----

  class URL {
    constructor(input, base) {
      const raw = dom.urlResolve(String(input), base === undefined ? "" : String(base));
      if (!raw) throw new TypeError("Invalid URL: " + input);
      const p = JSON.parse(raw);
      this.href = p.href;
      this.origin = p.origin;
      this.protocol = p.protocol;
      this.host = p.host;
      this.hostname = p.hostname;
      this.port = p.port;
      this.pathname = p.pathname;
      this.search = p.search;
      this.hash = p.hash;
      this.username = "";
      this.password = "";
    }
    toString() {
      return this.href;
    }
    toJSON() {
      return this.href;
    }
  }
  g.URL = URL;

  class URLSearchParams {
    constructor(init) {
      this._pairs = [];
      if (typeof init === "string") {
        const s = init.startsWith("?") ? init.slice(1) : init;
        for (const part of s.split("&")) {
          if (!part) continue;
          const eq = part.indexOf("=");
          const k = eq < 0 ? part : part.slice(0, eq);
          const v = eq < 0 ? "" : part.slice(eq + 1);
          this._pairs.push([decodeURIComponent(k.replace(/\+/g, " ")), decodeURIComponent(v.replace(/\+/g, " "))]);
        }
      } else if (init instanceof URLSearchParams) {
        this._pairs = init._pairs.map((p) => [...p]);
      } else if (Array.isArray(init)) {
        for (const [k, v] of init) this._pairs.push([String(k), String(v)]);
      } else if (init && typeof init === "object") {
        for (const k of Object.keys(init)) this._pairs.push([k, String(init[k])]);
      }
    }
    append(k, v) {
      this._pairs.push([String(k), String(v)]);
    }
    set(k, v) {
      k = String(k);
      const first = this._pairs.findIndex(([pk]) => pk === k);
      this._pairs = this._pairs.filter(([pk]) => pk !== k);
      const entry = [k, String(v)];
      if (first < 0) this._pairs.push(entry);
      else this._pairs.splice(first, 0, entry);
    }
    get(k) {
      const hit = this._pairs.find(([pk]) => pk === String(k));
      return hit ? hit[1] : null;
    }
    getAll(k) {
      return this._pairs.filter(([pk]) => pk === String(k)).map(([, v]) => v);
    }
    has(k) {
      return this._pairs.some(([pk]) => pk === String(k));
    }
    delete(k) {
      this._pairs = this._pairs.filter(([pk]) => pk !== String(k));
    }
    forEach(fn) {
      for (const [k, v] of this._pairs) fn(v, k, this);
    }
    keys() {
      return this._pairs.map(([k]) => k)[Symbol.iterator]();
    }
    values() {
      return this._pairs.map(([, v]) => v)[Symbol.iterator]();
    }
    entries() {
      return this._pairs.map((p) => [...p])[Symbol.iterator]();
    }
    [Symbol.iterator]() {
      return this.entries();
    }
    get size() {
      return this._pairs.length;
    }
    toString() {
      return this._pairs
        .map(([k, v]) => encodeURIComponent(k) + "=" + encodeURIComponent(v))
        .join("&");
    }
  }
  g.URLSearchParams = URLSearchParams;
  Object.defineProperty(URL.prototype, "searchParams", {
    configurable: true,
    get() {
      return new URLSearchParams(this.search);
    },
  });

  // ---- location / navigator ----

  function makeLocation(href) {
    try {
      const url = new URL(href || "http://localhost/");
      url.assign = function () {};
      url.replace = function () {};
      url.reload = function () {};
      return url;
    } catch (_) {
      return { href: href || "", origin: "", protocol: "", host: "", hostname: "", port: "", pathname: "/", search: "", hash: "", assign() {}, replace() {}, reload() {}, toString() { return this.href; } };
    }
  }
  g.location = makeLocation(dom.baseUrl);
  document.location = g.location;
  g.navigator = {
    userAgent: "surl (browser-lite; like Gecko-not-at-all)",
    language: "en-US",
    languages: ["en-US"],
    platform: "surl",
    onLine: true,
  };
  g.history = {
    length: 1,
    state: null,
    pushState(state) {
      this.state = state;
    },
    replaceState(state) {
      this.state = state;
    },
    back() {},
    forward() {},
    go() {},
  };

  // ---- fetch:请求经 op 入队,宿主 settle 循环完成后经蹦床回调 ----

  const pendingFetches = new Map();

  class Headers {
    constructor(pairs) {
      this._map = new Map();
      if (Array.isArray(pairs)) {
        for (const [k, v] of pairs) this.append(k, v);
      } else if (pairs && typeof pairs === "object") {
        for (const k of Object.keys(pairs)) this.append(k, pairs[k]);
      }
    }
    append(k, v) {
      k = String(k).toLowerCase();
      const prev = this._map.get(k);
      this._map.set(k, prev === undefined ? String(v) : prev + ", " + String(v));
    }
    set(k, v) {
      this._map.set(String(k).toLowerCase(), String(v));
    }
    get(k) {
      const v = this._map.get(String(k).toLowerCase());
      return v === undefined ? null : v;
    }
    has(k) {
      return this._map.has(String(k).toLowerCase());
    }
    forEach(fn) {
      for (const [k, v] of this._map) fn(v, k, this);
    }
    entries() {
      return this._map.entries();
    }
    [Symbol.iterator]() {
      return this._map.entries();
    }
  }
  g.Headers = Headers;

  class Response {
    constructor(body, init) {
      init = init || {};
      this._bodyText = body == null ? "" : String(body);
      this.status = init.status === undefined ? 200 : init.status;
      this.statusText = init.statusText || "";
      this.headers = init.headers instanceof Headers ? init.headers : new Headers(init.headers);
      this.url = init.url || "";
      this.ok = this.status >= 200 && this.status < 300;
      this.redirected = false;
      this.type = "basic";
      this.bodyUsed = false;
    }
    text() {
      this.bodyUsed = true;
      return Promise.resolve(this._bodyText);
    }
    json() {
      this.bodyUsed = true;
      try {
        return Promise.resolve(JSON.parse(this._bodyText));
      } catch (e) {
        return Promise.reject(e);
      }
    }
    clone() {
      const r = new Response(this._bodyText, {
        status: this.status,
        statusText: this.statusText,
        url: this.url,
      });
      r.headers = this.headers;
      return r;
    }
  }
  g.Response = Response;

  g.fetch = function (input, init) {
    init = init || {};
    let url;
    try {
      url = new URL(String(input && input.url !== undefined ? input.url : input), g.location.href || undefined).href;
    } catch (e) {
      return Promise.reject(new TypeError("fetch: invalid URL: " + input));
    }
    const method = String(init.method || "GET").toUpperCase();
    const headerPairs = [];
    if (init.headers) {
      const h = init.headers instanceof Headers ? init.headers : new Headers(init.headers);
      h.forEach((v, k) => headerPairs.push([k, v]));
    }
    const hasBody = init.body != null;
    const body = hasBody ? String(init.body) : "";
    return new Promise((resolve, reject) => {
      const id = dom.fetchStart(url, method, headerPairs, hasBody, body);
      pendingFetches.set(id, { resolve, reject, url });
    });
  };

  // 宿主回调:done=true 时 (status, statusText, finalUrl, headerPairs, bodyText),
  // 失败时 errMessage 非空
  Object.defineProperty(g, "__surl_fetchDone", {
    enumerable: false,
    value: function (id, errMessage, status, statusText, finalUrl, headerPairs, bodyText) {
      const pending = pendingFetches.get(id);
      if (!pending) return;
      pendingFetches.delete(id);
      if (errMessage) {
        pending.reject(new TypeError("fetch failed: " + errMessage));
        return;
      }
      const resp = new Response(bodyText, {
        status,
        statusText,
        url: finalUrl,
        headers: headerPairs,
      });
      pending.resolve(resp);
    },
  });

  // ---- 环境垫片:真实 bundle(React/Vite/路由/UI 库)会摸的 Web API ----
  // 原则:能不崩、返回中性值;不假装有像素。

  class Storage {
    constructor() {
      this._data = new Map();
    }
    get length() {
      return this._data.size;
    }
    key(i) {
      return [...this._data.keys()][i] ?? null;
    }
    getItem(k) {
      const v = this._data.get(String(k));
      return v === undefined ? null : v;
    }
    setItem(k, v) {
      this._data.set(String(k), String(v));
    }
    removeItem(k) {
      this._data.delete(String(k));
    }
    clear() {
      this._data.clear();
    }
  }
  g.Storage = Storage;
  g.localStorage = new Storage();
  g.sessionStorage = new Storage();

  g.matchMedia = function (query) {
    return {
      matches: false,
      media: String(query),
      onchange: null,
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent() {
        return false;
      },
    };
  };

  class NoopObserver {
    constructor(callback) {
      this._callback = callback;
    }
    observe() {}
    unobserve() {}
    disconnect() {}
    takeRecords() {
      return [];
    }
  }
  g.IntersectionObserver = NoopObserver;
  g.ResizeObserver = NoopObserver;
  g.MutationObserver = NoopObserver;
  g.PerformanceObserver = NoopObserver;
  g.PerformanceObserver.supportedEntryTypes = [];

  g.getComputedStyle = function (el) {
    return {
      getPropertyValue() {
        return "";
      },
      display: "block",
      visibility: "visible",
      opacity: "1",
      pointerEvents: "auto",
    };
  };

  g.requestIdleCallback = function (fn) {
    return g.setTimeout(() => fn({ didTimeout: false, timeRemaining: () => 50 }), 1);
  };
  g.cancelIdleCallback = g.clearTimeout;

  // 布局几何:不渲染像素,一律零矩形
  const zeroRect = () => ({
    x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0,
    toJSON() { return this; },
  });
  Element.prototype.getBoundingClientRect = zeroRect;
  Element.prototype.getClientRects = function () {
    return [];
  };
  Element.prototype.scrollIntoView = function () {};
  Element.prototype.scrollTo = function () {};
  Element.prototype.focus = function () {};
  Element.prototype.blur = function () {};
  Element.prototype.click = function () {
    this.dispatchEvent(new Event("click", { bubbles: true, cancelable: true }));
  };
  for (const prop of ["offsetWidth", "offsetHeight", "offsetTop", "offsetLeft",
                      "clientWidth", "clientHeight", "clientTop", "clientLeft",
                      "scrollTop", "scrollLeft", "scrollWidth", "scrollHeight"]) {
    Object.defineProperty(Element.prototype, prop, {
      configurable: true,
      get() { return 0; },
      set() {},
    });
  }

  // React 等库对一批 DOM property 直接赋值(不走 setAttribute)
  const reflectedProps = {
    value: "value", checked: "checked", selected: "selected", disabled: "disabled",
    src: "src", href: "href", type: "type", name: "name", placeholder: "placeholder",
    htmlFor: "for", rel: "rel", target: "target", title: "title", lang: "lang",
    dir: "dir", alt: "alt", role: "role",
  };
  for (const [prop, attr] of Object.entries(reflectedProps)) {
    if (Object.getOwnPropertyDescriptor(Element.prototype, prop)) continue;
    const isBool = prop === "checked" || prop === "selected" || prop === "disabled";
    Object.defineProperty(Element.prototype, prop, {
      configurable: true,
      get() {
        const v = this.getAttribute(attr);
        return isBool ? v !== null : (v ?? "");
      },
      set(v) {
        if (isBool) {
          if (v) this.setAttribute(attr, "");
          else this.removeAttribute(attr);
        } else {
          this.setAttribute(attr, String(v));
        }
      },
    });
  }
  Object.defineProperty(Element.prototype, "tabIndex", {
    configurable: true,
    get() {
      const v = this.getAttribute("tabindex");
      return v === null ? -1 : Number(v) || 0;
    },
    set(v) {
      this.setAttribute("tabindex", String(v));
    },
  });
  Object.defineProperty(Element.prototype, "dataset", {
    configurable: true,
    get() {
      const el = this;
      return new Proxy({}, {
        get(_, prop) {
          if (typeof prop === "symbol") return undefined;
          const v = el.getAttribute("data-" + prop.replace(/[A-Z]/g, (m) => "-" + m.toLowerCase()));
          return v === null ? undefined : v;
        },
        set(_, prop, value) {
          el.setAttribute("data-" + String(prop).replace(/[A-Z]/g, (m) => "-" + m.toLowerCase()), String(value));
          return true;
        },
        has(_, prop) {
          return el.hasAttribute("data-" + String(prop).replace(/[A-Z]/g, (m) => "-" + m.toLowerCase()));
        },
      });
    },
  });
  Object.defineProperty(Node.prototype, "isConnected", {
    configurable: true,
    get() {
      return g.document.contains(this);
    },
  });

  // 现代插入 API(React/库常用)
  function toNode(x) {
    return x instanceof Node ? x : g.document.createTextNode(String(x));
  }
  Element.prototype.append = function (...items) {
    for (const item of items) this.appendChild(toNode(item));
  };
  Element.prototype.prepend = function (...items) {
    const first = this.firstChild;
    for (const item of items) this.insertBefore(toNode(item), first);
  };
  Element.prototype.before = function (...items) {
    const p = this.parentNode;
    if (p) for (const item of items) p.insertBefore(toNode(item), this);
  };
  Element.prototype.after = function (...items) {
    const p = this.parentNode;
    if (!p) return;
    const ref = this.nextSibling;
    for (const item of items) p.insertBefore(toNode(item), ref);
  };
  Element.prototype.replaceWith = function (...items) {
    const p = this.parentNode;
    if (!p) return;
    for (const item of items) p.insertBefore(toNode(item), this);
    p.removeChild(this);
  };
  Element.prototype.replaceChildren = function (...items) {
    this.textContent = "";
    for (const item of items) this.appendChild(toNode(item));
  };
  Element.prototype.insertAdjacentHTML = function (position, html) {
    const frag = g.document.createElement("template-host");
    frag.innerHTML = html;
    const nodes = [...frag.childNodes];
    if (position === "beforeend") for (const n of nodes) this.appendChild(n);
    else if (position === "afterbegin") {
      const first = this.firstChild;
      for (const n of nodes) this.insertBefore(n, first);
    } else if (position === "beforebegin") for (const n of nodes) this.parentNode && this.parentNode.insertBefore(n, this);
    else if (position === "afterend") {
      const ref = this.nextSibling;
      for (const n of nodes) this.parentNode && this.parentNode.insertBefore(n, ref);
    }
  };
  Element.prototype.insertAdjacentElement = function (position, el) {
    if (position === "beforeend") this.appendChild(el);
    else if (position === "afterbegin") this.insertBefore(el, this.firstChild);
    else if (position === "beforebegin" && this.parentNode) this.parentNode.insertBefore(el, this);
    else if (position === "afterend" && this.parentNode) this.parentNode.insertBefore(el, this.nextSibling);
    return el;
  };
  Element.prototype.insertAdjacentText = function (position, text) {
    this.insertAdjacentElement(position, g.document.createTextNode(String(text)));
  };

  // 文档级杂项
  let cookieJar = "";
  Object.defineProperty(g.document, "cookie", {
    configurable: true,
    get() {
      return cookieJar;
    },
    set(v) {
      // 只保留 name=value 段,足够让"能写能读"的代码活着
      const pair = String(v).split(";")[0];
      cookieJar = cookieJar ? cookieJar + "; " + pair : pair;
    },
  });
  Object.defineProperty(g.document, "activeElement", {
    configurable: true,
    get() {
      return g.document.body;
    },
  });
  Object.defineProperty(g.document, "currentScript", {
    configurable: true,
    get() {
      return null;
    },
  });
  g.document.hasFocus = () => true;
  g.document.createRange = function () {
    return {
      setStart() {}, setEnd() {}, collapse() {},
      selectNode() {}, selectNodeContents() {},
      deleteContents() {}, extractContents() {}, cloneContents() {},
      insertNode() {}, getBoundingClientRect: zeroRect,
      getClientRects() { return []; },
      createContextualFragment(html) {
        const host = g.document.createElement("div");
        host.innerHTML = html;
        const frag = g.document.createDocumentFragment();
        for (const n of [...host.childNodes]) frag.appendChild(n);
        return frag;
      },
      commonAncestorContainer: g.document.body,
    };
  };
  g.getSelection = () => ({
    rangeCount: 0,
    addRange() {}, removeAllRanges() {}, getRangeAt() { return null; },
    toString() { return ""; },
  });

  // 视口与滚动:固定 1280x720,滚动是 no-op
  g.innerWidth = 1280;
  g.innerHeight = 720;
  g.outerWidth = 1280;
  g.outerHeight = 720;
  g.devicePixelRatio = 1;
  g.scrollX = 0;
  g.scrollY = 0;
  g.pageXOffset = 0;
  g.pageYOffset = 0;
  g.scrollTo = function () {};
  g.scrollBy = function () {};
  g.scroll = function () {};
  g.screen = { width: 1280, height: 720, availWidth: 1280, availHeight: 720, colorDepth: 24, pixelDepth: 24 };
  g.alert = function () {};
  g.confirm = function () {
    return false;
  };
  g.prompt = function () {
    return null;
  };
  g.open = function () {
    return null;
  };
  g.CSS = {
    supports() {
      return false;
    },
    escape(s) {
      return String(s).replace(/[^a-zA-Z0-9_ -￿-]/g, (c) => "\\" + c);
    },
  };

  // base64(纯 JS;QuickJS 无 atob/btoa)
  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  g.btoa = function (input) {
    const s = String(input);
    let out = "";
    for (let i = 0; i < s.length; i += 3) {
      const c1 = s.charCodeAt(i), c2 = s.charCodeAt(i + 1), c3 = s.charCodeAt(i + 2);
      if (c1 > 255 || c2 > 255 || c3 > 255) throw new Error("btoa: invalid character");
      const n = (c1 << 16) | ((c2 || 0) << 8) | (c3 || 0);
      out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63]
        + (isNaN(c2) ? "=" : B64[(n >> 6) & 63])
        + (isNaN(c3) ? "=" : B64[n & 63]);
    }
    return out;
  };
  g.atob = function (input) {
    const s = String(input).replace(/=+$/, "");
    let out = "";
    let buffer = 0, bits = 0;
    for (const ch of s) {
      const idx = B64.indexOf(ch);
      if (idx < 0) continue;
      buffer = (buffer << 6) | idx;
      bits += 6;
      if (bits >= 8) {
        bits -= 8;
        out += String.fromCharCode((buffer >> bits) & 255);
      }
    }
    return out;
  };

  // TextEncoder/TextDecoder(UTF-8,纯 JS)
  class TextEncoder {
    get encoding() {
      return "utf-8";
    }
    encode(input) {
      const s = String(input ?? "");
      const bytes = [];
      for (const ch of s) {
        const cp = ch.codePointAt(0);
        if (cp < 0x80) bytes.push(cp);
        else if (cp < 0x800) bytes.push(0xc0 | (cp >> 6), 0x80 | (cp & 63));
        else if (cp < 0x10000) bytes.push(0xe0 | (cp >> 12), 0x80 | ((cp >> 6) & 63), 0x80 | (cp & 63));
        else bytes.push(0xf0 | (cp >> 18), 0x80 | ((cp >> 12) & 63), 0x80 | ((cp >> 6) & 63), 0x80 | (cp & 63));
      }
      return new Uint8Array(bytes);
    }
  }
  class TextDecoder {
    get encoding() {
      return "utf-8";
    }
    decode(input) {
      if (input == null) return "";
      const bytes = input instanceof Uint8Array ? input : new Uint8Array(input.buffer || input);
      let out = "";
      let i = 0;
      while (i < bytes.length) {
        const b = bytes[i++];
        let cp;
        if (b < 0x80) cp = b;
        else if (b < 0xe0) cp = ((b & 31) << 6) | (bytes[i++] & 63);
        else if (b < 0xf0) cp = ((b & 15) << 12) | ((bytes[i++] & 63) << 6) | (bytes[i++] & 63);
        else cp = ((b & 7) << 18) | ((bytes[i++] & 63) << 12) | ((bytes[i++] & 63) << 6) | (bytes[i++] & 63);
        out += String.fromCodePoint(cp);
      }
      return out;
    }
  }
  g.TextEncoder = TextEncoder;
  g.TextDecoder = TextDecoder;

  class AbortSignal extends EventTarget {
    constructor() {
      super();
      this.aborted = false;
      this.reason = undefined;
      this.onabort = null;
    }
    throwIfAborted() {
      if (this.aborted) throw this.reason;
    }
    static abort(reason) {
      const s = new AbortSignal();
      s.aborted = true;
      s.reason = reason;
      return s;
    }
    static timeout() {
      return new AbortSignal();
    }
  }
  class AbortController {
    constructor() {
      this.signal = new AbortSignal();
    }
    abort(reason) {
      if (this.signal.aborted) return;
      this.signal.aborted = true;
      this.signal.reason = reason === undefined ? new Error("AbortError") : reason;
      const ev = new Event("abort");
      this.signal.dispatchEvent(ev);
      if (typeof this.signal.onabort === "function") this.signal.onabort(ev);
    }
  }
  g.AbortSignal = AbortSignal;
  g.AbortController = AbortController;

  g.structuredClone = function (value) {
    return JSON.parse(JSON.stringify(value));
  };

  // 确定性 crypto:xorshift32 固定种子——可复现是产品契约,别换成真随机
  let rngState = 0x5f375a86;
  function nextByte() {
    rngState ^= rngState << 13;
    rngState ^= rngState >>> 17;
    rngState ^= rngState << 5;
    rngState >>>= 0;
    return rngState & 255;
  }
  g.crypto = {
    getRandomValues(arr) {
      for (let i = 0; i < arr.length; i++) arr[i] = nextByte();
      return arr;
    },
    randomUUID() {
      const hex = [];
      for (let i = 0; i < 16; i++) hex.push(nextByte().toString(16).padStart(2, "0"));
      return (
        hex.slice(0, 4).join("") + "-" + hex.slice(4, 6).join("") + "-" +
        hex.slice(6, 8).join("") + "-" + hex.slice(8, 10).join("") + "-" +
        hex.slice(10, 16).join("")
      );
    },
    subtle: undefined,
  };

  class DOMImplementation {
    createHTMLDocument() {
      return g.document;
    }
    hasFeature() {
      return true;
    }
  }
  Object.defineProperty(g.document, "implementation", {
    configurable: true,
    get() {
      return new DOMImplementation();
    },
  });

  // ---- 模块评估的失败跟踪(宿主在 load 结束时取走)----
  g.__surl_moduleFailures = [];
  Object.defineProperty(g, "__surl_trackModule", {
    enumerable: false,
    value: function (promise) {
      if (promise && typeof promise.catch === "function") {
        promise.catch((e) => {
          __surl_moduleFailures.push(String((e && (e.message || e.stack)) || e));
        });
      }
    },
  });

  function stringify(value) {
    if (typeof value === "string") return value;
    if (value instanceof Node) return "[object " + value.constructor.name + "]";
    try {
      const json = JSON.stringify(value);
      return json === undefined ? String(value) : json;
    } catch (_) {
      return String(value);
    }
  }
  function makeLog(level) {
    return function () {
      const parts = [];
      for (let i = 0; i < arguments.length; i++) parts.push(stringify(arguments[i]));
      dom.consoleLog(level, parts.join(" "));
    };
  }
  g.console = {
    log: makeLog("log"),
    info: makeLog("info"),
    debug: makeLog("debug"),
    warn: makeLog("warn"),
    error: makeLog("error"),
  };
})(globalThis);
