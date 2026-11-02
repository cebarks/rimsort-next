//! Immutable, normalized community constraints; no transport or persistence here.
use crate::domain::types::PackageId;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ruleset {
    pub(crate) timestamp: Option<u64>,
    pub(crate) rules: BTreeMap<PackageId, ModRules>,
}
impl Ruleset {
    pub fn timestamp(&self) -> Option<u64> {
        self.timestamp
    }
    pub fn get(&self, id: &PackageId) -> Option<&ModRules> {
        self.rules.get(id)
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModRules {
    pub(crate) before: Vec<RuleEdge>,
    pub(crate) after: Vec<RuleEdge>,
    pub(crate) top: bool,
    pub(crate) bottom: bool,
    pub(crate) top_comments: Vec<String>,
    pub(crate) bottom_comments: Vec<String>,
}
impl ModRules {
    pub fn before(&self) -> &[RuleEdge] {
        &self.before
    }
    pub fn after(&self) -> &[RuleEdge] {
        &self.after
    }
    pub fn top(&self) -> bool {
        self.top
    }
    pub fn bottom(&self) -> bool {
        self.bottom
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleEdge {
    pub target: PackageId,
    pub comments: Vec<String>,
}
