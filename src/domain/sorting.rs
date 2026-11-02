//! Deterministic topological sorting of an active list.
//!
//! [`sort_active`] reorders cloned [`ActiveEntry`] values and nothing else: it
//! never activates a mod, never drops an entry, never re-resolves a token and
//! never touches the caller's list. A refusal returns a typed [`SortError`] and
//! leaves the input exactly as it was.
//!
//! # What the graph is built from
//!
//! Only resolved, usable, unambiguous active copies become nodes. Unresolved
//! placeholders, repeated identities, two active copies of one package id,
//! records the catalog does not hold and records with unusable metadata are all
//! refused up front, because a list that cannot say which copy to load cannot be
//! ordered honestly.
//!
//! Edges come from the same provider choice validation uses
//! ([`crate::domain::validation`]): the declared `packageId` if it is active,
//! otherwise the first active `alternativePackageIds` entry. A dependency whose
//! provider is not active creates no edge and no phantom node; the entry list
//! that comes back has exactly the length that went in.
//!
//! # Tiers
//!
//! Installed game content stays ahead of user mods. Official-to-official and
//! user-to-user edges are enforced inside their own phase, an official-to-user
//! edge is already satisfied by the barrier, and a user-to-official edge is a
//! contradiction the caller hears about as [`SortError::TierConflicts`] rather
//! than a silently dropped edge. Community rules split user content into top,
//! ordinary and bottom tiers. Every backwards cross-tier edge is refused. The
//! barriers cost no edges of their own: four phases, not a bipartite graph.
//!
//! # Level order
//!
//! Within a phase this is level-based Kahn: every node that is ready right now is
//! snapshotted, that whole level is sorted by normalised display name (falling
//! back to the package id), then package id, then raw [`ModId`], and the level is
//! removed before the next one is formed. A priority queue would interleave a
//! node that became ready *from* this level into the middle of it, which is
//! exactly the instability this task rejects.

use std::collections::{HashMap, HashSet};

use crate::domain::catalog::{ActiveEntry, Catalog, UnresolvedReason};
use crate::domain::rules::Ruleset;
use crate::domain::types::{ModId, ModMetadata, PackageId, Source};
use crate::domain::validation::ActiveProviders;

/// Why one active position cannot take part in sorting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RejectionReason {
    /// The token names no installed copy.
    Unresolved { reason: UnresolvedReason },
    /// The copy it names is not in the catalog.
    MissingRecord,
    /// The copy's metadata is unusable, so it declares no usable constraints.
    InvalidRecord,
    /// Another position already names this exact copy.
    RepeatedIdentity,
    /// Another position names a different copy of the same package id.
    DuplicateActivePackage,
}

impl std::fmt::Display for RejectionReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unresolved { reason } => write!(
                formatter,
                "resolves to no installed copy (reason {reason:?})"
            ),
            Self::MissingRecord => formatter.write_str("names a copy the catalog does not hold"),
            Self::InvalidRecord => formatter.write_str("names a copy with unusable metadata"),
            Self::RepeatedIdentity => formatter.write_str("repeats an already active copy"),
            Self::DuplicateActivePackage => {
                formatter.write_str("activates a second copy of an already active package id")
            }
        }
    }
}

/// One refused active position, named by position, raw token and identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedEntry {
    pub position: usize,
    pub token: String,
    /// The copy the entry names, when it names one.
    pub mod_id: Option<ModId>,
    /// The package id of that copy, when the record declares one.
    pub package_id: Option<PackageId>,
    pub reason: RejectionReason,
}

impl RejectedEntry {
    /// The reported text for this position.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.mod_id {
            Some(mod_id) => format!(
                "position {} ({mod_id}, token {:?}) {}",
                self.position, self.token, self.reason
            ),
            None => format!(
                "position {} (token {:?}) {}",
                self.position, self.token, self.reason
            ),
        }
    }
}

/// The kind of metadata declaration that produced an edge.
///
/// `Hash` joins the other derives because tier contradictions are deduplicated
/// by `(from, to, kind)` in a set, which keeps that report linear.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConstraintKind {
    /// A `modDependencies` entry (including an active alternative).
    Dependency,
    /// A `loadBefore` or `forceLoadBefore` entry.
    LoadBefore,
    /// A `loadAfter` or `forceLoadAfter` entry.
    LoadAfter,
    CommunityBefore,
    CommunityAfter,
}

