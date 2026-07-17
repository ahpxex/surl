//! 语义树 IR:对齐 a11y snapshot 风格(role/name/state/href + uid)。
//!
//! uid 是稳定节点身份:role/level/href 的结构哈希沿祖先链传播,同键兄弟
//! 用出现序号消歧。设计目标——跨次渲染不漂移(diff 的前提):
//! - 无关子树的改动不影响本节点 uid(只依赖自身键 + 祖先链);
//! - 插入不同键的兄弟不移动既有 uid(序号只在同键节点间计数);
//! - name/value 变化不改变 uid(它们是 diff 的「修改」信号,不是身份)。

use std::fmt::Write;

use serde::{Deserialize, Serialize};

/// 节点角色,取 ARIA role 的一个实用子集。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
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

#[derive(Debug, Serialize, Deserialize)]
pub struct SemanticNode {
    #[serde(default)]
    pub uid: String,
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
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<SemanticNode>,
}

impl SemanticNode {
    pub fn new(role: Role) -> Self {
        SemanticNode {
            uid: String::new(),
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
#[derive(Debug, Serialize, Deserialize)]
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

impl SemanticNode {
    /// 一行摘要(--tree 的单行形态,diff 输出复用)。
    pub fn line(&self) -> String {
        let mut out = String::new();
        out.push_str(self.role.as_str());
        if let Some(level) = self.level {
            let _ = write!(out, "[{level}]");
        }
        if let Some(name) = &self.name {
            let _ = write!(out, " {name:?}");
        }
        if let Some(value) = &self.value {
            let _ = write!(out, " value={value:?}");
        }
        if let Some(href) = &self.href {
            let _ = write!(out, " -> {href}");
        }
        out
    }
}

/// FNV-1a:标准库 Hasher 不承诺跨版本稳定,uid 必须稳定,手写。
fn fnv1a(data: &[u8], seed: u64) -> u64 {
    let mut h = seed ^ 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 后序赋 uid:node 的键 = role|level|href,uid = hash(祖先链, 键, 同键序号)。
pub fn assign_stable_uids(node: &mut SemanticNode, parent_hash: u64) {
    let mut seen: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
    for child in &mut node.children {
        let mut key = String::new();
        key.push_str(child.role.as_str());
        if let Some(level) = child.level {
            let _ = write!(key, "|{level}");
        }
        if let Some(href) = &child.href {
            let _ = write!(key, "|{href}");
        }
        let key_hash = fnv1a(key.as_bytes(), 0);
        let occ = seen.entry(key_hash).or_insert(0);
        let h = fnv1a(&occ.to_le_bytes(), parent_hash ^ key_hash);
        *occ += 1;
        child.uid = format!("{:08x}", (h >> 32) as u32 ^ h as u32);
        assign_stable_uids(child, h);
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
        let mut root = SemanticNode::new(Role::Document);
        root.name = Some("T".into());
        let mut h = SemanticNode::new(Role::Heading);
        h.level = Some(1);
        h.name = Some("Hello".into());
        let mut a = SemanticNode::new(Role::Link);
        a.name = Some("go".into());
        a.href = Some("https://x.dev/".into());
        let mut cb = SemanticNode::new(Role::CheckBox);
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
        let node = SemanticNode::new(Role::Paragraph);
        let v = serde_json::to_value(&node).unwrap();
        assert_eq!(v, serde_json::json!({"uid": "", "role": "paragraph"}));
    }

    #[test]
    fn json_option_role_renamed() {
        let mut node = SemanticNode::new(Role::OptionItem);
        node.state.selected = Some(true);
        let v = serde_json::to_value(&node).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"uid": "", "role": "option", "selected": true})
        );
    }
}
