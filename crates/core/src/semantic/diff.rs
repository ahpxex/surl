//! 结构 diff:两份语义树按稳定 uid 对齐,输出 增/删/改。
//!
//! uid 承载身份(role/level/href + 祖先链 + 同键序号),因此:
//! - name/value/state 的变化 → `~ 改`(同一节点,内容不同);
//! - uid 只在一侧出现 → `+ 增` / `- 删`;
//! - 同键兄弟重排会表现为删+增(v1 不做移动检测)。

use std::collections::BTreeMap;

use super::ir::{SemanticNode, Snapshot};

#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Added { path: String, line: String },
    Removed { path: String, line: String },
    Modified { path: String, before: String, after: String },
}

/// 先序收集 uid → (路径, 节点)。路径 = 祖先角色链,人读定位用。
fn index<'a>(node: &'a SemanticNode, path: &str, out: &mut BTreeMap<&'a str, (String, &'a SemanticNode)>) {
    let here = if path.is_empty() {
        node.role.as_str().to_owned()
    } else {
        format!("{path} > {}", node.role.as_str())
    };
    out.insert(node.uid.as_str(), (here.clone(), node));
    for child in &node.children {
        index(child, &here, out);
    }
}

fn renders_differently(a: &SemanticNode, b: &SemanticNode) -> bool {
    a.name != b.name || a.value != b.value || a.state != b.state || a.href != b.href
}

pub fn diff(a: &Snapshot, b: &Snapshot) -> Vec<Change> {
    let mut ia = BTreeMap::new();
    let mut ib = BTreeMap::new();
    index(&a.root, "", &mut ia);
    index(&b.root, "", &mut ib);

    let mut out = Vec::new();
    for (uid, (path, na)) in &ia {
        match ib.get(uid) {
            None => out.push(Change::Removed {
                path: path.clone(),
                line: na.line(),
            }),
            Some((_, nb)) if renders_differently(na, nb) => out.push(Change::Modified {
                path: path.clone(),
                before: na.line(),
                after: nb.line(),
            }),
            Some(_) => {}
        }
    }
    for (uid, (path, nb)) in &ib {
        if !ia.contains_key(uid) {
            out.push(Change::Added {
                path: path.clone(),
                line: nb.line(),
            });
        }
    }
    out
}

/// 人读输出:一行一个变化,前缀 +/-/~。
pub fn to_text(changes: &[Change]) -> String {
    let mut out = String::new();
    for c in changes {
        match c {
            Change::Added { path, line } => out.push_str(&format!("+ {line}\n    @ {path}\n")),
            Change::Removed { path, line } => out.push_str(&format!("- {line}\n    @ {path}\n")),
            Change::Modified { path, before, after } => {
                out.push_str(&format!("~ {before}\n  → {after}\n    @ {path}\n"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::extract;
    use surl_dom::parse_html;

    fn snap(html: &str) -> Snapshot {
        extract(&parse_html(html), None)
    }

    #[test]
    fn identical_pages_have_no_diff_and_stable_uids() {
        let a = snap("<main><h1>T</h1><a href='/x'>x</a></main>");
        let b = snap("<main><h1>T</h1><a href='/x'>x</a></main>");
        assert_eq!(a.root.children[0].children[0].uid, b.root.children[0].children[0].uid);
        assert!(diff(&a, &b).is_empty());
    }

    #[test]
    fn name_change_is_modification_not_replacement() {
        let a = snap("<main><button>Count is 0</button></main>");
        let b = snap("<main><button>Count is 1</button></main>");
        let d = diff(&a, &b);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(matches!(&d[0], Change::Modified { .. }), "{d:?}");
    }

    #[test]
    fn unrelated_sibling_insert_keeps_uids_stable() {
        let a = snap("<main><h1>T</h1><a href='/x'>x</a></main>");
        let b = snap("<main><h1>T</h1><p>new</p><a href='/x'>x</a></main>");
        // 既有节点 uid 不漂移:diff 全是「增」(p 及其文本子节点),无删无改
        let d = diff(&a, &b);
        assert_eq!(d.len(), 2, "{d:?}");
        assert!(d.iter().all(|c| matches!(c, Change::Added { .. })), "{d:?}");
    }

    #[test]
    fn removal_is_reported() {
        let a = snap("<main><a href='/x'>x</a><a href='/y'>y</a></main>");
        let b = snap("<main><a href='/x'>x</a></main>");
        let d = diff(&a, &b);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(matches!(&d[0], Change::Removed { line, .. } if line.contains("/y")), "{d:?}");
    }
}