impl std::fmt::Display for ConstraintKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Dependency => "dependency",
            Self::LoadBefore => "load-before",
            Self::LoadAfter => "load-after",
            Self::CommunityBefore => "community load-before",
            Self::CommunityAfter => "community load-after",
        };
        formatter.write_str(text)
    }
}

/// A declaration that forces a user mod ahead of installed game content.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TierConflict {
    /// The installed game content that would have to move behind a user mod.
    pub official: ModId,
    /// The user mod whose declaration demands that.
    pub user: ModId,
    pub kind: ConstraintKind,
}

impl TierConflict {
    /// The reported text for this contradiction.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} must load before installed game content {} because of a {} constraint",
            self.user, self.official, self.kind
        )
    }
}

/// One mod that could not be placed because of a cycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CycleMember {
    pub position: usize,
    pub mod_id: ModId,
    pub package_id: PackageId,
    pub token: String,
}

impl CycleMember {
    /// The reported text for this blocked mod.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "position {} ({}, package id {}, token {:?})",
            self.position, self.mod_id, self.package_id, self.token
        )
    }
}

/// Why an active list could not be sorted.
///
/// Every variant reports a bounded set: the refused positions, the contradictory
/// pairs, the mods still blocked. None of them enumerates every cycle in the
/// graph.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SortError {
    #[error("cannot sort: {message}")]
    Community { message: String },
    /// Positions the graph refuses to use, listed in position order.
    #[error(
        "cannot sort: {}",
        rejected.iter().map(RejectedEntry::describe).collect::<Vec<_>>().join("; ")
    )]
    EntriesRejected { rejected: Vec<RejectedEntry> },
    /// User-mod declarations that contradict the game-content tier.
    #[error(
        "cannot sort: {} user mod(s) must load before installed game content: {}",
        conflicts.len(),
        conflicts.iter().map(TierConflict::describe).collect::<Vec<_>>().join("; ")
    )]
    TierConflicts { conflicts: Vec<TierConflict> },
    /// The remaining blocked mods, in position order.
    #[error(
        "cannot sort: a load-order cycle blocks {} mod(s): {}",
        members.len(),
        members.iter().map(CycleMember::describe).collect::<Vec<_>>().join("; ")
    )]
    Cycle { members: Vec<CycleMember> },
}

/// Sorts `active` into a deterministic load order.
///
/// The returned vector holds clones of the same entries, so every original
/// token, source selection and identity survives the reorder. The caller's list
/// is never mutated, which is what lets a failed sort leave a session untouched.
pub fn sort_active(
    catalog: &Catalog,
    active: &[ActiveEntry],
) -> Result<Vec<ActiveEntry>, SortError> {
    sort_active_with_rules(catalog, active, &Ruleset::default())
}

pub fn sort_active_with_rules(
    catalog: &Catalog,
    active: &[ActiveEntry],
    rules: &Ruleset,
) -> Result<Vec<ActiveEntry>, SortError> {
    match sort_with(catalog, active, rules, &mut always_continue) {
        Ok(order) => Ok(order),
        Err(Stopped::Refused(error)) => Err(error),
        // `Infallible` holds no value and `always_continue` answers "keep going" every
        // time, so a sort nobody asked to stop cannot report a stop. The refusal it would
        // have to invent does not exist: this arm is exhausted by the type instead.
        Err(Stopped::Cancelled(never)) => match never {},
    }
}

/// Why a sort that was allowed to be interrupted did not return an order.
///
/// The two answers are kept apart because only one of them is a statement about the
/// user's list: a cancellation says no order was produced, a refusal says this list
/// cannot be put into load order at all.
#[derive(Debug)]
pub(crate) enum SortStop {
    /// The caller's check said stop before a complete order existed.
    Cancelled,
    /// The list cannot be sorted, in the words [`sort_active`] reports it in.
    Refused(SortError),
}

