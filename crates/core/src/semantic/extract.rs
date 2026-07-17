//! DOM → 语义树提取:HTML-AAM 的实用子集。
//!
//! 有意的 M0 简化(与规范的偏差,golden corpus 显示伤害大再修):
//! - header/footer 一律映射 banner/contentinfo,不检查是否嵌在 article/section 里;
//! - form 一律成节点(规范要求有可及名才有 form role);
//! - 可见性只看 `hidden` 属性与 `aria-hidden="true"`,不做样式(范围边界);
//! - accname 是够用子集:aria-label > aria-labelledby > 原生来源 > 内容 > title。

use std::collections::HashMap;

use surl_dom::{Document, ElementData, NodeData, NodeId};
use url::Url;

use super::ir::{Role, SemanticNode, Snapshot, State};

/// 从解析后的文档提取语义树。`base` 用于把相对 href 解析成绝对地址。
pub fn extract(doc: &Document, base: Option<&Url>) -> Snapshot {
    let mut ex = Extractor {
        doc,
        base,
        ids: HashMap::new(),
        labels: HashMap::new(),
    };
    ex.build_maps();

    let title = ex.find_title();
    let mut root = SemanticNode::new(Role::Document);
    root.name = title.clone();
    if let Some(html) = doc.document_element() {
        ex.walk_children(html, &mut root.children, false);
    }
    root.uid = "root".to_owned();
    crate::semantic::ir::assign_stable_uids(&mut root, 0);
    Snapshot {
        url: base.map(|u| u.to_string()),
        title,
        root,
    }
}

/// 这些标签整棵子树不进语义树。
fn is_skipped_tag(tag: &str) -> bool {
    matches!(
        tag,
        "head"
            | "script"
            | "style"
            | "template"
            | "noscript"
            | "link"
            | "meta"
            | "title"
            | "base"
            | "br"
            | "wbr"
            | "datalist"
            | "source"
            | "track"
            | "param"
            | "object"
            | "embed"
            | "audio"
            | "video"
            | "canvas"
            | "map"
            | "slot"
    )
}

/// 一个元素在语义树里的三种归宿。
enum Disposition {
    /// 成为一个语义节点
    Emit(Role),
    /// 自身消失,孩子上提(div/span 等无语义容器)
    Flatten,
    /// 整棵子树丢弃(装饰性 img 等)
    Skip,
}

struct Extractor<'a> {
    doc: &'a Document,
    base: Option<&'a Url>,
    /// id → 元素,供 aria-labelledby 解析
    ids: HashMap<String, NodeId>,
    /// 目标控件 id → `<label for=…>` 元素
    labels: HashMap<String, NodeId>,
}

