//! CSS 选择器匹配:Servo `selectors` crate 接到我们的 arena DOM 上。
//!
//! 结构:`SurlSelectors` 定义选择器里的字符串类型(全部 String 包装),
//! `ElementRef`(Document + NodeId)实现 `selectors::Element` 供匹配引擎
//! 遍历,`Document::{query_selector, query_selector_all}` 是对外 API。

use std::borrow::Borrow;
use std::fmt;

use cssparser::{CowRcStr, ParseError, Parser as CssParser, ParserInput, SourceLocation, ToCss};
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::BloomFilter;
use selectors::context::{
    MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, SelectorCaches,
};
use selectors::matching::{ElementSelectorFlags, matches_selector_list};
use selectors::parser::{
    NonTSPseudoClass, ParseRelative, Parser, PseudoElement, SelectorImpl, SelectorList,
    SelectorParseErrorKind,
};
use selectors::{Element, OpaqueElement};

use crate::tree::{Document, ElementData, NodeData, NodeId};

// ---- 选择器里的字符串类型 ----

/// String 包装,补上 selectors 要求的 ToCss / PrecomputedHash。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CssString(pub String);

impl<'a> From<&'a str> for CssString {
    fn from(s: &'a str) -> Self {
        CssString(s.to_owned())
    }
}

impl Borrow<str> for CssString {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for CssString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl ToCss for CssString {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        cssparser::serialize_string(&self.0, dest)
    }
}

impl precomputed_hash::PrecomputedHash for CssString {
    fn precomputed_hash(&self) -> u32 {
        // 只用于 bloom filter 优化;我们不启用 bloom,给个稳定值即可
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        self.0.hash(&mut h);
        h.finish() as u32
    }
}

/// 我们支持的非树结构伪类(与 JS 无关的先给一个实用子集)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurlPseudoClass {
    AnyLink,
    Link,
    Disabled,
    Enabled,
    Checked,
    /// 合法但未实现的伪类(:modal、:focus-visible 等):不匹配任何元素。
    /// 抛解析错误会炸掉整条选择器,github 的 details-dialog 实测踩雷;
    /// 浏览器语义是「已知伪类不命中」,对无像素的我们,不命中即正确近似。
    Unsupported(CssString),
}

impl NonTSPseudoClass for SurlPseudoClass {
    type Impl = SurlSelectors;
    fn is_active_or_hover(&self) -> bool {
        false
    }
    fn is_user_action_state(&self) -> bool {
        false
    }
}

impl ToCss for SurlPseudoClass {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        match self {
            SurlPseudoClass::AnyLink => dest.write_str(":any-link"),
            SurlPseudoClass::Link => dest.write_str(":link"),
            SurlPseudoClass::Disabled => dest.write_str(":disabled"),
            SurlPseudoClass::Enabled => dest.write_str(":enabled"),
            SurlPseudoClass::Checked => dest.write_str(":checked"),
            SurlPseudoClass::Unsupported(name) => write!(dest, ":{}", name.0),
        }
    }
}

/// 不渲染,伪元素永远不匹配;类型仍需存在。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurlPseudoElement {}

impl PseudoElement for SurlPseudoElement {
    type Impl = SurlSelectors;
}

