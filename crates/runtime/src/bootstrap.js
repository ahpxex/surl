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
    if (type === 1) node = new Element(id);
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

  g.EventTarget = EventTarget;
  g.Event = Event;
  g.CustomEvent = CustomEvent;
  g.Node = Node;
  g.CharacterData = CharacterData;
  g.Text = Text;
  g.Comment = Comment;
  g.Element = Element;
  g.HTMLElement = Element; // M1 粒度:不细分 HTML*Element 子类
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