/// [`sort_active`] with the cooperative stop boundary a background pass needs.
///
/// `cancelled` is asked once per active position while the graph is built and once per
/// level while levels are placed, so a sort stops within one position or one level of
/// being asked to instead of only after ordering the whole list. The graph, the tiers,
/// the level keys, the refusal reports and the order itself are unchanged.
#[cfg(test)]
pub(crate) fn sort_active_checked(
    catalog: &Catalog,
    active: &[ActiveEntry],
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<ActiveEntry>, SortStop> {
    sort_active_with_rules_checked(catalog, active, &Ruleset::default(), cancelled)
}

pub(crate) fn sort_active_with_rules_checked(
    catalog: &Catalog,
    active: &[ActiveEntry],
    rules: &Ruleset,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<ActiveEntry>, SortStop> {
    match sort_with(catalog, active, rules, &mut || {
        if cancelled() { Err(()) } else { Ok(()) }
    }) {
        Ok(order) => Ok(order),
        Err(Stopped::Cancelled(())) => Err(SortStop::Cancelled),
        Err(Stopped::Refused(error)) => Err(SortStop::Refused(error)),
    }
}

/// Why one pass of the shared ordering stopped.
///
/// `C` is what the caller's check hands back when it says stop: [`std::convert::Infallible`]
/// for a pass that cannot be asked, which is what makes that caller's cancellation arm
/// exhaustive without inventing a `SortError` about a list that sorted just fine.
enum Stopped<C> {
    /// The caller asked this sort to stop, carrying the caller's own stop value through.
    Cancelled(C),
    /// This list cannot be sorted, in the words [`sort_active`] has always used.
    Refused(SortError),
}

/// The check a sort that cannot be stopped runs.
fn always_continue() -> Result<(), std::convert::Infallible> {
    Ok(())
}

/// Runs one boundary check, carrying the caller's stop value through unchanged.
fn stop_at<C>(cancelled: &mut dyn FnMut() -> Result<(), C>) -> Result<(), Stopped<C>> {
    match cancelled() {
        Ok(()) => Ok(()),
        Err(stop) => Err(Stopped::Cancelled(stop)),
    }
}

/// The whole ordering, over one cancellation boundary.
fn sort_with<C>(
    catalog: &Catalog,
    active: &[ActiveEntry],
    rules: &Ruleset,
    cancelled: &mut dyn FnMut() -> Result<(), C>,
) -> Result<Vec<ActiveEntry>, Stopped<C>> {
    let graph = Graph::build(catalog, active, rules, cancelled)?;
    graph.order(active, cancelled)
}

/// One active copy that takes part in the graph, with its level order key.
struct Node<'catalog> {
    mod_id: ModId,
    package_id: PackageId,
    /// Official, top, ordinary, bottom, in that order.
    tier: u8,
    /// Normalised display name, falling back to the package id, then the package
    /// id and the directory identity, so no two mods in one level can tie.
    level_key: (String, PackageId, ModId),
    record: &'catalog ModMetadata,
}

/// The graph of one active list: nodes in position order, edges built once.
struct Graph<'catalog> {
    nodes: Vec<Node<'catalog>>,
    /// Same-tier successors per position, deduplicated so one constraint counts
    /// one in-degree no matter how many metadata lists declared it.
    successors: Vec<Vec<usize>>,
    in_degree: Vec<usize>,
    community: Vec<String>,
}

