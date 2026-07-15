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
    else node = new Node(id);
    cache.set(id, node);
    return node;
  }

  function unwrap(node, what) {
    if (node instanceof Node) return node._id;
    throw new TypeError((what || "argument") + " is not a Node");
  }

  class Node {
    constructor(id) {
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
    createTextNode(text) {
      return wrap(dom.createText(String(text)));
    }
    createComment(text) {
      return wrap(dom.createComment(String(text)));
    }
  }

  g.Node = Node;
  g.CharacterData = CharacterData;
  g.Text = Text;
  g.Comment = Comment;
  g.Element = Element;
  g.HTMLElement = Element; // M1 粒度:不细分 HTML*Element 子类
  g.Document = DocumentNode;

  g.document = wrap(dom.root());
  g.window = g;
  g.self = g;

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
