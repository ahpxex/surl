//! 语义树 IR:对齐 a11y snapshot 风格(role/name/state/href + uid)。
//!
//! uid 目前是先序遍历序号占位;跨次渲染的稳定身份是 diff(M5)要解的树匹配
//! 问题,字段先留在 IR 里。

use std::fmt::Write;

use serde::Serialize;

/// 节点角色,取 ARIA role 的一个实用子集。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Document,
    // landmark
    Banner,
    Navigation,
    Main,
    Complementary,
    ContentInfo,
    Search,
    Form,
    Region,
    // 结构
    Article,
    Heading,
    Paragraph,
    Blockquote,
    List,
    ListItem,
    Table,
    Row,
    Cell,
    ColumnHeader,
    RowHeader,
    Figure,
    Separator,
    Img,
    Iframe,
    // 交互
    Link,
    Button,
    TextBox,
    SearchBox,
    CheckBox,
    Radio,
    Slider,
    ComboBox,
    ListBox,
    #[serde(rename = "option")]
    OptionItem,
    ProgressBar,
    Dialog,
    // 叶子文本
    Text,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Document => "document",
            Role::Banner => "banner",
            Role::Navigation => "navigation",
            Role::Main => "main",
            Role::Complementary => "complementary",
            Role::ContentInfo => "contentinfo",
            Role::Search => "search",
            Role::Form => "form",
            Role::Region => "region",
            Role::Article => "article",
            Role::Heading => "heading",
            Role::Paragraph => "paragraph",
            Role::Blockquote => "blockquote",
            Role::List => "list",
            Role::ListItem => "listitem",
            Role::Table => "table",
            Role::Row => "row",
            Role::Cell => "cell",
            Role::ColumnHeader => "columnheader",
            Role::RowHeader => "rowheader",
            Role::Figure => "figure",
            Role::Separator => "separator",
            Role::Img => "img",
            Role::Iframe => "iframe",
            Role::Link => "link",
            Role::Button => "button",
            Role::TextBox => "textbox",
            Role::SearchBox => "searchbox",
            Role::CheckBox => "checkbox",
            Role::Radio => "radio",
            Role::Slider => "slider",
            Role::ComboBox => "combobox",
            Role::ListBox => "listbox",
            Role::OptionItem => "option",
            Role::ProgressBar => "progressbar",
            Role::Dialog => "dialog",
            Role::Text => "text",
        }
    }

    /// 该角色的可及名默认取自内容(链接文字、按钮文字…),
    /// 因而其下的裸文本不再单独成节点。
    pub fn names_from_contents(self) -> bool {
        matches!(
            self,
            Role::Link
                | Role::Button
                | Role::Heading
                | Role::Cell
                | Role::ColumnHeader
                | Role::RowHeader
                | Role::OptionItem
        )
    }
}

/// 节点状态。全部可缺省,JSON 里只序列化非默认值。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct State {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
}

impl State {
    pub fn is_default(&self) -> bool {
        *self == State::default()
    }
}

#[derive(Debug, Serialize)]
pub struct SemanticNode {
    pub uid: u32,
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// heading 层级
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    #[serde(flatten)]
    pub state: State,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SemanticNode>,
}

impl SemanticNode {
    pub fn new(uid: u32, role: Role) -> Self {
        SemanticNode {
            uid,
            role,
            name: None,
            href: None,
            value: None,
            level: None,
            state: State::default(),
            children: Vec::new(),
        }
    }
}

/// 一次抓取+提取的完整产物。
#[derive(Debug, Serialize)]
pub struct Snapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub root: SemanticNode,
}

impl Snapshot {
    /// `--tree` 输出:两空格缩进的语义大纲。
    pub fn to_tree_string(&self) -> String {
        let mut out = String::new();
        render(&self.root, 0, &mut out);
        out
    }
}

fn render(n: &SemanticNode, depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
    out.push_str(n.role.as_str());
    if let Some(level) = n.level {
        let _ = write!(out, "[{level}]");
    }
    if let Some(name) = &n.name {
        let _ = write!(out, " {name:?}");
    }
    if let Some(value) = &n.value {
        let _ = write!(out, " = {value:?}");
    }
    if n.state.disabled {
        out.push_str(" (disabled)");
    }
    if let Some(checked) = n.state.checked {
        out.push_str(if checked { " (checked)" } else { " (unchecked)" });
    }
    if let Some(selected) = n.state.selected
        && selected
    {
        out.push_str(" (selected)");
    }
    if let Some(expanded) = n.state.expanded {
        out.push_str(if expanded { " (expanded)" } else { " (collapsed)" });
    }
    if let Some(href) = &n.href {
        let _ = write!(out, " -> {href}");
    }
    out.push('\n');
    for child in &n.children {
        render(child, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_string_format() {
        let mut root = SemanticNode::new(0, Role::Document);
        root.name = Some("T".into());
        let mut h = SemanticNode::new(1, Role::Heading);
        h.level = Some(1);
        h.name = Some("Hello".into());
        let mut a = SemanticNode::new(2, Role::Link);
        a.name = Some("go".into());
        a.href = Some("https://x.dev/".into());
        let mut cb = SemanticNode::new(3, Role::CheckBox);
        cb.name = Some("agree".into());
        cb.state.checked = Some(true);
        cb.state.disabled = true;
        root.children = vec![h, a, cb];
        let snap = Snapshot {
            url: None,
            title: Some("T".into()),
            root,
        };
        assert_eq!(
            snap.to_tree_string(),
            "document \"T\"\n  heading[1] \"Hello\"\n  link \"go\" -> https://x.dev/\n  checkbox \"agree\" (disabled) (checked)\n"
        );
    }

    #[test]
    fn json_shape_skips_defaults() {
        let node = SemanticNode::new(5, Role::Paragraph);
        let v = serde_json::to_value(&node).unwrap();
        assert_eq!(v, serde_json::json!({"uid": 5, "role": "paragraph"}));
    }

    #[test]
    fn json_option_role_renamed() {
        let mut node = SemanticNode::new(1, Role::OptionItem);
        node.state.selected = Some(true);
        let v = serde_json::to_value(&node).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"uid": 1, "role": "option", "selected": true})
        );
    }
}