impl<'catalog> Graph<'catalog> {
    /// Refuses unusable input, then turns the surviving list into edges.
    ///
    /// Refusal comes first and is complete: one [`SortError`] naming every
    /// rejected position, or every tier contradiction, before any ordering is
    /// attempted. Nothing here activates a mod, resolves a token again, drops an
    /// entry or re-picks a copy.
    fn build<C>(
        catalog: &'catalog Catalog,
        active: &[ActiveEntry],
        rules: &Ruleset,
        cancelled: &mut dyn FnMut() -> Result<(), C>,
    ) -> Result<Self, Stopped<C>> {
        let mut nodes: Vec<Node> = Vec::with_capacity(active.len());
        let mut rejected: Vec<RejectedEntry> = Vec::new();
        let mut seen_mod_ids: HashSet<&ModId> = HashSet::new();
        let mut seen_package_ids: HashMap<&PackageId, usize> = HashMap::new();

        for (position, entry) in active.iter().enumerate() {
            // One boundary per active row: a large list is refused or ordered one row at
            // a time, so a stop asked for here costs one row of work, not the pass.
            stop_at(cancelled)?;
            let token = entry.original_token().to_string();
            let accepted = match entry {
                ActiveEntry::Unresolved { reason, .. } => {
                    rejected.push(RejectedEntry {
                        position,
                        token,
                        mod_id: None,
                        package_id: None,
                        reason: RejectionReason::Unresolved { reason: *reason },
                    });
                    None
                }
                ActiveEntry::Resolved { mod_id, .. } => match catalog.get(mod_id) {
                    None => {
                        rejected.push(RejectedEntry {
                            position,
                            token,
                            mod_id: Some(mod_id.clone()),
                            package_id: None,
                            reason: RejectionReason::MissingRecord,
                        });
                        None
                    }
                    Some(record) if !record.is_valid() => {
                        rejected.push(RejectedEntry {
                            position,
                            token,
                            mod_id: Some(mod_id.clone()),
                            package_id: record.parsed.package_id.clone(),
                            reason: RejectionReason::InvalidRecord,
                        });
                        None
                    }
                    Some(record) => match &record.parsed.package_id {
                        // A record that declares no package id can be inspected
                        // but cannot be constrained: no metadata list can name it.
                        None => {
                            rejected.push(RejectedEntry {
                                position,
                                token,
                                mod_id: Some(mod_id.clone()),
                                package_id: None,
                                reason: RejectionReason::InvalidRecord,
                            });
                            None
                        }
                        Some(package_id) if !seen_mod_ids.insert(mod_id) => {
                            rejected.push(RejectedEntry {
                                position,
                                token,
                                mod_id: Some(mod_id.clone()),
                                package_id: Some(package_id.clone()),
                                reason: RejectionReason::RepeatedIdentity,
                            });
                            None
                        }
                        Some(package_id) if seen_package_ids.contains_key(package_id) => {
                            rejected.push(RejectedEntry {
                                position,
                                token,
                                mod_id: Some(mod_id.clone()),
                                package_id: Some(package_id.clone()),
                                reason: RejectionReason::DuplicateActivePackage,
                            });
                            None
                        }
                        Some(package_id) => {
                            seen_package_ids.insert(package_id, position);
                            Some(Node {
                                mod_id: mod_id.clone(),
                                package_id: package_id.clone(),
                                tier: if record.source == Source::Official {
                                    0
                                } else {
                                    2
                                },
                                level_key: level_key(record, package_id),
                                record,
                            })
                        }
                    },
                },
            };
            if let Some(node) = accepted {
                nodes.push(node);
            }
        }

        if !rejected.is_empty() {
            return Err(Stopped::Refused(SortError::EntriesRejected { rejected }));
        }

        // The same provider table validation reports from: the dependency the
        // user is told about and the edge the graph draws are one decision.
        for node in &mut nodes {
            if let Some(rule) = rules.get(&node.package_id) {
                if node.tier == 0 {
                    continue;
                }
                if rule.top() && rule.bottom() {
                    return Err(Stopped::Refused(SortError::Community {
                        message: format!(
                            "community flags for {} request both top and bottom ({:?}; {:?})",
                            node.package_id, rule.top_comments, rule.bottom_comments
                        ),
                    }));
                }
                node.tier = if rule.top() {
                    1
                } else if rule.bottom() {
                    3
                } else {
                    2
                };
            }
        }
        let providers = ActiveProviders::build(catalog, active);
        let mut declarations: Vec<(usize, usize, ConstraintKind)> = Vec::new();
        for (position, node) in nodes.iter().enumerate() {
            for dependency in &node.record.parsed.dependencies {
                if let Some(provider) = providers.provider(dependency)
                    && provider != position
                {
                    declarations.push((provider, position, ConstraintKind::Dependency));
                }
            }
            for target in &node.record.parsed.load_before {
                if let Some(other) = providers.first(target)
                    && other != position
                {
                    declarations.push((position, other, ConstraintKind::LoadBefore));
                }
            }
            for target in &node.record.parsed.load_after {
                if let Some(other) = providers.first(target)
                    && other != position
                {
                    declarations.push((other, position, ConstraintKind::LoadAfter));
                }
            }
        }

        let mut community = Vec::new();
        let mut provenance = HashMap::new();
        for (position, node) in nodes.iter().enumerate() {
            if let Some(rule) = rules.get(&node.package_id) {
                for (edges, kind) in [
                    (rule.before(), ConstraintKind::CommunityBefore),
                    (rule.after(), ConstraintKind::CommunityAfter),
                ] {
                    for edge in edges {
                        if let Some(other) = providers.first(&edge.target) {
                            let (from, to) = if kind == ConstraintKind::CommunityBefore {
                                (position, other)
                            } else {
                                (other, position)
                            };
                            let description = format!(
                                "{} {} {}: {}",
                                node.package_id,
                                kind,
                                edge.target,
                                edge.comments.join("; ")
                            );
                            community.push(description.clone());
                            provenance.insert((from, to, kind), description);
                            declarations.push((from, to, kind));
                        }
                    }
                }
            }
        }
        let mut edges: HashSet<(usize, usize)> = HashSet::new();
        let mut conflicts: Vec<TierConflict> = Vec::new();
        let mut seen_conflicts: HashSet<(usize, usize, ConstraintKind)> = HashSet::new();
        for (from, to, kind) in declarations {
            let (from_tier, to_tier) = (nodes[from].tier, nodes[to].tier);
            if from_tier > to_tier && (to_tier != 0 || provenance.contains_key(&(from, to, kind))) {
                return Err(Stopped::Refused(SortError::Community {
                    message: format!(
                        "{} ({}) must precede {} ({}) due to {}; backwards across official/top/ordinary/bottom tiers. {}",
                        nodes[from].package_id,
                        from_tier,
                        nodes[to].package_id,
                        to_tier,
                        kind,
                        provenance
                            .get(&(from, to, kind))
                            .map(String::as_str)
                            .unwrap_or("metadata constraint conflicts with community tier flags")
                    ),
                }));
            }
            if from_tier > to_tier {
                // The barrier puts installed game content ahead of every user mod,
                // so this edge cannot be honored. Reporting it beats dropping it
                // and ordering the list against its own metadata.
                let conflict = TierConflict {
                    official: nodes[to].mod_id.clone(),
                    user: nodes[from].mod_id.clone(),
                    kind,
                };
                // Indexed membership, keyed including the constraint kind, so
                // many user mods pinning one copy of game content costs one
                // hashing step per declaration instead of a search over the
                // conflicts collected so far. Order is the order the declarations
                // were read, which is already deterministic.
                if seen_conflicts.insert((from, to, kind)) {
                    conflicts.push(conflict);
                }
            } else if from_tier == to_tier {
                edges.insert((from, to));
            }
            // Game content ahead of a user mod needs no edge: the phase order
            // already guarantees it, and spelling it out would make the graph a
            // bipartite pair count instead of V + E.
        }
        if !conflicts.is_empty() {
            return Err(Stopped::Refused(SortError::TierConflicts { conflicts }));
        }

        let mut successors: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
        let mut in_degree = vec![0usize; nodes.len()];
        for &(from, to) in &edges {
            successors[from].push(to);
            in_degree[to] += 1;
        }
        // Successor adjacency is deliberately left unordered: a level is sorted
        // by its placement key before the whole level is published, and that key
        // is unique per copy, so sorting every successor list would add work
        // without changing one output position.
        Ok(Self {
            nodes,
            successors,
            in_degree,
            community,
        })
    }