impl Extractor<'_> {
    fn build_maps(&mut self) {
        for id in self.doc.descendants(self.doc.root()) {
            let Some(el) = self.doc.element(id) else {
                continue;
            };
            if let Some(elem_id) = el.attr("id") {
                self.ids.entry(elem_id.to_owned()).or_insert(id);
            }
            if el.is_html_element("label")
                && let Some(target) = el.attr("for")
            {
                self.labels.entry(target.to_owned()).or_insert(id);
            }
        }
    }

    fn find_title(&self) -> Option<String> {
        let title = self.doc.descendants(self.doc.root()).find(|&n| {
            self.doc
                .element(n)
                .is_some_and(|el| el.is_html_element("title"))
        })?;
        non_empty(collapse_ws(&self.doc.text_content(title)))
    }

    /// 把 parent 的孩子逐个走进 out。`suppress_text`:祖先已把内容取作名字,
    /// 裸文本不再单独成节点。
    fn walk_children(&mut self, parent: NodeId, out: &mut Vec<SemanticNode>, suppress_text: bool) {
        for &child in &self.doc.node(parent).children {
            self.walk(child, out, suppress_text);
        }
    }

    fn walk(&mut self, id: NodeId, out: &mut Vec<SemanticNode>, suppress_text: bool) {
        match &self.doc.node(id).data {
            NodeData::Text { contents } => {
                if suppress_text {
                    return;
                }
                if let Some(text) = non_empty(collapse_ws(contents)) {
                    let mut node = SemanticNode::new(Role::Text);
                    node.name = Some(text);
                    out.push(node);
                }
            }
            NodeData::Element(el) => self.walk_element(id, el, out, suppress_text),
            _ => {}
        }
    }

    fn walk_element(
        &mut self,
        id: NodeId,
        el: &ElementData,
        out: &mut Vec<SemanticNode>,
        suppress_text: bool,
    ) {
        let tag = el.local_name().as_ref();
        if is_skipped_tag(tag) || is_hidden(el) {
            return;
        }

        // svg 子树是绘图指令,整体折叠成一张“图”
        if tag == "svg" {
            let mut node = SemanticNode::new(Role::Img);
            node.name = self.svg_name(id, el);
            out.push(node);
            return;
        }

        match self.disposition(id, el) {
            Disposition::Skip => {}
            Disposition::Flatten => self.walk_children(id, out, suppress_text),
            Disposition::Emit(role) => {
                let mut node = SemanticNode::new(role);
                node.name = self.accessible_name(id, el, role);
                node.level = heading_level(el, tag, role);
                node.href = self.resolve_href(el, role);
                node.value = element_value(el, role);
                node.state = element_state(el);
                let suppress = role.names_from_contents();
                // iframe 的内容是另一份文档,M0 不进去
                if role != Role::Iframe {
                    self.walk_children(id, &mut node.children, suppress);
                }
                out.push(node);
            }
        }
    }

    /// 显式 role 属性优先,否则按标签的隐式映射。
    fn disposition(&self, id: NodeId, el: &ElementData) -> Disposition {
        if let Some(explicit) = el.attr("role") {
            // role 可以是空格分隔的候选列表,取第一个认识的
            for token in explicit.split_ascii_whitespace() {
                if matches!(token, "presentation" | "none" | "generic") {
                    return Disposition::Flatten;
                }
                if let Some(role) = aria_role(token) {
                    return Disposition::Emit(role);
                }
            }
        }
        self.implicit_disposition(id, el)
    }

    fn implicit_disposition(&self, id: NodeId, el: &ElementData) -> Disposition {
        use Disposition::{Emit, Flatten, Skip};
        let role = match el.local_name().as_ref() {
            "header" => Role::Banner,
            "nav" => Role::Navigation,
            "main" => Role::Main,
            "aside" => Role::Complementary,
            "footer" => Role::ContentInfo,
            "search" => Role::Search,
            "form" => Role::Form,
            "article" => Role::Article,
            "section" => {
                // 规范:section 有可及名才是 region,否则是无语义容器
                if self.label_from_aria(id, el).is_some() {
                    Role::Region
                } else {
                    return Flatten;
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Role::Heading,
            "p" => Role::Paragraph,
            "blockquote" => Role::Blockquote,
            "ul" | "ol" | "menu" => Role::List,
            "li" => Role::ListItem,
            "table" => Role::Table,
            // caption 的文本已作为 table 的可及名,不再重复成内容节点
            "caption" => return Skip,
            "tr" => Role::Row,
            "td" => Role::Cell,
            "th" => {
                if el.attr("scope") == Some("row") {
                    Role::RowHeader
                } else {
                    Role::ColumnHeader
                }
            }
            "figure" => Role::Figure,
            "hr" => Role::Separator,
            "img" => match el.attr("alt") {
                // alt="" 是明确的“装饰性图片”声明
                Some("") => return Skip,
                _ => Role::Img,
            },
            "a" => {
                if el.attr("href").is_some() {
                    Role::Link
                } else {
                    return Flatten;
                }
            }
            "button" => Role::Button,
            "input" => match el.attr("type").unwrap_or("text") {
                "hidden" => return Skip,
                "checkbox" => Role::CheckBox,
                "radio" => Role::Radio,
                "range" => Role::Slider,
                "search" => Role::SearchBox,
                "button" | "submit" | "reset" | "image" => Role::Button,
                _ => Role::TextBox,
            },
            "select" => {
                if el.attr("multiple").is_some() {
                    Role::ListBox
                } else {
                    Role::ComboBox
                }
            }
            "textarea" => Role::TextBox,
            "option" => Role::OptionItem,
            "progress" => Role::ProgressBar,
            "dialog" => Role::Dialog,
            "iframe" => Role::Iframe,
            _ => return Flatten,
        };
        Emit(role)
    }

    // ---- 可及名 ----

    /// aria-label / aria-labelledby(accname 的前两级)。
    fn label_from_aria(&self, _id: NodeId, el: &ElementData) -> Option<String> {
        if let Some(label) = el.attr("aria-label")
            && let Some(label) = non_empty(collapse_ws(label))
        {
            return Some(label);
        }
        if let Some(idrefs) = el.attr("aria-labelledby") {
            let text: Vec<String> = idrefs
                .split_ascii_whitespace()
                .filter_map(|idref| self.ids.get(idref))
                .filter_map(|&n| non_empty(collapse_ws(&self.visible_text(n))))
                .collect();
            if !text.is_empty() {
                return Some(text.join(" "));
            }
        }
        None
    }

    fn accessible_name(&self, id: NodeId, el: &ElementData, role: Role) -> Option<String> {
        if let Some(name) = self.label_from_aria(id, el) {
            return Some(name);
        }
        // 原生来源
        let tag = el.local_name().as_ref();
        match tag {
            "img" | "area" => {
                if let Some(name) = el.attr("alt").and_then(|v| non_empty(collapse_ws(v))) {
                    return Some(name);
                }
            }
            "input" => {
                if role == Role::Button {
                    // <input type=submit value=…>
                    if let Some(name) = el.attr("value").and_then(|v| non_empty(collapse_ws(v))) {
                        return Some(name);
                    }
                }
            }
            "table" => {
                if let Some(caption) = self.find_child_tag(id, "caption")
                    && let Some(name) = non_empty(collapse_ws(&self.visible_text(caption)))
                {
                    return Some(name);
                }
            }
            "iframe" => {
                if let Some(name) = el.attr("title").and_then(|v| non_empty(collapse_ws(v))) {
                    return Some(name);
                }
            }
            _ => {}
        }
        // 表单控件:<label for=…> > placeholder
        if is_labelable_control(role) {
            if let Some(elem_id) = el.attr("id")
                && let Some(&label) = self.labels.get(elem_id)
                && let Some(name) = non_empty(collapse_ws(&self.visible_text(label)))
            {
                return Some(name);
            }
            if let Some(name) = el.attr("placeholder").and_then(|v| non_empty(collapse_ws(v))) {
                return Some(name);
            }
        }
        // 内容即名字(链接文字、按钮文字…)
        if role.names_from_contents()
            && let Some(name) = non_empty(collapse_ws(&self.visible_text(id)))
        {
            return Some(name);
        }
        // 最后的兜底:title 属性
        el.attr("title").and_then(|v| non_empty(collapse_ws(v)))
    }

    /// 子树里的可见文本:跳过 script/style 等和隐藏元素。
    fn visible_text(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_visible_text(id, &mut out);
        out
    }

    fn collect_visible_text(&self, id: NodeId, out: &mut String) {
        match &self.doc.node(id).data {
            NodeData::Text { contents } => out.push_str(contents),
            NodeData::Element(el) => {
                if is_skipped_tag(el.local_name().as_ref()) || is_hidden(el) {
                    return;
                }
                for &child in &self.doc.node(id).children {
                    self.collect_visible_text(child, out);
                }
                // img 的 alt 参与内容文本(链接里只有一张图的常见情形)
                if el.is_html_element("img")
                    && let Some(alt) = el.attr("alt")
                {
                    out.push(' ');
                    out.push_str(alt);
                    out.push(' ');
                }
            }
            _ => {}
        }
    }

    fn svg_name(&self, id: NodeId, el: &ElementData) -> Option<String> {
        if let Some(name) = self.label_from_aria(id, el) {
            return Some(name);
        }
        // svg 的 <title> 子元素
        if let Some(title) = self.find_child_tag(id, "title") {
            return non_empty(collapse_ws(&self.doc.text_content(title)));
        }
        None
    }

    fn find_child_tag(&self, id: NodeId, tag: &str) -> Option<NodeId> {
        self.doc
            .node(id)
            .children
            .iter()
            .copied()
            .find(|&c| self.doc.element(c).is_some_and(|el| *el.local_name() == *tag))
    }

    fn resolve_href(&self, el: &ElementData, role: Role) -> Option<String> {
        let raw = match role {
            Role::Link => el.attr("href")?,
            Role::Iframe => el.attr("src")?,
            _ => return None,
        };
        match self.base {
            Some(base) => match base.join(raw) {
                Ok(abs) => Some(abs.to_string()),
                Err(_) => Some(raw.to_owned()),
            },
            None => Some(raw.to_owned()),
        }
    }
}

fn is_hidden(el: &ElementData) -> bool {
    el.attr("hidden").is_some() || el.attr("aria-hidden") == Some("true")
}

fn is_labelable_control(role: Role) -> bool {
    matches!(
        role,
        Role::TextBox
            | Role::SearchBox
            | Role::CheckBox
            | Role::Radio
            | Role::Slider
            | Role::ComboBox
            | Role::ListBox
            | Role::ProgressBar
    )
}

fn heading_level(el: &ElementData, tag: &str, role: Role) -> Option<u8> {
    if role != Role::Heading {
        return None;
    }
    if let Some(level) = el.attr("aria-level").and_then(|v| v.parse().ok()) {
        return Some(level);
    }
    match tag {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => Some(2), // 规范:role=heading 无 aria-level 时默认 2
    }
}

fn element_value(el: &ElementData, role: Role) -> Option<String> {
    match role {
        Role::TextBox | Role::SearchBox | Role::Slider | Role::ProgressBar => {
            el.attr("value").map(str::to_owned)
        }
        _ => None,
    }
}

fn element_state(el: &ElementData) -> State {
    State {
        disabled: el.attr("disabled").is_some() || el.attr("aria-disabled") == Some("true"),
        checked: match (el.attr("checked"), el.attr("aria-checked")) {
            (Some(_), _) | (_, Some("true")) => Some(true),
            (_, Some("false")) => Some(false),
            _ => {
                // checkbox/radio 未勾选也值得显式表达
                if matches!(el.attr("type"), Some("checkbox") | Some("radio")) {
                    Some(false)
                } else {
                    None
                }
            }
        },
        selected: (el.attr("selected").is_some() || el.attr("aria-selected") == Some("true"))
            .then_some(true),
        expanded: el.attr("aria-expanded").and_then(|v| match v {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }),
    }
}

/// 显式 role 属性 → Role 的映射(认识的子集)。
fn aria_role(token: &str) -> Option<Role> {
    Some(match token {
        "banner" => Role::Banner,
        "navigation" => Role::Navigation,
        "main" => Role::Main,
        "complementary" => Role::Complementary,
        "contentinfo" => Role::ContentInfo,
        "search" => Role::Search,
        "form" => Role::Form,
        "region" => Role::Region,
        "article" => Role::Article,
        "heading" => Role::Heading,
        "paragraph" => Role::Paragraph,
        "blockquote" => Role::Blockquote,
        "list" => Role::List,
        "listitem" => Role::ListItem,
        "table" | "grid" => Role::Table,
        "row" => Role::Row,
        "cell" | "gridcell" => Role::Cell,
        "columnheader" => Role::ColumnHeader,
        "rowheader" => Role::RowHeader,
        "figure" => Role::Figure,
        "separator" => Role::Separator,
        "img" | "image" => Role::Img,
        "link" => Role::Link,
        "button" => Role::Button,
        "textbox" => Role::TextBox,
        "searchbox" => Role::SearchBox,
        "checkbox" => Role::CheckBox,
        "radio" => Role::Radio,
        "slider" => Role::Slider,
        "combobox" => Role::ComboBox,
        "listbox" => Role::ListBox,
        "option" => Role::OptionItem,
        "progressbar" => Role::ProgressBar,
        "dialog" | "alertdialog" => Role::Dialog,
        _ => return None,
    })
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surl_dom::parse_html;

    fn snap(html: &str) -> Snapshot {
        let doc = parse_html(html);
        extract(&doc, None)
    }

    fn tree(html: &str) -> String {
        snap(html).to_tree_string()
    }

    #[test]
    fn landmarks_headings_links() {
        let out = tree(concat!(
            "<!doctype html><title>Demo</title>",
            "<header><h1>Site</h1><nav><a href='/a'>A</a><a href='/b'>B</a></nav></header>",
            "<main><h2>Body</h2><p>hello world</p></main>",
            "<footer>© 2026</footer>",
        ));
        assert_eq!(
            out,
            r#"document "Demo"
  banner
    heading[1] "Site"
    navigation
      link "A" -> /a
      link "B" -> /b
  main
    heading[2] "Body"
    paragraph
      text "hello world"
  contentinfo
    text "© 2026"
"#
        );
    }

    #[test]
    fn generic_divs_flatten() {
        let out = tree("<!doctype html><div><div><span><a href='/x'>go</a></span></div></div>");
        assert_eq!(out, "document\n  link \"go\" -> /x\n");
    }

    #[test]
    fn link_text_suppressed_but_nested_elements_kept() {
        // 链接名来自内容,裸文本不再重复;但嵌套 img 仍是子节点
        let out = tree(r#"<!doctype html><a href="/x">click <img src=i.png alt="icon"></a>"#);
        assert_eq!(
            out,
            "document\n  link \"click icon\" -> /x\n    img \"icon\"\n"
        );
    }

    #[test]
    fn hidden_subtrees_pruned() {
        let out = tree(concat!(
            "<!doctype html>",
            "<div hidden><a href='/x'>no</a></div>",
            "<div aria-hidden='true'><p>no</p></div>",
            "<input type=hidden value=csrf>",
            "<p>yes</p>",
        ));
        assert_eq!(out, "document\n  paragraph\n    text \"yes\"\n");
    }

    #[test]
    fn explicit_role_overrides() {
        let out = tree(concat!(
            "<!doctype html>",
            "<div role='navigation' aria-label='Crumbs'><a href='/'>Home</a></div>",
            "<ul role='presentation'><li role='none'>x</li></ul>",
        ));
        assert_eq!(
            out,
            "document\n  navigation \"Crumbs\"\n    link \"Home\" -> /\n  text \"x\"\n"
        );
    }

    #[test]
    fn accname_priority() {
        // aria-label 胜过内容
        let out = tree(r#"<!doctype html><button aria-label="Close">×</button>"#);
        assert_eq!(out, "document\n  button \"Close\"\n");
        // aria-labelledby 取引用元素文本
        let out = tree(concat!(
            "<!doctype html>",
            "<span id=t>Search the site</span>",
            "<input aria-labelledby=t>",
        ));
        assert!(out.contains("textbox \"Search the site\""));
    }

    #[test]
    fn form_controls() {
        let out = tree(concat!(
            "<!doctype html>",
            "<label for=e>Email</label><input id=e type=email placeholder=you@x.dev>",
            "<input type=checkbox checked id=c><label for=c>Agree</label>",
            "<select><option selected>One</option><option>Two</option></select>",
            "<button disabled>Send</button>",
        ));
        assert!(out.contains("textbox \"Email\""));
        assert!(out.contains("checkbox \"Agree\" (checked)"));
        assert!(out.contains("combobox\n"));
        assert!(out.contains("option \"One\" (selected)"));
        assert!(out.contains("option \"Two\"\n"));
        assert!(out.contains("button \"Send\" (disabled)"));
    }

    #[test]
    fn section_needs_name_for_region() {
        let out = tree(concat!(
            "<!doctype html>",
            "<section><p>anonymous</p></section>",
            "<section aria-label='News'><p>named</p></section>",
        ));
        assert!(!out.contains("region\n"));
        assert!(out.contains("region \"News\""));
    }

    #[test]
    fn decorative_img_skipped() {
        let out = tree(r#"<!doctype html><img src=a.png alt=""><img src=b.png alt="Logo">"#);
        assert_eq!(out, "document\n  img \"Logo\"\n");
    }

    #[test]
    fn table_with_caption() {
        let out = tree(concat!(
            "<!doctype html><table><caption>Prices</caption>",
            "<tr><th>Item</th><th scope=row>Cost</th></tr>",
            "<tr><td>Tea</td></tr></table>",
        ));
        assert!(out.contains("table \"Prices\""));
        assert!(out.contains("columnheader \"Item\""));
        assert!(out.contains("rowheader \"Cost\""));
        assert!(out.contains("cell \"Tea\""));
    }

    #[test]
    fn svg_collapses_to_img() {
        let out = tree("<!doctype html><svg><title>Chart</title><rect/><path/></svg>");
        assert_eq!(out, "document\n  img \"Chart\"\n");
    }

    #[test]
    fn href_resolved_against_base() {
        let doc = parse_html(r#"<!doctype html><a href="/docs">Docs</a>"#);
        let base = Url::parse("https://example.com/page/").unwrap();
        let snapshot = extract(&doc, Some(&base));
        assert_eq!(
            snapshot.root.children[0].href.as_deref(),
            Some("https://example.com/docs")
        );
        assert_eq!(snapshot.url.as_deref(), Some("https://example.com/page/"));
    }

    #[test]
    fn uids_are_stable_identities() {
        // 同一页面两次提取:uid 逐节点一致
        let a = snap("<!doctype html><main><h1>a</h1><p>b</p></main>");
        let b = snap("<!doctype html><main><h1>a</h1><p>b</p></main>");
        assert_eq!(a.root.uid, "root");
        let (ma, mb) = (&a.root.children[0], &b.root.children[0]);
        assert_eq!(ma.uid, mb.uid);
        assert_eq!(ma.children[0].uid, mb.children[0].uid);
        assert!(!ma.uid.is_empty() && ma.uid != ma.children[0].uid);
        // 无关兄弟的插入不移动既有节点的 uid
        let c = snap("<!doctype html><main><figure></figure><h1>a</h1><p>b</p></main>");
        let mc = &c.root.children[0];
        let h1_c = mc.children.iter().find(|n| n.role == Role::Heading).unwrap();
        assert_eq!(h1_c.uid, ma.children[0].uid);
    }

    #[test]
    fn spa_shell_yields_empty_tree() {
        // 项目起源:SPA 空壳在 M0 就该诚实地显示“没有结构”
        let out = tree(concat!(
            "<!doctype html><html><head><title>app</title>",
            "<script src=/bundle.js></script></head>",
            "<body><div id=root></div></body></html>",
        ));
        assert_eq!(out, "document \"app\"\n");
    }
}
