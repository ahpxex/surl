// surl bootstrap:在 QuickJS 全局里搭出 DOM 的 JS 面孔。
// 原则:所有真实状态在 Rust 侧(经 __surl_dom 句柄 op 访问),这里只有
// 包装对象与缓存。同一节点永远返回同一个包装对象(=== 身份,hydration 依赖)。
"use strict";
(function (g) {
  const dom = g.__surl_dom;

  // ---- 引擎 bug 止血(rquickjs-sys 0.12.1 vendor 的 quickjs-ng)----
  // Iterator.prototype.find/filter 的 C 实现对谓词不命中的元素漏 JS_FreeValue:
  // 每个被扫过的对象泄漏一个引用,teardown 时 JS_FreeRuntime 直接 assert
  // (astro.build 实测,quickjs.c 的 FIND 分支 `item = JS_UNDEFINED` 前没 free)。
  // 用 JS 实现覆盖;引擎升级修复后可删。
  {
    const IteratorProto = Object.getPrototypeOf(Object.getPrototypeOf([].values()));
    const closeIterator = (it) => {
      if (typeof it.return === "function") {
        try {
          it.return();
        } catch (_) {}
      }
    };
    Object.defineProperty(IteratorProto, "find", {
      configurable: true,
      writable: true,
      enumerable: false,
      value: function find(pred) {
        let i = 0;
        for (;;) {
          const r = this.next();
          if (r.done) return undefined;
          let matched;
          try {
            matched = pred(r.value, i++);
          } catch (e) {
            closeIterator(this);
            throw e;
          }
          if (matched) {
            closeIterator(this);
            return r.value;
          }
        }
      },
    });
    Object.defineProperty(IteratorProto, "filter", {
      configurable: true,
      writable: true,
      enumerable: false,
      value: function filter(pred) {
        const source = this;
        let i = 0;
        // 生成器天然满足惰性;finally 保证提前 close 时传播到底层迭代器
        return (function* () {
          try {
            for (;;) {
              const r = source.next();
              if (r.done) return;
              if (pred(r.value, i++)) yield r.value;
            }
          } finally {
            closeIterator(source);
          }
        })();
      },
    });
  }

  // id -> wrapper。arena 槽位不复用,句柄不会二义。
  const cache = new Map();

  function wrap(id) {
    if (!id) return null;
    let node = cache.get(id);
    if (node) return node;
    const type = dom.nodeType(id);
    if (type === 1) {
      const meta = dom.elementMeta(id);
      if (meta[0] === "http://www.w3.org/1999/xhtml") {
        node = new (elementClassFor(meta[2]))(id);
      } else if (meta[0] === "http://www.w3.org/2000/svg") {
        node = new SVGElement(id);
      } else {
        node = new Element(id);
      }
    } else if (type === 3) node = new Text(id);
    else if (type === 7) node = new ProcessingInstruction(id);
    else if (type === 8) node = new Comment(id);
    else if (type === 9) node = new DocumentNode(id);
    else if (type === 10) node = new DocumentType(id);
    else if (type === 11) node = new DocumentFragment(id);
    else node = new Node(id);
    cache.set(id, node);
    return node;
  }

  function unwrap(node, what) {
    if (node instanceof Node) return node._id;
    throw new TypeError((what || "argument") + " is not a Node");
  }

  // ---- DOMException ----

  const DOM_EXCEPTION_CODES = {
    IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4,
    InvalidCharacterError: 5, NoModificationAllowedError: 7, NotFoundError: 8,
    NotSupportedError: 9, InUseAttributeError: 10, InvalidStateError: 11,
    SyntaxError: 12, InvalidModificationError: 13, NamespaceError: 14,
    InvalidAccessError: 15, SecurityError: 18, NetworkError: 19, AbortError: 20,
    URLMismatchError: 21, QuotaExceededError: 22, TimeoutError: 23,
    InvalidNodeTypeError: 24, DataCloneError: 25,
  };
  class DOMException extends Error {
    constructor(message, name) {
      super(message === undefined ? "" : String(message));
      this.name = name === undefined ? "Error" : String(name);
    }
    get code() {
      return DOM_EXCEPTION_CODES[this.name] || 0;
    }
  }
  DOMException.INDEX_SIZE_ERR = 1;
  DOMException.HIERARCHY_REQUEST_ERR = 3;
  DOMException.INVALID_CHARACTER_ERR = 5;
  DOMException.NOT_FOUND_ERR = 8;
  DOMException.NOT_SUPPORTED_ERR = 9;
  DOMException.INVALID_STATE_ERR = 11;
  DOMException.SYNTAX_ERR = 12;
  g.DOMException = DOMException;

  // Rust op 用带前缀的 TypeError 报 DOM 错误;在 JS 边界翻译成 DOMException
  function rethrowDom(e) {
    if (e instanceof TypeError) {
      const m = /^([A-Z][A-Za-z]*Error): (.*)$/.exec(e.message || "");
      if (m && DOM_EXCEPTION_CODES[m[1]]) throw new DOMException(m[2], m[1]);
    }
    throw e;
  }

  // ---- 事件系统(纯 JS 侧:监听器不跨 FFI)----

  class Event {
    constructor(type, init) {
      init = init || {};
      this.type = String(type);
      this.bubbles = !!init.bubbles;
      this.cancelable = !!init.cancelable;
      this.composed = !!init.composed;
      this.target = null;
      this.currentTarget = null;
      this.eventPhase = 0;
      this._stopped = false;
      this._immediateStopped = false;
      this._canceled = false;
      this._initialized = true;
      this._dispatching = false;
      this.isTrusted = false;
      this.timeStamp = 0;
    }
    get defaultPrevented() {
      return this._canceled;
    }
    stopPropagation() {
      this._stopped = true;
    }
    stopImmediatePropagation() {
      this._stopped = true;
      this._immediateStopped = true;
    }
    preventDefault() {
      if (this.cancelable) this._canceled = true;
    }
    // 传统 API(规范仍要求)
    get cancelBubble() {
      return this._stopped;
    }
    set cancelBubble(v) {
      if (v) this._stopped = true;
    }
    get returnValue() {
      return !this._canceled;
    }
    set returnValue(v) {
      if (v === false) this.preventDefault();
    }
    initEvent(type, bubbles, cancelable) {
      if (this._dispatching) return;
      this._initialized = true;
      this._stopped = false;
      this._immediateStopped = false;
      this._canceled = false;
      this.isTrusted = false;
      this.target = null;
      this.type = String(type);
      this.bubbles = !!bubbles;
      this.cancelable = !!cancelable;
    }
    composedPath() {
      if (!this._dispatching || !this.target) return [];
      const path = [this.target];
      let p = this.target.parentNode ? this.target.parentNode : null;
      while (p) {
        path.push(p);
        p = p.parentNode;
      }
      return path;
    }
  }
  Event.NONE = 0;
  Event.CAPTURING_PHASE = 1;
  Event.AT_TARGET = 2;
  Event.BUBBLING_PHASE = 3;

  class CustomEvent extends Event {
    constructor(type, init) {
      super(type, init);
      this.detail = (init && init.detail) !== undefined ? init.detail : null;
    }
    initCustomEvent(type, bubbles, cancelable, detail) {
      this.initEvent(type, bubbles, cancelable);
      this.detail = detail === undefined ? null : detail;
    }
  }

  // 事件类家族(结构性:UI 事件不产生真实输入,类存在是为 instanceof 与
  // createEvent 的规范表)
  class UIEvent extends Event {
    constructor(type, init) {
      super(type, init);
      this.view = (init && init.view) || null;
      this.detail = (init && init.detail) || 0;
    }
    initUIEvent(type, bubbles, cancelable, view, detail) {
      this.initEvent(type, bubbles, cancelable);
      this.view = view || null;
      this.detail = detail || 0;
    }
  }
  class MouseEvent extends UIEvent {
    initMouseEvent(type, bubbles, cancelable) {
      this.initEvent(type, bubbles, cancelable);
    }
  }
  class KeyboardEvent extends UIEvent {
    initKeyboardEvent(type, bubbles, cancelable) {
      this.initEvent(type, bubbles, cancelable);
    }
  }
  class FocusEvent extends UIEvent {}
  class CompositionEvent extends UIEvent {
    initCompositionEvent(type, bubbles, cancelable) {
      this.initEvent(type, bubbles, cancelable);
    }
  }
  class TextEvent extends UIEvent {
    initTextEvent(type, bubbles, cancelable) {
      this.initEvent(type, bubbles, cancelable);
    }
  }
  class InputEvent extends UIEvent {}
  class DragEvent extends MouseEvent {}
  class PointerEvent extends MouseEvent {}
  class WheelEvent extends MouseEvent {}
  class MessageEvent extends Event {
    initMessageEvent(type, bubbles, cancelable) {
      this.initEvent(type, bubbles, cancelable);
    }
  }
  class StorageEvent extends Event {
    initStorageEvent(type, bubbles, cancelable) {
      this.initEvent(type, bubbles, cancelable);
    }
  }
  class HashChangeEvent extends Event {}
  class PopStateEvent extends Event {}
  class BeforeUnloadEvent extends Event {}
  class DeviceMotionEvent extends Event {}
  class DeviceOrientationEvent extends Event {}
  class ErrorEvent extends Event {}
  class ProgressEvent extends Event {}
  class TransitionEvent extends Event {}
  class AnimationEvent extends Event {}
  class PageTransitionEvent extends Event {}

  // createEvent 的规范别名表(比较不区分大小写)
  const CREATE_EVENT_TABLE = {
    beforeunloadevent: BeforeUnloadEvent,
    compositionevent: CompositionEvent,
    customevent: CustomEvent,
    devicemotionevent: DeviceMotionEvent,
    deviceorientationevent: DeviceOrientationEvent,
    dragevent: DragEvent,
    event: Event,
    events: Event,
    htmlevents: Event,
    svgevents: Event,
    focusevent: FocusEvent,
    hashchangeevent: HashChangeEvent,
    keyboardevent: KeyboardEvent,
    messageevent: MessageEvent,
    mouseevent: MouseEvent,
    mouseevents: MouseEvent,
    storageevent: StorageEvent,
    textevent: TextEvent,
    uievent: UIEvent,
    uievents: UIEvent,
  };
  for (const cls of [UIEvent, MouseEvent, KeyboardEvent, FocusEvent, CompositionEvent,
    TextEvent, InputEvent, DragEvent, PointerEvent, WheelEvent, MessageEvent,
    StorageEvent, HashChangeEvent, PopStateEvent, BeforeUnloadEvent,
    DeviceMotionEvent, DeviceOrientationEvent, ErrorEvent, ProgressEvent,
    TransitionEvent, AnimationEvent, PageTransitionEvent]) {
    g[cls.name] = cls;
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
      if (!(event instanceof Event)) {
        throw new TypeError("dispatchEvent: argument is not an Event");
      }
      if (event._dispatching || !event._initialized) {
        throw new DOMException(
          "The event is already being dispatched or has not been initialized",
          "InvalidStateError",
        );
      }
      event._dispatching = true;
      event.target = this;
      // 祖先链(仅 Node 有;window 单独处理)
      const path = [];
      if (this instanceof Node) {
        let p = this.parentNode;
        while (p) {
          path.push(p);
          p = p.parentNode;
        }
      }
      // capture 阶段:祖先(远→近),然后 target 上的 capture 监听器。
      // 现代规范:target 上两类监听器都以 AT_TARGET 相位触发,但 capture
      // 监听器属于 capture 遍,先于非 capture——顺序是可观测的。
      event.eventPhase = Event.CAPTURING_PHASE;
      for (let i = path.length - 1; i >= 0 && !event._stopped; i--) {
        event.currentTarget = path[i];
        path[i]._invokeListeners(event, 1);
      }
      if (!event._stopped) {
        event.eventPhase = Event.AT_TARGET;
        event.currentTarget = this;
        this._invokeListeners(event, 1);
      }
      if (!event._stopped) {
        event.eventPhase = Event.AT_TARGET;
        event.currentTarget = this;
        this._invokeListeners(event, 3);
      }
      if (event.bubbles) {
        event.eventPhase = Event.BUBBLING_PHASE;
        for (let i = 0; i < path.length && !event._stopped; i++) {
          event.currentTarget = path[i];
          path[i]._invokeListeners(event, 3);
        }
      }
      // 派发收尾:相位复位、传播标志清零(同一事件可再次派发)
      event.eventPhase = Event.NONE;
      event.currentTarget = null;
      event._dispatching = false;
      event._stopped = false;
      event._immediateStopped = false;
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
      if (this.nodeType === 9) return null;
      // 挂在树上:树根 document 即 owner;游离:记住创建时的 document
      const root = dom.rootDocument(this._id);
      if (root) return wrap(root);
      return this._ownerDoc || g.document || null;
    }
    hasChildNodes() {
      return dom.firstChild(this._id) !== 0;
    }
    appendChild(child) {
      try {
        dom.appendChild(this._id, unwrap(child, "child"));
      } catch (e) {
        rethrowDom(e);
      }
      return child;
    }
    insertBefore(node, reference) {
      // WebIDL:第二个参数不可省略(显式 null/undefined 允许)
      if (arguments.length < 2) {
        throw new TypeError("insertBefore: 2 arguments required");
      }
      try {
        dom.insertBefore(
          this._id,
          unwrap(node, "node"),
          reference == null ? 0 : unwrap(reference, "reference"),
        );
      } catch (e) {
        rethrowDom(e);
      }
      return node;
    }
    removeChild(child) {
      try {
        dom.removeChild(this._id, unwrap(child, "child"));
      } catch (e) {
        rethrowDom(e);
      }
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
    isSameNode(other) {
      return other === this;
    }
    // Next.js 的 head-manager 靠它对比 head 元素;按规范逐层结构比较
    isEqualNode(other) {
      if (!other || !(other instanceof Node)) return false;
      if (other === this) return true;
      if (other.nodeType !== this.nodeType) return false;
      switch (this.nodeType) {
        case 1: {
          if (
            this.namespaceURI !== other.namespaceURI ||
            this.prefix !== other.prefix ||
            this.localName !== other.localName
          ) {
            return false;
          }
          const a = this.attributes;
          const b = other.attributes;
          if (a.length !== b.length) return false;
          for (let i = 0; i < a.length; i++) {
            if (other.getAttribute(a[i].name) !== a[i].value) return false;
          }
          break;
        }
        case 3:
        case 8:
          if (this.nodeValue !== other.nodeValue) return false;
          break;
        case 7:
          if (this.target !== other.target || this.nodeValue !== other.nodeValue) return false;
          break;
        case 10:
          if (
            this.name !== other.name ||
            this.publicId !== other.publicId ||
            this.systemId !== other.systemId
          ) {
            return false;
          }
          break;
      }
      const c1 = this.childNodes;
      const c2 = other.childNodes;
      if (c1.length !== c2.length) return false;
      for (let i = 0; i < c1.length; i++) {
        if (!c1[i].isEqualNode(c2[i])) return false;
      }
      return true;
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
      const v = dom.textContent(this._id);
      return v == null ? null : v;
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
  class ProcessingInstruction extends CharacterData {
    get target() {
      return dom.nodeName(this._id);
    }
  }
  class DocumentType extends Node {
    get name() {
      return dom.nodeName(this._id);
    }
    get publicId() {
      return "";
    }
    get systemId() {
      return "";
    }
  }

  class Element extends Node {
    get tagName() {
      return dom.tagName(this._id);
    }
    get localName() {
      return dom.elementMeta(this._id)[2];
    }
    get namespaceURI() {
      return dom.elementMeta(this._id)[0] || null;
    }
    get prefix() {
      return dom.elementMeta(this._id)[1] || null;
    }
    get attributes() {
      // 活集合:React 的容器清理循环
      // `for(var t=e.attributes;t.length;)e.removeAttributeNode(t[0])`
      // 依赖 length/索引实时反映当前状态,快照数组会让它死循环
      if (!this._attrMap) {
        const el = this;
        const rowToAttr = (row) =>
          row
            ? {
                namespaceURI: row[0] || null,
                prefix: row[1] || null,
                localName: row[2],
                name: row[1] ? row[1] + ":" + row[2] : row[2],
                value: row[3],
                specified: true,
                ownerElement: el,
              }
            : null;
        const base = {
          item(i) {
            return rowToAttr(dom.attributes(el._id)[i] || null);
          },
          getNamedItem(name) {
            name = String(name).toLowerCase();
            const row = dom.attributes(el._id).find((r) => r[2] === name);
            return rowToAttr(row || null);
          },
          [Symbol.iterator]() {
            return dom.attributes(el._id).map(rowToAttr)[Symbol.iterator]();
          },
        };
        this._attrMap = new Proxy(base, {
          get(target, prop) {
            if (prop === "length") return dom.attributes(el._id).length;
            if (typeof prop === "string" && /^\d+$/.test(prop)) {
              return target.item(Number(prop)) ?? undefined;
            }
            const v = Reflect.get(target, prop, target);
            return typeof v === "function" ? v.bind(target) : v;
          },
          has(target, prop) {
            if (prop === "length") return true;
            if (typeof prop === "string" && /^\d+$/.test(prop)) {
              return Number(prop) < dom.attributes(el._id).length;
            }
            return Reflect.has(target, prop);
          },
        });
      }
      return this._attrMap;
    }
    getAttributeNode(name) {
      return this.attributes.getNamedItem(name);
    }
    removeAttributeNode(attr) {
      if (!attr || typeof attr.name !== "string") {
        throw new TypeError("removeAttributeNode: argument is not an Attr");
      }
      this.removeAttribute(attr.name);
      return attr;
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
      if (!this._classList) {
        const list = new DOMTokenList(this);
        // 索引访问(classList[0])经 Proxy;方法绑定到真实例上
        this._classList = new Proxy(list, {
          get(target, prop) {
            if (typeof prop === "string" && /^\d+$/.test(prop)) {
              const v = target.item(Number(prop));
              return v === null ? undefined : v;
            }
            const v = Reflect.get(target, prop, target);
            return typeof v === "function" ? v.bind(target) : v;
          },
          set(target, prop, value) {
            return Reflect.set(target, prop, value, target);
          },
          has(target, prop) {
            if (typeof prop === "string" && /^\d+$/.test(prop)) {
              return Number(prop) < target.length;
            }
            return Reflect.has(target, prop);
          },
        });
      }
      return this._classList;
    }
    set classList(v) {
      // [PutForwards=value]:赋值转发到 class 属性
      this.setAttribute("class", String(v));
    }
    get style() {
      if (!this._style) this._style = makeStyle(this);
      return this._style;
    }
  }

  // DOMTokenList(有序去重集,底层是 class 属性)
  function validateToken(token) {
    token = String(token);
    if (token === "") throw new DOMException("token is empty", "SyntaxError");
    if (/[\t\n\f\r ]/.test(token))
      throw new DOMException("token contains whitespace", "InvalidCharacterError");
    return token;
  }
  class DOMTokenList {
    constructor(el) {
      this._el = el;
    }
    _all() {
      const raw = this._el.getAttribute("class");
      if (raw == null) return [];
      const seen = [];
      for (const t of raw.split(/[\t\n\f\r ]+/)) {
        if (t && !seen.includes(t)) seen.push(t);
      }
      return seen;
    }
    _write(tokens) {
      // 规范 update steps:attribute 本就缺失且集合为空时,不凭空创建
      if (tokens.length === 0 && this._el.getAttribute("class") === null) return;
      this._el.setAttribute("class", tokens.join(" "));
    }
    get length() {
      return this._all().length;
    }
    item(i) {
      return this._all()[i] ?? null;
    }
    contains(token) {
      return this._all().includes(String(token));
    }
    add(...tokens) {
      const valid = tokens.map(validateToken);
      const all = this._all();
      for (const t of valid) if (!all.includes(t)) all.push(t);
      this._write(all);
    }
    remove(...tokens) {
      const drop = tokens.map(validateToken);
      this._write(this._all().filter((c) => !drop.includes(c)));
    }
    toggle(token, force) {
      token = validateToken(token);
      const has = this.contains(token);
      if (has) {
        if (force === undefined || force === false) {
          this.remove(token);
          return false;
        }
        return true;
      }
      if (force === undefined || force === true) {
        this.add(token);
        return true;
      }
      return false;
    }
    replace(oldToken, newToken) {
      // 规范:两个参数先一起查空串(SyntaxError),再查空白(InvalidCharacterError)
      oldToken = String(oldToken);
      newToken = String(newToken);
      if (oldToken === "" || newToken === "") {
        throw new DOMException("token is empty", "SyntaxError");
      }
      if (/[\t\n\f\r ]/.test(oldToken) || /[\t\n\f\r ]/.test(newToken)) {
        throw new DOMException("token contains whitespace", "InvalidCharacterError");
      }
      const all = this._all();
      const idx = all.indexOf(oldToken);
      if (idx < 0) return false;
      all[idx] = newToken;
      // 替换后去重(ordered set 语义)
      this._write(all.filter((t, i) => all.indexOf(t) === i));
      return true;
    }
    supports() {
      throw new TypeError("classList has no supported tokens");
    }
    get value() {
      return this._el.getAttribute("class") ?? "";
    }
    set value(v) {
      this._el.setAttribute("class", String(v));
    }
    toString() {
      return this.value;
    }
    forEach(fn, thisArg) {
      this._all().forEach((t, i) => fn.call(thisArg, t, i, this));
    }
    keys() {
      return this._all().keys();
    }
    values() {
      return this._all().values();
    }
    entries() {
      return this._all().entries();
    }
    [Symbol.iterator]() {
      return this._all().values();
    }
  }
  g.DOMTokenList = DOMTokenList;

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
      return wrap(dom.documentElement(this._id));
    }
    get body() {
      return wrap(dom.body(this._id));
    }
    get head() {
      return wrap(dom.head(this._id));
    }
    get doctype() {
      for (const child of this.childNodes) {
        if (child.nodeType === 10) return child;
      }
      return null;
    }
    get nodeName() {
      return "#document";
    }
    _adopt(node) {
      node._ownerDoc = this;
      return node;
    }
    getElementById(id) {
      return wrap(dom.getElementById(this._id, String(id)));
    }
    createElement(tag) {
      return this._adopt(wrap(dom.createElement(String(tag))));
    }
    createElementNS(ns, tag) {
      return this._adopt(wrap(dom.createElementNS(ns == null ? "" : String(ns), String(tag))));
    }
    createTextNode(text) {
      return this._adopt(wrap(dom.createText(String(text))));
    }
    createComment(text) {
      return this._adopt(wrap(dom.createComment(String(text))));
    }
    createProcessingInstruction(target, data) {
      return this._adopt(wrap(dom.createPI(String(target), String(data))));
    }
    createDocumentFragment() {
      return this._adopt(wrap(dom.createFragment()));
    }
    createCDATASection(data) {
      // XML 专属;结构上等同文本节点(不单设 CDATA 类型)
      return this._adopt(wrap(dom.createText(String(data))));
    }
    get characterSet() {
      return "UTF-8";
    }
    get charset() {
      return "UTF-8";
    }
    get inputEncoding() {
      return "UTF-8";
    }
    get contentType() {
      return this._contentType || "text/html";
    }
    createEvent(interfaceName) {
      const cls = CREATE_EVENT_TABLE[String(interfaceName).toLowerCase()];
      if (!cls) {
        throw new DOMException(
          "createEvent: unsupported interface " + interfaceName,
          "NotSupportedError",
        );
      }
      const ev = new cls("");
      // createEvent 造出的事件未初始化,须经 initEvent 才能派发
      ev._initialized = false;
      ev.type = "";
      return ev;
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
  // HTML 规范的 tag → 接口全表(与 WPT Node-cloneNode 的期望一致)
  const TAG_CLASS_NAMES = {
    a: "HTMLAnchorElement", abbr: "HTMLElement", acronym: "HTMLElement",
    address: "HTMLElement", area: "HTMLAreaElement", article: "HTMLElement",
    aside: "HTMLElement", audio: "HTMLAudioElement", b: "HTMLElement",
    base: "HTMLBaseElement", bdi: "HTMLElement", bdo: "HTMLElement",
    bgsound: "HTMLElement", big: "HTMLElement", blockquote: "HTMLElement",
    body: "HTMLBodyElement", br: "HTMLBRElement", button: "HTMLButtonElement",
    canvas: "HTMLCanvasElement", caption: "HTMLTableCaptionElement",
    center: "HTMLElement", cite: "HTMLElement", code: "HTMLElement",
    col: "HTMLTableColElement", colgroup: "HTMLTableColElement",
    data: "HTMLDataElement", datalist: "HTMLDataListElement",
    dd: "HTMLElement", del: "HTMLModElement", details: "HTMLElement",
    dfn: "HTMLElement", dialog: "HTMLDialogElement", dir: "HTMLDirectoryElement",
    div: "HTMLDivElement", dl: "HTMLDListElement", dt: "HTMLElement",
    embed: "HTMLEmbedElement", fieldset: "HTMLFieldSetElement",
    figcaption: "HTMLElement", figure: "HTMLElement", font: "HTMLFontElement",
    footer: "HTMLElement", form: "HTMLFormElement", frame: "HTMLFrameElement",
    frameset: "HTMLFrameSetElement", h1: "HTMLHeadingElement",
    h2: "HTMLHeadingElement", h3: "HTMLHeadingElement", h4: "HTMLHeadingElement",
    h5: "HTMLHeadingElement", h6: "HTMLHeadingElement", head: "HTMLHeadElement",
    header: "HTMLElement", hgroup: "HTMLElement", hr: "HTMLHRElement",
    html: "HTMLHtmlElement", i: "HTMLElement", iframe: "HTMLIFrameElement",
    img: "HTMLImageElement", input: "HTMLInputElement", ins: "HTMLModElement",
    isindex: "HTMLElement", kbd: "HTMLElement", label: "HTMLLabelElement",
    legend: "HTMLLegendElement", li: "HTMLLIElement", link: "HTMLLinkElement",
    main: "HTMLElement", map: "HTMLMapElement", mark: "HTMLElement",
    marquee: "HTMLElement", meta: "HTMLMetaElement", meter: "HTMLMeterElement",
    nav: "HTMLElement", nobr: "HTMLElement", noframes: "HTMLElement",
    noscript: "HTMLElement", object: "HTMLObjectElement", ol: "HTMLOListElement",
    optgroup: "HTMLOptGroupElement", option: "HTMLOptionElement",
    output: "HTMLOutputElement", p: "HTMLParagraphElement",
    param: "HTMLParamElement", pre: "HTMLPreElement",
    progress: "HTMLProgressElement", q: "HTMLQuoteElement", rp: "HTMLElement",
    rt: "HTMLElement", ruby: "HTMLElement", s: "HTMLElement",
    samp: "HTMLElement", script: "HTMLScriptElement", section: "HTMLElement",
    select: "HTMLSelectElement", small: "HTMLElement", source: "HTMLSourceElement",
    spacer: "HTMLElement", span: "HTMLSpanElement", strike: "HTMLElement",
    strong: "HTMLElement", style: "HTMLStyleElement", sub: "HTMLElement",
    summary: "HTMLElement", sup: "HTMLElement", table: "HTMLTableElement",
    tbody: "HTMLTableSectionElement", td: "HTMLTableCellElement",
    template: "HTMLTemplateElement", textarea: "HTMLTextAreaElement",
    tfoot: "HTMLTableSectionElement", th: "HTMLTableCellElement",
    thead: "HTMLTableSectionElement", time: "HTMLTimeElement",
    title: "HTMLTitleElement", tr: "HTMLTableRowElement",
    track: "HTMLTrackElement", tt: "HTMLElement", u: "HTMLElement",
    ul: "HTMLUListElement", var: "HTMLElement", video: "HTMLVideoElement",
    wbr: "HTMLElement",
  };
  const tagClassCache = new Map([["iframe", HTMLIFrameElement]]);
  function elementClassFor(tagName) {
    const tag = String(tagName).toLowerCase();
    let cls = tagClassCache.get(tag);
    if (cls) return cls;
    const name = TAG_CLASS_NAMES[tag];
    // 表外的 HTML 标签:含连字符按自定义元素给 HTMLElement,否则 Unknown
    cls = name ? g[name] : tag.includes("-") ? HTMLElement : g.HTMLUnknownElement;
    tagClassCache.set(tag, cls);
    return cls;
  }
  // 把类名挂到全局(同名标签共享一个类,如 h1-h6 / td-th)。
  // 注意顺序:先挂手写类,再补生成类,否则缓存里会是同名的另一个类。
  g.HTMLElement = HTMLElement;
  g.HTMLIFrameElement = HTMLIFrameElement;
  g.HTMLUnknownElement = class HTMLUnknownElement extends HTMLElement {};
  for (const name of new Set(Object.values(TAG_CLASS_NAMES))) {
    if (!g[name]) {
      // 具名 class 表达式,让报错信息里带上真实类名
      g[name] = { [name]: class extends HTMLElement {} }[name];
    }
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
  g.ProcessingInstruction = ProcessingInstruction;
  g.DocumentType = DocumentType;
  g.Element = Element;
  g.HTMLElement = HTMLElement;
  g.HTMLIFrameElement = HTMLIFrameElement;
  g.SVGElement = class SVGElement extends Element {};
  g.Document = DocumentNode;
  g.DocumentFragment = DocumentFragment;
  g.HTMLDocument = DocumentNode;

  g.document = wrap(dom.root());
  g.window = g;
  g.self = g;
  // 顶层窗口语义:没有 iframe 层级,parent/top 即自身,opener 为空
  g.parent = g;
  g.top = g;
  g.opener = null;
  g.frames = g;
  g.frameElement = null;
  g.closed = false;
  g.name = "";

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

  // ---- 动态插入的 <script>:webpack/Next.js 的 chunk 加载全靠它 ----
  // 浏览器语义:插入即执行。内联同步执行;外链经事件循环取回后执行,
  // 执行完派发 load(取回失败才是 error;执行抛错仍算 load)。
  // 限制:只看直接插入的 script 元素;随祖先子树整体连入的不追踪。
  function hostFetchText(url) {
    return new Promise((resolve, reject) => {
      const id = dom.fetchStart(url, "GET", [], false, "");
      pendingFetches.set(id, { resolve, reject, url });
    }).then((resp) => {
      if (!resp.ok) throw new TypeError("script HTTP " + resp.status);
      return resp.text();
    });
  }
  function fireScriptEvent(node, type) {
    const ev = new Event(type);
    const handler = node["on" + type];
    if (typeof handler === "function") {
      try {
        handler.call(node, ev);
      } catch (e) {
        console.error("script on" + type + " error:", e && e.message ? e.message : String(e));
      }
    }
    node.dispatchEvent(ev);
  }
  const JS_MIME = /^(?:text\/javascript|application\/(?:x-)?javascript|text\/ecmascript)$/i;
  function maybeRunInsertedScript(node) {
    if (!node || node.nodeType !== 1 || node.localName !== "script") return;
    // already-started 旗标:一个 script 至多执行一次(含被移动的情况)
    if (node.__surlScriptStarted || !node.isConnected) return;
    node.__surlScriptStarted = true;
    const type = (node.getAttribute("type") || "").trim();
    const src = node.getAttribute("src");
    let url = null;
    if (src != null && src !== "") {
      try {
        url = new URL(src, g.location.href || undefined).href;
      } catch (e) {
        fireScriptEvent(node, "error");
        return;
      }
    }
    if (/^module$/i.test(type)) {
      if (!url) return; // 动态内联 module:暂不支持
      import(url).then(
        () => fireScriptEvent(node, "load"),
        (e) => {
          console.error("dynamic module script error:", e && e.message ? e.message : String(e));
          fireScriptEvent(node, "error");
        },
      );
      return;
    }
    if (type && !JS_MIME.test(type)) return; // JSON/模板等数据块不执行
    if (url) {
      hostFetchText(url).then(
        (source) => {
          try {
            // 间接 eval:全局作用域 + sloppy mode,等价 classic script
            (0, eval)(source + "\n//# sourceURL=" + url);
          } catch (e) {
            console.error("dynamic script error:", e && e.message ? e.message : String(e));
          }
          fireScriptEvent(node, "load");
        },
        (e) => {
          console.error("dynamic script fetch error:", e && e.message ? e.message : String(e));
          fireScriptEvent(node, "error");
        },
      );
    } else {
      try {
        (0, eval)((node.textContent || "") + "\n//# sourceURL=surl:dynamic-inline");
      } catch (e) {
        console.error("dynamic inline script error:", e && e.message ? e.message : String(e));
      }
    }
  }

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

  // core-js 的 Promise 可用性检测要求全局有 PromiseRejectionEvent,
  // 缺了它会把原生 Promise 整个换成慢一个数量级的 polyfill(react.dev 实测)。
  class PromiseRejectionEvent extends Event {
    constructor(type, init) {
      super(type, init);
      this.promise = (init && init.promise) || null;
      this.reason = init && init.reason;
    }
  }
  g.PromiseRejectionEvent = PromiseRejectionEvent;

  // React scheduler 的首选调度通道。postMessage 是宏任务:走虚拟时钟。
  class MessagePort extends EventTarget {
    constructor() {
      super();
      this.onmessage = null;
      this._other = null;
    }
    postMessage(data) {
      const target = this._other;
      if (!target) return;
      g.setTimeout(() => {
        const ev = new MessageEvent("message");
        ev.data = data;
        if (typeof target.onmessage === "function") {
          try {
            target.onmessage(ev);
          } catch (e) {
            console.error("onmessage error:", e && e.message ? e.message : String(e));
          }
        }
        target.dispatchEvent(ev);
      }, 0);
    }
    start() {}
    close() {
      this._other = null;
    }
  }
  class MessageChannel {
    constructor() {
      this.port1 = new MessagePort();
      this.port2 = new MessagePort();
      this.port1._other = this.port2;
      this.port2._other = this.port1;
    }
  }
  g.MessagePort = MessagePort;
  g.MessageChannel = MessageChannel;

  // vuejs.org 等库做 `x instanceof SVGAnimatedString` 特征检查;
  // 我们的 SVG className 反射的是字符串,这里只要类存在、检查得到 false 即可。
  g.SVGAnimatedString = class SVGAnimatedString {
    constructor() {
      this.baseVal = "";
      this.animVal = "";
    }
  };

  // 刻意不垫 Intl(quickjs-ng 无 ICU)。2026-07-16 在 stripe.com 上实测过
  // 假 Intl 的后果:格式化输出与 SSR 的 ICU 结果不一致 → React hydration
  // mismatch(#418/#425)→ 整树卸载,SSR 内容全丢(619 行树变 16 行错误页)。
  // 没有 Intl 时 react-intl 在初始化即抛错,hydration 根本不启动,
  // SSR DOM 反而完整保留——对提取结构来说,早崩优于错误地假装会格式化。

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
  g.PerformanceObserver = NoopObserver;
  g.PerformanceObserver.supportedEntryTypes = [];

  // ---- MutationObserver(真实现)----
  // 我们的架构里 JS 对 DOM 的一切修改都必经 glue 层方法,
  // 在那些方法里同步入队记录、微任务统一派发,就是完整语义。

  const activeMutationObservers = new Set();
  let mutationDeliveryScheduled = false;

  function makeMutationRecord(type, target, extra) {
    return Object.assign(
      {
        type,
        target,
        addedNodes: [],
        removedNodes: [],
        previousSibling: null,
        nextSibling: null,
        attributeName: null,
        attributeNamespace: null,
        oldValue: null,
      },
      extra,
    );
  }

  function scheduleMutationDelivery() {
    if (mutationDeliveryScheduled) return;
    mutationDeliveryScheduled = true;
    Promise.resolve().then(function () {
      mutationDeliveryScheduled = false;
      for (const obs of [...activeMutationObservers]) {
        if (obs._records.length === 0) continue;
        const records = obs._records;
        obs._records = [];
        try {
          obs._callback(records, obs);
        } catch (e) {
          console.error("MutationObserver callback error:", e && e.message ? e.message : String(e));
        }
      }
    });
  }

  function notifyMutation(record) {
    for (const obs of activeMutationObservers) {
      for (const [target, opts] of obs._observed) {
        const isTarget = target === record.target;
        if (!isTarget && !(opts.subtree && target.contains(record.target))) continue;
        if (record.type === "attributes") {
          if (!opts.attributes) continue;
          if (opts.attributeFilter && !opts.attributeFilter.includes(record.attributeName)) continue;
        } else if (record.type === "characterData") {
          if (!opts.characterData) continue;
        } else if (record.type === "childList" && !opts.childList) {
          continue;
        }
        const copy = Object.assign({}, record);
        if (record.type === "attributes" && !opts.attributeOldValue) {
          copy.oldValue = null;
        }
        if (record.type === "characterData" && !opts.characterDataOldValue) {
          copy.oldValue = null;
        }
        obs._records.push(copy);
        scheduleMutationDelivery();
        break; // 同一 observer 每条变更只记一次
      }
    }
  }

  class MutationObserver {
    constructor(callback) {
      if (typeof callback !== "function") throw new TypeError("callback is not a function");
      this._callback = callback;
      this._records = [];
      this._observed = new Map();
    }
    observe(target, options) {
      options = options || {};
      // 规范:attributeOldValue / attributeFilter 出现即隐含 attributes
      const opts = {
        childList: !!options.childList,
        attributes:
          options.attributes !== undefined
            ? !!options.attributes
            : options.attributeOldValue !== undefined || options.attributeFilter !== undefined,
        characterData:
          options.characterData !== undefined
            ? !!options.characterData
            : options.characterDataOldValue !== undefined,
        subtree: !!options.subtree,
        attributeOldValue: !!options.attributeOldValue,
        characterDataOldValue: !!options.characterDataOldValue,
        attributeFilter: options.attributeFilter ? [...options.attributeFilter] : null,
      };
      if (!opts.childList && !opts.attributes && !opts.characterData) {
        throw new TypeError("observe: no mutation types requested");
      }
      this._observed.set(target, opts);
      activeMutationObservers.add(this);
    }
    disconnect() {
      this._observed.clear();
      this._records = [];
      activeMutationObservers.delete(this);
    }
    takeRecords() {
      const records = this._records;
      this._records = [];
      return records;
    }
  }
  g.MutationObserver = MutationObserver;

  // 埋点:attribute 变更
  const rawSetAttribute = Element.prototype.setAttribute;
  Element.prototype.setAttribute = function (attrName, value) {
    const key = String(attrName).toLowerCase();
    const oldValue = this.getAttribute(key);
    rawSetAttribute.call(this, attrName, value);
    if (activeMutationObservers.size) {
      notifyMutation(
        makeMutationRecord("attributes", this, { attributeName: key, oldValue }),
      );
    }
  };
  const rawRemoveAttribute = Element.prototype.removeAttribute;
  Element.prototype.removeAttribute = function (attrName) {
    const key = String(attrName).toLowerCase();
    const oldValue = this.getAttribute(key);
    rawRemoveAttribute.call(this, attrName);
    if (activeMutationObservers.size && oldValue !== null) {
      notifyMutation(
        makeMutationRecord("attributes", this, { attributeName: key, oldValue }),
      );
    }
  };

  // 埋点:树结构变更
  function notifyChildList(parent, added, removed, prev, next) {
    if (!activeMutationObservers.size) return;
    notifyMutation(
      makeMutationRecord("childList", parent, {
        addedNodes: added,
        removedNodes: removed,
        previousSibling: prev || null,
        nextSibling: next || null,
      }),
    );
  }
  const rawAppendChild = Node.prototype.appendChild;
  Node.prototype.appendChild = function (child) {
    const prev = this.lastChild;
    const result = rawAppendChild.call(this, child);
    notifyChildList(this, child instanceof DocumentFragment ? [] : [child], [], prev, null);
    maybeRunInsertedScript(child);
    return result;
  };
  const rawInsertBefore = Node.prototype.insertBefore;
  Node.prototype.insertBefore = function (node, reference) {
    // arguments 原样转发:参数个数检查在原方法里
    const result = rawInsertBefore.apply(this, arguments);
    notifyChildList(
      this,
      node instanceof DocumentFragment ? [] : [node],
      [],
      node.previousSibling,
      reference || null,
    );
    maybeRunInsertedScript(node);
    return result;
  };
  const rawRemoveChild = Node.prototype.removeChild;
  Node.prototype.removeChild = function (child) {
    const prev = child.previousSibling;
    const next = child.nextSibling;
    const result = rawRemoveChild.call(this, child);
    notifyChildList(this, [], [child], prev, next);
    return result;
  };

  // 埋点:文本数据变更
  const nodeValueDesc = Object.getOwnPropertyDescriptor(Node.prototype, "nodeValue");
  Object.defineProperty(Node.prototype, "nodeValue", {
    configurable: true,
    get: nodeValueDesc.get,
    set(v) {
      const oldValue = nodeValueDesc.get.call(this);
      nodeValueDesc.set.call(this, v);
      if (activeMutationObservers.size && (this.nodeType === 3 || this.nodeType === 8)) {
        notifyMutation(makeMutationRecord("characterData", this, { oldValue }));
      }
    },
  });

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
    // 规范:URL 类属性(src/href)的 property 反射出解析后的绝对地址,
    // attribute 保持原样(webpack 的 publicPath 推导依赖前者)
    const isUrl = prop === "src" || prop === "href";
    Object.defineProperty(Element.prototype, prop, {
      configurable: true,
      get() {
        const v = this.getAttribute(attr);
        if (isBool) return v !== null;
        if (v == null) return "";
        if (isUrl && v !== "") {
          try {
            return new URL(v, g.location.href).href;
          } catch (_) {
            return v;
          }
        }
        return v;
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
  // document.currentScript:宿主在执行每段 classic script 前后设置/清除。
  // webpack 等 chunk 加载器靠 currentScript.src 推导 publicPath。
  let currentScriptNode = null;
  Object.defineProperty(g.document, "currentScript", {
    configurable: true,
    get() {
      return currentScriptNode;
    },
  });
  Object.defineProperty(g, "__surl_setCurrentScript", {
    enumerable: false,
    value: function (id) {
      currentScriptNode = id ? wrap(id) : null;
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

  // XMLHttpRequest:架在 fetch 桥上的完整异步实现(sync 模式降级为异步并告警)
  class XMLHttpRequest extends EventTarget {
    constructor() {
      super();
      this.readyState = 0;
      this.status = 0;
      this.statusText = "";
      this.responseText = "";
      this.response = "";
      this.responseType = "";
      this.responseURL = "";
      this.timeout = 0;
      this.withCredentials = false;
      this.onreadystatechange = null;
      this.onload = null;
      this.onerror = null;
      this.onloadend = null;
      this.onloadstart = null;
      this.onabort = null;
      this.ontimeout = null;
      this._headers = [];
      this._respHeaders = null;
      this._aborted = false;
    }
    _fire(type) {
      const ev = new Event(type);
      ev.target = this;
      const handler = this["on" + type];
      if (typeof handler === "function") {
        try {
          handler.call(this, ev);
        } catch (e) {
          console.error("XHR on" + type + " error:", e && e.message ? e.message : String(e));
        }
      }
      this._invokeListeners(ev, 2);
    }
    _setState(state) {
      this.readyState = state;
      this._fire("readystatechange");
    }
    open(method, url, async) {
      if (async === false) {
        console.warn("XMLHttpRequest: sync mode unsupported, degrading to async");
      }
      this._method = String(method).toUpperCase();
      this._url = String(url);
      this._setState(1);
    }
    setRequestHeader(k, v) {
      this._headers.push([String(k), String(v)]);
    }
    send(body) {
      const xhr = this;
      this._fire("loadstart");
      fetch(this._url, {
        method: this._method || "GET",
        headers: this._headers,
        body: body == null ? undefined : body,
      })
        .then((resp) => resp.text().then((text) => ({ resp, text })))
        .then(({ resp, text }) => {
          if (xhr._aborted) return;
          xhr.status = resp.status;
          xhr.statusText = resp.statusText;
          xhr.responseURL = resp.url;
          xhr._respHeaders = resp.headers;
          xhr.responseText = text;
          xhr.response =
            xhr.responseType === "json"
              ? (() => {
                  try {
                    return JSON.parse(text);
                  } catch (_) {
                    return null;
                  }
                })()
              : text;
          xhr._setState(2);
          xhr._setState(3);
          xhr._setState(4);
          xhr._fire("load");
          xhr._fire("loadend");
        })
        .catch(() => {
          if (xhr._aborted) return;
          xhr._setState(4);
          xhr._fire("error");
          xhr._fire("loadend");
        });
    }
    abort() {
      this._aborted = true;
      this._setState(0);
      this._fire("abort");
      this._fire("loadend");
    }
    getResponseHeader(k) {
      return this._respHeaders ? this._respHeaders.get(k) : null;
    }
    getAllResponseHeaders() {
      if (!this._respHeaders) return "";
      const out = [];
      this._respHeaders.forEach((v, k) => out.push(k + ": " + v));
      return out.join("\r\n");
    }
    overrideMimeType() {}
  }
  XMLHttpRequest.UNSENT = 0;
  XMLHttpRequest.OPENED = 1;
  XMLHttpRequest.HEADERS_RECEIVED = 2;
  XMLHttpRequest.LOADING = 3;
  XMLHttpRequest.DONE = 4;
  g.XMLHttpRequest = XMLHttpRequest;

  // customElements:注册表语义(define/get/whenDefined),不做真实升级——
  // 自定义元素以 HTMLElement 形态存在于树里,结构提取不受影响
  {
    const registry = new Map();
    const waiting = new Map();
    g.customElements = {
      define(name, ctor) {
        name = String(name).toLowerCase();
        if (registry.has(name)) {
          throw new DOMException("customElements: '" + name + "' already defined", "NotSupportedError");
        }
        registry.set(name, ctor);
        const resolvers = waiting.get(name);
        if (resolvers) {
          waiting.delete(name);
          for (const resolve of resolvers) resolve(ctor);
        }
      },
      get(name) {
        return registry.get(String(name).toLowerCase());
      },
      getName(ctor) {
        for (const [n, c] of registry) if (c === ctor) return n;
        return null;
      },
      whenDefined(name) {
        name = String(name).toLowerCase();
        if (registry.has(name)) return Promise.resolve(registry.get(name));
        return new Promise((resolve) => {
          if (!waiting.has(name)) waiting.set(name, []);
          waiting.get(name).push(resolve);
        });
      },
      upgrade() {},
    };
  }

  // ReadableStream:够环境探测 + 空流消费的最小实现
  class ReadableStream {
    constructor() {
      this.locked = false;
    }
    getReader() {
      const stream = this;
      stream.locked = true;
      return {
        read() {
          return Promise.resolve({ done: true, value: undefined });
        },
        releaseLock() {
          stream.locked = false;
        },
        cancel() {
          return Promise.resolve();
        },
        closed: Promise.resolve(),
      };
    }
    cancel() {
      return Promise.resolve();
    }
    tee() {
      return [new ReadableStream(), new ReadableStream()];
    }
  }
  g.ReadableStream = ReadableStream;
  g.WritableStream = class WritableStream {
    getWriter() {
      return {
        write: () => Promise.resolve(),
        close: () => Promise.resolve(),
        abort: () => Promise.resolve(),
        releaseLock() {},
      };
    }
  };
  g.TransformStream = class TransformStream {
    constructor() {
      this.readable = new ReadableStream();
      this.writable = new g.WritableStream();
    }
  };

  // canvas.getContext:黑洞上下文。不渲染像素,但 WebGL/2d 初始化代码
  // 不该炸掉整棵组件树(根级 error boundary 会连正文一起吞)。
  // 黑洞语义:任意属性=自身、可调用(返回自身)、数值转换=0、恒真值。
  // 配合脚本墙钟预算,黑洞上的条件死循环也会被掐断。
  {
    const holeTarget = function () {};
    const blackHole = new Proxy(holeTarget, {
      get(t, prop) {
        if (prop === Symbol.toPrimitive) return () => 0;
        if (prop === "toString") return () => "";
        if (prop === "valueOf") return () => 0;
        if (prop === Symbol.iterator) {
          return function () {
            return { next: () => ({ done: true, value: undefined }) };
          };
        }
        return blackHole;
      },
      apply() {
        return blackHole;
      },
      construct() {
        return {};
      },
      set() {
        return true;
      },
      has() {
        return true;
      },
    });
    g.HTMLCanvasElement.prototype.getContext = function (type) {
      if (!this._contexts) this._contexts = new Map();
      let ctx = this._contexts.get(type);
      if (!ctx) {
        // canvas 属性要指回元素,其余交给黑洞
        const el = this;
        ctx = new Proxy(holeTarget, {
          get(t, prop) {
            if (prop === "canvas") return el;
            // 特殊键(toPrimitive/toString/...)拿到特殊实现,其余拿到黑洞自身
            return blackHole[prop];
          },
          apply() {
            return blackHole;
          },
          set() {
            return true;
          },
          has() {
            return true;
          },
        });
        this._contexts.set(type, ctx);
      }
      return ctx;
    };
    g.HTMLCanvasElement.prototype.toDataURL = function () {
      return "data:,";
    };
    g.HTMLCanvasElement.prototype.toBlob = function (cb) {
      if (typeof cb === "function") g.setTimeout(() => cb(null), 0);
    };
  }

  // WebSocket:环境探测级 stub——存在、可实例化、永不连接。
  // (Supabase realtime 等库检测不到构造器会直接 throw,炸掉整个组件树)
  class WebSocket extends EventTarget {
    constructor(url) {
      super();
      this.url = String(url);
      this.readyState = WebSocket.CONNECTING; // 永远停在 CONNECTING
      this.protocol = "";
      this.extensions = "";
      this.bufferedAmount = 0;
      this.binaryType = "blob";
      this.onopen = null;
      this.onclose = null;
      this.onerror = null;
      this.onmessage = null;
    }
    send() {}
    close() {
      this.readyState = WebSocket.CLOSED;
    }
  }
  WebSocket.CONNECTING = 0;
  WebSocket.OPEN = 1;
  WebSocket.CLOSING = 2;
  WebSocket.CLOSED = 3;
  WebSocket.prototype.CONNECTING = 0;
  WebSocket.prototype.OPEN = 1;
  WebSocket.prototype.CLOSING = 2;
  WebSocket.prototype.CLOSED = 3;
  g.WebSocket = WebSocket;

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
    createHTMLDocument(title) {
      // 同一 arena、另一棵树:见 ops 的 createHTMLDocument
      return wrap(dom.createHTMLDocument(title === undefined ? "" : String(title)));
    }
    createDocumentType(name) {
      return wrap(dom.createDoctype(String(name)));
    }
    createDocument(ns, qualifiedName, doctype) {
      const docNode = wrap(dom.createBareDocument());
      docNode._contentType = "application/xml";
      if (doctype) docNode.appendChild(doctype);
      if (qualifiedName) {
        docNode.appendChild(
          wrap(dom.createElementNS(ns == null ? "" : String(ns), String(qualifiedName))),
        );
      }
      return docNode;
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
    if (value instanceof Error) {
      return (value.name || "Error") + ": " + value.message + (value.stack ? "\n" + value.stack : "");
    }
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