    /// Places official, top, ordinary and bottom tiers, one whole level at a time.
    ///
    /// Every node ready now is snapshotted, sorted and removed before the next
    /// level is formed, so a node that only became ready from this level waits.
    /// Whatever is left when a level comes up empty is the cycle: reporting those
    /// members is bounded, and enumerating the cycles themselves is not required.
    /// One cancellation boundary is taken per level, so a stop lands between whole
    /// published levels and never leaves a half-ordered list behind.
    fn order<C>(
        &self,
        active: &[ActiveEntry],
        cancelled: &mut dyn FnMut() -> Result<(), C>,
    ) -> Result<Vec<ActiveEntry>, Stopped<C>> {
        let mut in_degree = self.in_degree.clone();
        let mut placed: Vec<usize> = Vec::with_capacity(self.nodes.len());
        for tier in 0..4 {
            let members: Vec<usize> = (0..self.nodes.len())
                .filter(|&position| self.nodes[position].tier == tier)
                .collect();
            let mut level: Vec<usize> = members
                .iter()
                .copied()
                .filter(|&position| in_degree[position] == 0)
                .collect();
            let mut placed_from_level = 0usize;
            while !level.is_empty() {
                stop_at(cancelled)?;
                level.sort_by(|&a, &b| self.nodes[a].level_key.cmp(&self.nodes[b].level_key));
                let mut next: Vec<usize> = Vec::new();
                for &position in &level {
                    placed.push(position);
                    placed_from_level += 1;
                    for &successor in &self.successors[position] {
                        in_degree[successor] -= 1;
                        if in_degree[successor] == 0 {
                            next.push(successor);
                        }
                    }
                }
                level = next;
            }
            if placed_from_level != members.len() {
                let blocked: Vec<CycleMember> = members
                    .iter()
                    .copied()
                    .filter(|&position| in_degree[position] > 0)
                    .map(|position| CycleMember {
                        position,
                        mod_id: self.nodes[position].mod_id.clone(),
                        package_id: self.nodes[position].package_id.clone(),
                        token: active[position].original_token().to_string(),
                    })
                    .collect();
                if !self.community.is_empty() {
                    return Err(Stopped::Refused(SortError::Community {
                        message: format!(
                            "cycle blocks {}; active community constraints: {}",
                            blocked
                                .iter()
                                .map(CycleMember::describe)
                                .collect::<Vec<_>>()
                                .join("; "),
                            self.community.join("; ")
                        ),
                    }));
                }
                return Err(Stopped::Refused(SortError::Cycle { members: blocked }));
            }
        }
        Ok(placed
            .into_iter()
            .map(|position| active[position].clone())
            .collect())
    }
}