impl ToCss for SurlPseudoElement {
    fn to_css<W: fmt::Write>(&self, _dest: &mut W) -> fmt::Result {
        match *self {}
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurlSelectors;

impl SelectorImpl for SurlSelectors {
    type ExtraMatchingData<'a> = ();
    type AttrValue = CssString;
    type Identifier = CssString;
    type LocalName = CssString;
    type NamespaceUrl = CssString;
    type NamespacePrefix = CssString;
    type BorrowedNamespaceUrl = str;
    type BorrowedLocalName = str;
    type NonTSPseudoClass = SurlPseudoClass;
    type PseudoElement = SurlPseudoElement;
}

struct SurlParser;

impl<'i> Parser<'i> for SurlParser {
    type Impl = SurlSelectors;
    type Error = SelectorParseErrorKind<'i>;

    fn parse_is_and_where(&self) -> bool {
        true
    }

    fn parse_non_ts_pseudo_class(
        &self,
        location: SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<SurlPseudoClass, ParseError<'i, Self::Error>> {
        Ok(match name.as_ref() {
            "any-link" => SurlPseudoClass::AnyLink,
            "link" => SurlPseudoClass::Link,
            "disabled" => SurlPseudoClass::Disabled,
            "enabled" => SurlPseudoClass::Enabled,
            "checked" => SurlPseudoClass::Checked,
            _ => {
                let _ = location;
                SurlPseudoClass::Unsupported(CssString(name.as_ref().to_owned()))
            }
        })
    }
}

// ---- Element 实现:匹配引擎看到的 DOM 视图 ----

#[derive(Clone, Copy)]
struct ElementRef<'a> {
    doc: &'a Document,
    id: NodeId,
}

impl fmt::Debug for ElementRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ElementRef({:?})", self.id)
    }
}

impl<'a> ElementRef<'a> {
    fn data(&self) -> &'a ElementData {
        self.doc.element(self.id).expect("ElementRef on non-element")
    }
}

impl Element for ElementRef<'_> {
    type Impl = SurlSelectors;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.doc.node(self.id))
    }

    fn parent_element(&self) -> Option<Self> {
        let parent = self.doc.node(self.id).parent?;
        matches!(self.doc.node(parent).data, NodeData::Element(_))
            .then_some(ElementRef { doc: self.doc, id: parent })
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        let parent = self.doc.node(self.id).parent?;
        let children = &self.doc.node(parent).children;
        let idx = children.iter().position(|&c| c == self.id)?;
        children[..idx]
            .iter()
            .rev()
            .copied()
            .find(|&c| matches!(self.doc.node(c).data, NodeData::Element(_)))
            .map(|id| ElementRef { doc: self.doc, id })
    }

    fn next_sibling_element(&self) -> Option<Self> {
        let parent = self.doc.node(self.id).parent?;
        let children = &self.doc.node(parent).children;
        let idx = children.iter().position(|&c| c == self.id)?;
        children[idx + 1..]
            .iter()
            .copied()
            .find(|&c| matches!(self.doc.node(c).data, NodeData::Element(_)))
            .map(|id| ElementRef { doc: self.doc, id })
    }

    fn first_element_child(&self) -> Option<Self> {
        self.doc
            .node(self.id)
            .children
            .iter()
            .copied()
            .find(|&c| matches!(self.doc.node(c).data, NodeData::Element(_)))
            .map(|id| ElementRef { doc: self.doc, id })
    }

    fn is_html_element_in_html_document(&self) -> bool {
        self.data().name.ns == html5ever::ns!(html)
    }

    fn has_local_name(&self, name: &str) -> bool {
        *self.data().name.local == *name
    }

    fn has_namespace(&self, ns: &str) -> bool {
        self.data().name.ns.as_ref() == ns
    }

    fn is_same_type(&self, other: &Self) -> bool {
        self.data().name == other.data().name
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&CssString>,
        local_name: &CssString,
        operation: &AttrSelectorOperation<&CssString>,
    ) -> bool {
        self.data().attrs.iter().any(|attr| {
            if *attr.name.local != *local_name.0 {
                return false;
            }
            match ns {
                NamespaceConstraint::Any => {}
                NamespaceConstraint::Specific(url) => {
                    if attr.name.ns.as_ref() != url.0 {
                        return false;
                    }
                }
            }
            operation.eval_str(&attr.value)
        })
    }

    fn match_non_ts_pseudo_class(
        &self,
        pc: &SurlPseudoClass,
        _context: &mut MatchingContext<SurlSelectors>,
    ) -> bool {
        let el = self.data();
        match pc {
            SurlPseudoClass::AnyLink | SurlPseudoClass::Link => self.is_link(),
            SurlPseudoClass::Disabled => el.attr("disabled").is_some(),
            SurlPseudoClass::Enabled => {
                el.attr("disabled").is_none()
                    && matches!(
                        el.local_name().as_ref(),
                        "input" | "button" | "select" | "textarea" | "option" | "fieldset"
                    )
            }
            SurlPseudoClass::Checked => el.attr("checked").is_some() || el.attr("selected").is_some(),
            SurlPseudoClass::Unsupported(_) => false,
        }
    }

    fn match_pseudo_element(
        &self,
        pe: &SurlPseudoElement,
        _context: &mut MatchingContext<SurlSelectors>,
    ) -> bool {
        match *pe {}
    }

    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {}

    fn is_link(&self) -> bool {
        let el = self.data();
        matches!(el.local_name().as_ref(), "a" | "area") && el.attr("href").is_some()
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(&self, id: &CssString, case_sensitivity: CaseSensitivity) -> bool {
        self.data()
            .attr("id")
            .is_some_and(|v| case_sensitivity.eq(v.as_bytes(), id.0.as_bytes()))
    }

    fn has_class(&self, name: &CssString, case_sensitivity: CaseSensitivity) -> bool {
        self.data().attr("class").is_some_and(|classes| {
            classes
                .split_ascii_whitespace()
                .any(|c| case_sensitivity.eq(c.as_bytes(), name.0.as_bytes()))
        })
    }

    fn has_custom_state(&self, _name: &CssString) -> bool {
        false
    }

    fn imported_part(&self, _name: &CssString) -> Option<CssString> {
        None
    }

    fn is_part(&self, _name: &CssString) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        self.doc.node(self.id).children.iter().all(|&c| {
            match &self.doc.node(c).data {
                NodeData::Element(_) => false,
                NodeData::Text { contents } => contents.is_empty(),
                _ => true,
            }
        })
    }

    fn is_root(&self) -> bool {
        self.doc
            .node(self.id)
            .parent
            .is_some_and(|p| matches!(self.doc.node(p).data, NodeData::Document))
    }

    fn add_element_unique_hashes(&self, _filter: &mut BloomFilter) -> bool {
        false
    }
}

