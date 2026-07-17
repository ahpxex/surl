//! `--md`:readability 式正文提取,输出 Markdown。
//!
//! 不做完整 readability 评分:结构化启发——优先 `<main>`/`[role=main]`/
//! `<article>` 作为正文根,剥掉导航/页眉/页脚/侧栏与隐藏内容,其余按
//! 块级/内联规则转 Markdown。agent 读链接场景的主力输出模式。

use surl_dom::{Document, NodeData, NodeId};
use url::Url;

pub fn extract(doc: &Document, base: Option<&Url>) -> String {
    let root = content_root(doc);
    let mut out = String::new();

    // 标题:正文根里若没有 h1,用 <title> 起头
    if let Some(title) = page_title(doc)
        && !subtree_has_h1(doc, root) && !title.trim().is_empty() {
            out.push_str(&format!("# {}\n\n", collapse_ws(&title)));
        }

    let mut w = Writer {
        doc,
        base,
        out,
        list_stack: Vec::new(),
    };
    w.walk_blocks(root);
    let mut text = w.out;
    // 压掉三连以上空行
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    let trimmed = text.trim_matches('\n');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

/// 正文根:main > [role=main] > article > body > 整树。
fn content_root(doc: &Document) -> NodeId {
    let root = doc.root();
    let find = |pred: &dyn Fn(&surl_dom::ElementData) -> bool| {
        doc.descendants(root)
            .find(|&n| doc.element(n).is_some_and(pred))
    };
    find(&|el| el.is_html_element("main"))
        .or_else(|| find(&|el| el.attr("role") == Some("main")))
        .or_else(|| find(&|el| el.is_html_element("article")))
        .or_else(|| find(&|el| el.is_html_element("body")))
        .unwrap_or(root)
}

fn page_title(doc: &Document) -> Option<String> {
    let title = doc
        .descendants(doc.root())
        .find(|&n| doc.element(n).is_some_and(|el| el.is_html_element("title")))?;
    Some(doc.text_content(title))
}

fn subtree_has_h1(doc: &Document, root: NodeId) -> bool {
    doc.descendants(root)
        .any(|n| doc.element(n).is_some_and(|el| el.is_html_element("h1")))
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Writer<'a> {
    doc: &'a Document,
    base: Option<&'a Url>,
    out: String,
    /// 嵌套列表:(有序?, 当前序号)
    list_stack: Vec<(bool, usize)>,
}

impl Writer<'_> {
    fn skip(&self, el: &surl_dom::ElementData) -> bool {
        matches!(
            el.local_name().as_ref(),
            "script" | "style" | "template" | "noscript" | "nav" | "header" | "footer" | "aside"
        ) || el.attr("hidden").is_some()
            || el.attr("aria-hidden") == Some("true")
    }

    fn resolve(&self, href: &str) -> String {
        match self.base {
            Some(base) => base
                .join(href)
                .map(|u| u.to_string())
                .unwrap_or_else(|_| href.to_owned()),
            None => href.to_owned(),
        }
    }

    fn walk_blocks(&mut self, id: NodeId) {
        self.flow_children(id);
    }

    /// 匿名盒:把连续的内联子节点(文本/a/strong…)聚成一个段落,
    /// 遇到块级子节点先冲刷段落再递归。杜绝「父层收一遍、子层再收一遍」。
    fn flow_children(&mut self, id: NodeId) {
        let mut para = String::new();
        let children = self.doc.node(id).children.clone();
        for child in children {
            if self.is_inline_node(child) {
                self.inline_node_into(child, &mut para);
            } else {
                self.flush_para(&mut para);
                self.block(child);
            }
        }
        self.flush_para(&mut para);
    }

    fn flush_para(&mut self, para: &mut String) {
        let text = collapse_ws(para);
        if !text.is_empty() {
            self.out.push_str(&format!("{text}\n\n"));
        }
        para.clear();
    }

    fn is_inline_node(&self, id: NodeId) -> bool {
        match &self.doc.node(id).data {
            NodeData::Text { .. } => true,
            NodeData::Element(el) => matches!(
                el.local_name().as_ref(),
                "a" | "span" | "strong" | "b" | "em" | "i" | "code" | "small" | "time"
                    | "label" | "sub" | "sup" | "u" | "s" | "abbr" | "mark" | "img" | "br"
                    | "input" | "button" | "svg" | "picture" | "source"
            ),
            _ => false,
        }
    }

    /// 单个内联节点写进段落缓冲(复用 inline_into 的元素规则)
    fn inline_node_into(&self, id: NodeId, out: &mut String) {
        match &self.doc.node(id).data {
            NodeData::Text { contents } => out.push_str(contents),
            NodeData::Element(_) => {
                // 借 inline_into 的分派:包一层假父不划算,直接按单元素处理
                let tmp_parent_children = [id];
                let _ = tmp_parent_children;
                self.inline_element_into(id, out);
            }
            _ => {}
        }
    }

    fn block(&mut self, id: NodeId) {
        match &self.doc.node(id).data {
            NodeData::Text { contents } => {
                // 块级语境里的裸文本:归入段落流
                let t = collapse_ws(contents);
                if !t.is_empty() {
                    self.out.push_str(&t);
                    self.out.push('\n');
                }
            }
            NodeData::Element(el) => {
                if self.skip(el) {
                    return;
                }
                let name = el.local_name().as_ref().to_owned();
                match name.as_str() {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        let level = name[1..].parse::<usize>().unwrap_or(1);
                        let text = collapse_ws(&self.inline_text(id));
                        if !text.is_empty() {
                            self.out
                                .push_str(&format!("\n{} {}\n\n", "#".repeat(level), text));
                        }
                    }
                    "p" => {
                        let text = collapse_ws(&self.inline_text(id));
                        if !text.is_empty() {
                            self.out.push_str(&format!("{text}\n\n"));
                        }
                    }
                    "br" => self.out.push('\n'),
                    "hr" => self.out.push_str("\n---\n\n"),
                    "pre" => {
                        let code = self.doc.text_content(id);
                        let code = code.trim_matches('\n');
                        self.out.push_str(&format!("\n```\n{code}\n```\n\n"));
                    }
                    "blockquote" => {
                        let mut inner = Writer {
                            doc: self.doc,
                            base: self.base,
                            out: String::new(),
                            list_stack: Vec::new(),
                        };
                        inner.walk_blocks(id);
                        for line in inner.out.trim_matches('\n').lines() {
                            self.out.push_str(&format!("> {line}\n"));
                        }
                        self.out.push('\n');
                    }
                    "ul" | "ol" => {
                        self.list_stack.push((name == "ol", 0));
                        self.walk_blocks(id);
                        self.list_stack.pop();
                        if self.list_stack.is_empty() {
                            self.out.push('\n');
                        }
                    }
                    "li" => {
                        let depth = self.list_stack.len().saturating_sub(1);
                        let marker = match self.list_stack.last_mut() {
                            Some((true, n)) => {
                                *n += 1;
                                format!("{}. ", n)
                            }
                            _ => "- ".to_owned(),
                        };
                        let text = collapse_ws(&self.inline_text_shallow(id));
                        self.out
                            .push_str(&format!("{}{marker}{text}\n", "  ".repeat(depth)));
                        // 嵌套列表继续走块级
                        for &child in &self.doc.node(id).children {
                            if self.doc.element(child).is_some_and(|el| {
                                matches!(el.local_name().as_ref(), "ul" | "ol")
                            }) {
                                self.block(child);
                            }
                        }
                    }
                    "img" => {
                        let alt = el.attr("alt").unwrap_or("");
                        if let Some(src) = el.attr("src") {
                            let src = self.resolve(src);
                            self.out.push_str(&format!("![{alt}]({src})\n\n"));
                        }
                    }
                    "table" => self.table(id),
                    // 已知内联元素落到块级语境:说明父层是 flow,由 flow 聚段,
                    // 这里不该出现;防御性单独成段
                    "a" | "span" | "strong" | "b" | "em" | "i" | "code" | "small" | "time"
                    | "label" | "sub" | "sup" | "u" | "s" | "abbr" | "mark" => {
                        let mut para = String::new();
                        self.inline_into(id, &mut para, false);
                        let text = collapse_ws(&para);
                        if !text.is_empty() {
                            self.out.push_str(&format!("{text}\n\n"));
                        }
                    }
                    // 纯容器与未知块:匿名盒模型——连续内联聚段,块级递归
                    _ => self.flow_children(id),
                }
            }
            _ => {}
        }
    }

    /// 深度收集内联 Markdown(块级子元素也拍平进来——标题/段落语境用)
    fn inline_text(&self, id: NodeId) -> String {
        let mut s = String::new();
        self.inline_into(id, &mut s, false);
        s
    }

    /// 只收直接内联孩子,跳过块级孩子(li/div 语境:块级由 block 处理)
    fn inline_text_shallow(&self, id: NodeId) -> String {
        let mut s = String::new();
        self.inline_into(id, &mut s, true);
        s
    }

    fn inline_into(&self, id: NodeId, out: &mut String, shallow: bool) {
        for &child in &self.doc.node(id).children {
            match &self.doc.node(child).data {
                NodeData::Text { contents } => out.push_str(contents),
                NodeData::Element(_) => self.inline_child_into(child, out, shallow),
                _ => {}
            }
        }
    }

    fn inline_element_into(&self, id: NodeId, out: &mut String) {
        self.inline_child_into(id, out, false);
    }

    fn inline_child_into(&self, child: NodeId, out: &mut String, shallow: bool) {
        {
            {
                if let NodeData::Element(el) = &self.doc.node(child).data {
                if self.skip(el) {
                    return;
                }
                match el.local_name().as_ref() {
                    "a" => {
                        let inner = self.inline_text(child);
                        let text = collapse_ws(&inner);
                        match el.attr("href") {
                            Some(href) if !text.is_empty() => {
                                let href = self.resolve(href);
                                out.push_str(&format!("[{text}]({href})"));
                            }
                            _ => out.push_str(&text),
                        }
                    }
                    "strong" | "b" => {
                        let t = collapse_ws(&self.inline_text(child));
                        if !t.is_empty() {
                            out.push_str(&format!("**{t}**"));
                        }
                    }
                    "em" | "i" => {
                        let t = collapse_ws(&self.inline_text(child));
                        if !t.is_empty() {
                            out.push_str(&format!("*{t}*"));
                        }
                    }
                    "code" => {
                        let t = self.doc.text_content(child);
                        if !t.is_empty() {
                            out.push_str(&format!("`{t}`"));
                        }
                    }
                    "br" => out.push('\n'),
                    "img" => {
                        if let Some(src) = el.attr("src") {
                            let alt = el.attr("alt").unwrap_or("");
                            out.push_str(&format!("![{alt}]({})", self.resolve(src)));
                        }
                    }
                    "ul" | "ol" | "li" | "p" | "div" | "section" | "table" | "pre"
                    | "blockquote"
                        if shallow => {}
                    _ => {
                        out.push(' ');
                        self.inline_into(child, out, shallow);
                        out.push(' ');
                    }
                }
                }
            }
        }
    }

    fn table(&mut self, id: NodeId) {
        let mut rows: Vec<Vec<String>> = Vec::new();
        for tr in self
            .doc
            .descendants(id)
            .filter(|&n| self.doc.element(n).is_some_and(|el| el.is_html_element("tr")))
        {
            let cells: Vec<String> = self
                .doc
                .node(tr)
                .children
                .iter()
                .filter(|&&c| {
                    self.doc
                        .element(c)
                        .is_some_and(|el| matches!(el.local_name().as_ref(), "td" | "th"))
                })
                .map(|&c| collapse_ws(&self.inline_text(c)).replace('|', "\\|"))
                .collect();
            if !cells.is_empty() {
                rows.push(cells);
            }
        }
        if rows.is_empty() {
            return;
        }
        self.out.push('\n');
        for (i, row) in rows.iter().enumerate() {
            self.out.push_str(&format!("| {} |\n", row.join(" | ")));
            if i == 0 {
                self.out
                    .push_str(&format!("|{}\n", " --- |".repeat(row.len())));
            }
        }
        self.out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surl_dom::parse_html;

    #[test]
    fn extracts_main_content_as_markdown() {
        let doc = parse_html(
            r#"<!doctype html><title>T</title>
            <nav><a href="/x">nav link</a></nav>
            <main>
              <h1>Hello</h1>
              <p>A <strong>bold</strong> <a href="/rel">link</a>.</p>
              <ul><li>one</li><li>two<ul><li>deep</li></ul></li></ul>
              <pre>let x = 1;</pre>
            </main>
            <footer>bye</footer>"#,
        );
        let base = Url::parse("https://ex.test/").unwrap();
        let md = extract(&doc, Some(&base));
        assert!(md.contains("# Hello"), "{md}");
        assert!(md.contains("A **bold** [link](https://ex.test/rel)."), "{md}");
        assert!(md.contains("- one"), "{md}");
        assert!(md.contains("  - deep"), "{md}");
        assert!(md.contains("```\nlet x = 1;\n```"), "{md}");
        assert!(!md.contains("nav link"), "{md}");
        assert!(!md.contains("bye"), "{md}");
    }

    #[test]
    fn falls_back_to_title_and_body() {
        let doc = parse_html("<!doctype html><title>Page T</title><p>body text</p>");
        let md = extract(&doc, None);
        assert!(md.starts_with("# Page T"), "{md}");
        assert!(md.contains("body text"), "{md}");
    }
}