/// The order one record is placed at within its level.
///
/// The display name is trimmed and lowercased, so `Harmony` and `harmony` sort
/// together, and falls back to the package id because installed game content
/// legitimately carries no `<name>` element at all. Lowered text is compared as
/// bytes: a locale collation would make the same list order differently on two
/// machines, and the package id and directory identity break any remaining tie.
///
/// Folding is case-only, so a name that begins with a non-ASCII letter sorts
/// after every ASCII name (code points above U+007F follow ASCII bytes). That is
/// the documented cost of a deterministic order without a collation dependency:
/// stable everywhere, not alphabetical in every locale.
fn level_key(record: &ModMetadata, package_id: &PackageId) -> (String, PackageId, ModId) {
    let name = record
        .parsed
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| name.to_lowercase())
        .unwrap_or_else(|| package_id.to_string());
    (name, package_id.clone(), record.id.clone())
}

#[cfg(test)]
mod checked_sort_tests {
    //! The cooperative boundary of [`super::sort_active_checked`], proved by counting the
    //! checks the build pass and the level pass make. Stopping in the middle of ordering is
    //! a property of those loops, so it is shown here rather than inferred from a jobs
    //! boundary that could only ask before or after the pass ran.
    use super::{SortError, SortStop, sort_active, sort_active_checked};
    use crate::domain::catalog::ActiveEntry;
    use crate::domain::catalog::Catalog;
    use crate::domain::types::{ModId, ModMetadata, PackageId, ParsedMetadata, Source};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn package(raw: &str) -> PackageId {
        PackageId::normalize(raw).expect("the package id must normalize")
    }

    /// A usable copy, optionally declaring that it loads after `after`.
    fn record(path: &str, source: Source, package_id: &str, after: Option<&str>) -> ModMetadata {
        let mut parsed = ParsedMetadata {
            package_id: Some(package(package_id)),
            name: Some(package_id.to_string()),
            ..ParsedMetadata::default()
        };
        if let Some(after) = after {
            parsed.load_after = vec![package(after)];
        }
        ModMetadata::new(ModId::new(path), source, parsed)
    }

    /// Game content, a user mod, and a user mod that must load after it: two tiers and two
    /// levels inside the user tier.
    fn catalog() -> Catalog {
        Catalog::new(vec![
            record("/game/Data", Source::Official, "base.game", None),
            record("/mods/provider", Source::Local, "provider.mod", None),
            record(
                "/mods/dependent",
                Source::Local,
                "dependent.mod",
                Some("provider.mod"),
            ),
        ])
        .expect("the catalog must build")
    }