// ---- 对外 API ----

#[derive(Debug, thiserror::Error)]
#[error("invalid selector `{0}`")]
pub struct SelectorError(String);

fn parse_selector_list(input: &str) -> Result<SelectorList<SurlSelectors>, SelectorError> {
    let mut parser_input = ParserInput::new(input);
    let mut parser = CssParser::new(&mut parser_input);
    SelectorList::parse(&SurlParser, &mut parser, ParseRelative::No)
        .map_err(|_| SelectorError(input.to_owned()))
}

impl Document {
    /// scope 的后代里第一个匹配的元素(querySelector 语义,不含 scope 自身)。
    pub fn query_selector(
        &self,
        scope: NodeId,
        selectors: &str,
    ) -> Result<Option<NodeId>, SelectorError> {
        let list = parse_selector_list(selectors)?;
        Ok(self.matching_descendants(scope, &list).next())
    }

    /// scope 的后代里所有匹配的元素,文档序。
    pub fn query_selector_all(
        &self,
        scope: NodeId,
        selectors: &str,
    ) -> Result<Vec<NodeId>, SelectorError> {
        let list = parse_selector_list(selectors)?;
        Ok(self.matching_descendants(scope, &list).collect())
    }

    /// 元素自身是否匹配(Element.matches 语义)。
    pub fn element_matches(&self, id: NodeId, selectors: &str) -> Result<bool, SelectorError> {
        let list = parse_selector_list(selectors)?;
        Ok(self.matches_list(id, &list))
    }