    /// The same three copies, handed in an order the sort has to change.
    fn active() -> Vec<ActiveEntry> {
        ["/mods/dependent", "/game/Data", "/mods/provider"]
            .into_iter()
            .map(|path| {
                let token = match path {
                    "/mods/dependent" => "dependent.mod",
                    "/game/Data" => "base.game",
                    _ => "provider.mod",
                };
                ActiveEntry::Resolved {
                    mod_id: ModId::new(path),
                    original_token: token.to_string(),
                }
            })
            .collect()
    }

    struct Counter {
        asked: AtomicUsize,
        stop_after: usize,
    }

    impl Counter {
        fn stopping_at(stop_after: usize) -> Self {
            Self {
                asked: AtomicUsize::new(0),
                stop_after,
            }
        }

        fn check(&self) -> bool {
            let count = self.asked.fetch_add(1, Ordering::Relaxed) + 1;
            count >= self.stop_after
        }

        fn asked(&self) -> usize {
            self.asked.load(Ordering::Relaxed)
        }
    }

    #[test]
    fn a_check_raised_while_the_graph_is_built_stops_before_any_order_exists() {
        let catalog = catalog();
        let active = active();
        // Three rows, so a stop asked for at the second boundary is inside the build pass.
        let counter = Counter::stopping_at(2);

        let stopped = sort_active_checked(&catalog, &active, &|| counter.check());

        assert!(
            matches!(stopped, Err(SortStop::Cancelled)),
            "a sort stopped while the graph is being built must report the stop: {stopped:?}"
        );
        assert_eq!(
            counter.asked(),
            2,
            "the pass stopped at the row that said stop instead of building the whole graph"
        );
        assert_eq!(
            active.len(),
            3,
            "and the list it was given is untouched, as `sort_active` has always left it"
        );
    }

    #[test]
    fn a_check_raised_between_levels_stops_after_the_levels_it_had_placed() {
        let catalog = catalog();
        let active = active();
        // The build pass asks once per row: three times here. Asking it to stop at the
        // fifth boundary puts the stop inside the ordering pass, after tiers and levels
        // have already been walked.
        let counter = Counter::stopping_at(5);

        let stopped = sort_active_checked(&catalog, &active, &|| counter.check());

        assert!(
            matches!(stopped, Err(SortStop::Cancelled)),
            "a sort stopped between levels must report the stop: {stopped:?}"
        );
        assert!(
            counter.asked() >= 4,
            "the stop happened past the build pass, inside the level loop: {} asks",
            counter.asked()
        );
        assert_eq!(
            counter.asked(),
            5,
            "and exactly at the level boundary that said stop, not after ordering everything"
        );
    }

    #[test]
    fn a_check_that_never_says_stop_produces_exactly_the_public_order() {
        let catalog = catalog();
        let active = active();
        let counter = Counter::stopping_at(usize::MAX);

        let checked = sort_active_checked(&catalog, &active, &|| counter.check())
            .expect("no stop was asked, so an order must come back");
        let public = sort_active(&catalog, &active).expect("the same list must sort");

        assert_eq!(
            checked, public,
            "the checked pass orders exactly what the plain pass does"
        );
        assert_eq!(
            counter.asked(),
            6,
            "three build boundaries and one per level: the boundaries really were taken"
        );
    }

    #[test]
    fn a_list_that_cannot_be_sorted_refuses_the_same_way_in_both_paths() {
        // A repeated identity is the accepted refusal, and a cancellation must never be
        // dressed up as one: the checked path has to return the very same refusal.
        let catalog = catalog();
        let mut active = active();
        active.push(active[0].clone());

        let public = sort_active(&catalog, &active).expect_err("a repeated copy cannot be sorted");
        let counter = Counter::stopping_at(usize::MAX);
        let checked =
            sort_active_checked(&catalog, &active, &|| counter.check()).expect_err("same refusal");

        match checked {
            SortStop::Refused(error) => assert_eq!(
                error, public,
                "the refusal is the sorter's own, unchanged by the boundary around it"
            ),
            other => panic!("a refusal must not come back as a cancellation: {other:?}"),
        }
        match public {
            SortError::EntriesRejected { rejected } => assert_eq!(
                rejected.len(),
                1,
                "and the refusal still names exactly the row that repeats"
            ),
            other => panic!("expected the repeated-identity refusal, got {other}"),
        }
    }
}