    fn matching_descendants<'a>(
        &'a self,
        scope: NodeId,
        list: &'a SelectorList<SurlSelectors>,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.descendants(scope)
            .filter(move |&n| n != scope)
            .filter(|&n| matches!(self.node(n).data, NodeData::Element(_)))
            .filter(move |&n| self.matches_list(n, list))
    }

    fn matches_list(&self, id: NodeId, list: &SelectorList<SurlSelectors>) -> bool {
        if !matches!(self.node(id).data, NodeData::Element(_)) {
            return false;
        }
        let mut caches = SelectorCaches::default();
        let mut context = MatchingContext::new(
            MatchingMode::Normal,
            None,
            &mut caches,
            selectors::context::QuirksMode::NoQuirks,
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );
        matches_selector_list(list, &ElementRef { doc: self, id }, &mut context)
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::parse_html;

    #[test]
    fn by_tag_id_class() {
        let doc = parse_html(concat!(
            "<!doctype html>",
            r#"<div id="app" class="shell dark"><p class="lead">a</p><p>b</p></div>"#,
        ));
        let root = doc.root();
        assert!(doc.query_selector(root, "#app").unwrap().is_some());
        assert!(doc.query_selector(root, "div.shell.dark").unwrap().is_some());
        assert_eq!(doc.query_selector_all(root, "p").unwrap().len(), 2);
        assert_eq!(doc.query_selector_all(root, "p.lead").unwrap().len(), 1);
        assert!(doc.query_selector(root, ".missing").unwrap().is_none());
    }

    #[test]
    fn combinators_and_attributes() {
        let doc = parse_html(concat!(
            "<!doctype html>",
            "<nav><ul><li><a href='/a'>a</a></li><li><a data-x='1'>b</a></li></ul></nav>",
            "<a href='/outside'>c</a>",
        ));
        let root = doc.root();
        assert_eq!(doc.query_selector_all(root, "nav a").unwrap().len(), 2);
        assert_eq!(doc.query_selector_all(root, "li > a").unwrap().len(), 2);
        assert_eq!(doc.query_selector_all(root, "a[href]").unwrap().len(), 2);
        assert_eq!(doc.query_selector_all(root, "a[data-x='1']").unwrap().len(), 1);
        assert_eq!(
            doc.query_selector_all(root, "li + li a").unwrap().len(),
            1
        );
    }

    #[test]
    fn structural_pseudo_classes() {
        let doc = parse_html("<!doctype html><ul><li>1</li><li>2</li><li>3</li></ul>");
        let root = doc.root();
        let first = doc.query_selector(root, "li:first-child").unwrap().unwrap();
        assert_eq!(doc.text_content(first), "1");
        let last = doc.query_selector(root, "li:last-child").unwrap().unwrap();
        assert_eq!(doc.text_content(last), "3");
        let second = doc.query_selector(root, "li:nth-child(2)").unwrap().unwrap();
        assert_eq!(doc.text_content(second), "2");
        assert!(doc.query_selector(root, "html:root").unwrap().is_some());
    }

    #[test]
    fn scoped_query_excludes_scope() {
        let doc = parse_html(r#"<!doctype html><div id="a"><div id="b"></div></div>"#);
        let root = doc.root();
        let a = doc.query_selector(root, "#a").unwrap().unwrap();
        // scope 自身不参与匹配
        let hits = doc.query_selector_all(a, "div").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(doc.element(hits[0]).unwrap().attr("id"), Some("b"));
    }

    #[test]
    fn element_matches_semantics() {
        let doc = parse_html(r#"<!doctype html><button class="cta" disabled>x</button>"#);
        let root = doc.root();
        let btn = doc.query_selector(root, "button").unwrap().unwrap();
        assert!(doc.element_matches(btn, "button.cta").unwrap());
        assert!(doc.element_matches(btn, ":disabled").unwrap());
        assert!(!doc.element_matches(btn, ":enabled").unwrap());
        assert!(doc.element_matches(btn, "body :is(button, a)").unwrap());
    }

    #[test]
    fn invalid_selector_is_error() {
        let doc = parse_html("<!doctype html>");
        assert!(doc.query_selector(doc.root(), "p[").is_err());
        assert!(doc.query_selector(doc.root(), "::before").is_err());
    }
}
