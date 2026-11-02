//! Structured validation issues for an active list against the catalog.
//!
//! [`validate`] reads the installed records, the active entries in their current
//! order and one target game version, and reports what a user should see about
//! that list: unresolved or unusable entries, identity collisions, dependency
//! gaps, incompatibilities, version support, duplicated installed copies and
//! violated load-order declarations.
//!
//! Nothing here opens a file or runs a process, and no finding is an I/O error:
//! a user-list problem stays an [`Issue`] with a [`IssueCategory::severity`], so
//! the application layer decides what needs acknowledging. Validation never
//! rewrites the list it reads, and it never activates anything to make an issue
//! disappear.
//!
//! # Provider choice
//!
//! A dependency is satisfied by one active provider, chosen by the provider
//! index below: the declared `packageId` first, then the first active entry in
//! `alternativePackageIds` declaration order. Alternatives are an OR, not a
//! requirement to activate every one of them, and an installed copy that is not
//! active never beats an active alternative. The same index is the one the sorter
//! builds its graph from, so validation and sorting always agree on which mod
//! provides a dependency.

use std::collections::{HashMap, HashSet};

use crate::domain::catalog::{ActiveEntry, Catalog};
use crate::domain::types::{
    Dependency, DiagnosticSeverity, GameVersion, ModId, ModMetadata, PackageId,
};

/// How serious an [`Issue`] is.
///
/// The severity classifies the finding; it is not a save or launch gate. Which
/// findings block an operation is an application decision, and a filesystem or
/// operation failure is never one of these warnings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IssueSeverity {
    /// Worth showing, but the list still describes a loadable intention.
    Warning,
    /// The list itself cannot be used as it stands.
    Error,
}

/// What one [`Issue`] is about.
///
/// Declaration order is also the deterministic report order used by
/// [`validate`], so adding a category here changes reported ordering and must be
/// a deliberate choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IssueCategory {
    /// An active entry names no installed copy and keeps its raw token.
    UnresolvedEntry,
    /// A resolved entry names a [`ModId`] the catalog does not hold.
    MissingRecord,
    /// A resolved entry names a record whose metadata is unusable.
    InvalidRecord,
    /// The same identity appears at more than one active position.
    RepeatedEntry,
    /// Two different installed copies of one package id are both active.
    AmbiguousActivePackage,
    /// A dependency has no active provider and no installed copy at all.
    MissingDependency,
    /// A dependency has no active provider, but an installed copy exists.
    InactiveDependency,
    /// Two active mods declare each other incompatible.
    IncompatibleActive,
    /// The record's `supportedVersions` does not name the target release.
    UnsupportedGameVersion,
    /// The record declares no `supportedVersions`, so support is unknown.
    UnknownSupportedVersions,
    /// More than one installed copy declares a package id in use.
    DuplicateInstalledCopies,
    /// A declared load-before or load-after order is violated by the list.
    LoadOrderViolation,
    CommunityRule,
    CommunityConflict,
}

impl IssueCategory {
    /// The severity this category always carries. Keeping severity derived from
    /// the category means an issue cannot claim to be both a warning and an
    /// error about the same finding.
    #[must_use]
    pub const fn severity(self) -> IssueSeverity {
        match self {
            // Identity problems make the list itself unusable for sorting, so
            // they are not something a warning acknowledgement can wave away.
            Self::CommunityConflict
            | Self::MissingRecord
            | Self::InvalidRecord
            | Self::RepeatedEntry
            | Self::AmbiguousActivePackage => IssueSeverity::Error,
            // Everything else is a compatibility or content finding about a list
            // that still says what the user intended to load.
            Self::UnresolvedEntry
            | Self::MissingDependency
            | Self::InactiveDependency
            | Self::IncompatibleActive
            | Self::UnsupportedGameVersion
            | Self::UnknownSupportedVersions
            | Self::DuplicateInstalledCopies
            | Self::LoadOrderViolation
            | Self::CommunityRule => IssueSeverity::Warning,
        }
    }
}

impl std::fmt::Display for IssueCategory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::UnresolvedEntry => "unresolved active entry",
            Self::MissingRecord => "active entry with no installed record",
            Self::InvalidRecord => "active entry with an unusable record",
            Self::RepeatedEntry => "repeated active identity",
            Self::AmbiguousActivePackage => "ambiguous active package",
            Self::MissingDependency => "missing dependency",
            Self::InactiveDependency => "inactive dependency",
            Self::IncompatibleActive => "incompatible active pair",
            Self::UnsupportedGameVersion => "unsupported game version",
            Self::UnknownSupportedVersions => "unknown supported versions",
            Self::DuplicateInstalledCopies => "duplicate installed copies",
            Self::LoadOrderViolation => "load order violation",
            Self::CommunityRule => "community rule",
            Self::CommunityConflict => "community rule conflict",
        };
        formatter.write_str(text)
    }
}

/// One finding about an active list.
///
/// `positions` are indices into the list `validate` was given, `mod_ids` and
/// `tokens` name the affected entries by their real identity and their raw
/// active-list text, and `message` is the human-readable data. The finding never
/// carries a path, a UI type or a serializable shape: those belong to the layer
/// that displays or records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub category: IssueCategory,
    /// Active-list positions this finding is about, ascending.
    pub positions: Vec<usize>,
    /// The installed copies involved, in the order the finding names them.
    pub mod_ids: Vec<ModId>,
    /// The raw active-list tokens involved, in position order.
    pub tokens: Vec<String>,
    pub message: String,
}

impl Issue {
    /// The severity [`self.category`] always carries.
    #[must_use]
    pub fn severity(&self) -> IssueSeverity {
        self.category.severity()
    }

    fn new(
        category: IssueCategory,
        positions: Vec<usize>,
        mod_ids: Vec<ModId>,
        tokens: Vec<String>,
        message: String,
    ) -> Self {
        Self {
            category,
            positions,
            mod_ids,
            tokens,
            message,
        }
    }
}

/// The record an active entry names, when that record can be used at all.
///
/// Two guards, not one: `is_valid()` is about recorded diagnostics, and
/// [`crate::domain::types::ParsedMetadata::default`] can carry neither a package
/// id nor a diagnostic, so a record with no identity has to be excluded here as
/// well. Everything that reads constraints, versions or copy groups goes through
/// this guard, and `entry_findings` reports the same shape as an
/// [`IssueCategory::InvalidRecord`], so validation and sorting refuse it alike.
pub(crate) fn usable_record<'catalog>(
    catalog: &'catalog Catalog,
    entry: &ActiveEntry,
) -> Option<&'catalog ModMetadata> {
    entry
        .mod_id()
        .and_then(|mod_id| catalog.get(mod_id))
        .filter(|record| record.is_valid() && record.parsed.package_id.is_some())
}

/// The active positions that answer a package id, indexed once per list.
///
/// This is the table validation reports against and the same table the sorter
/// builds its graph from, so the two can never disagree about which mod provides
/// a dependency. A position is listed here only when it names a record the catalog
/// holds and can use: an unresolved placeholder, a missing record and an unusable
/// record answer nothing, and each of those is reported (or refused, in sorting)
/// as its own finding instead of being treated as absent.
pub(crate) struct ActiveProviders {
    by_package_id: HashMap<PackageId, Vec<usize>>,
}

impl ActiveProviders {
    /// Indexes every active position whose copy declares a package id.
    ///
    /// Positions stay ascending per package id, so [`ActiveProviders::first`] is
    /// the earliest entry that claims it and a repeated claim stays visible
    /// instead of being averaged away.
    pub(crate) fn build(catalog: &Catalog, active: &[ActiveEntry]) -> Self {
        let mut by_package_id: HashMap<PackageId, Vec<usize>> = HashMap::new();
        for (position, entry) in active.iter().enumerate() {
            let Some(record) = usable_record(catalog, entry) else {
                continue;
            };
            if let Some(package_id) = &record.parsed.package_id {
                by_package_id
                    .entry(package_id.clone())
                    .or_default()
                    .push(position);
            }
        }
        Self { by_package_id }
    }

    /// Every active position declaring `package_id`, ascending.
    pub(crate) fn positions(&self, package_id: &PackageId) -> &[usize] {
        self.by_package_id
            .get(package_id)
            .map_or(&[] as &[usize], Vec::as_slice)
    }

    /// The earliest active position declaring `package_id`, if any.
    pub(crate) fn first(&self, package_id: &PackageId) -> Option<usize> {
        self.positions(package_id).first().copied()
    }

    /// The active entry that provides a declared dependency.
    ///
    /// The declared `packageId` is preferred; only when nothing active provides it
    /// does the first active `alternativePackageIds` entry, in declaration order,
    /// become the provider. One provider is enough: alternatives are an OR, not a
    /// requirement to activate every one of them, and an installed copy that is
    /// not active never outranks an active alternative.
    pub(crate) fn provider(&self, dependency: &Dependency) -> Option<usize> {
        std::iter::once(&dependency.package_id)
            .chain(&dependency.alternatives)
            .find_map(|package_id| self.first(package_id))
    }
}

/// Reports what a user should see about `active`, deterministically.
///
/// The findings come out ordered by category, then by the positions, identities
/// and tokens they name, and identical findings are collapsed. Each rule is one
/// pass over the list with the [`ActiveProviders`] index and one small identity
/// table built up front, so nothing is compared against every other entry and no
/// cycle is enumerated.
#[must_use]
pub fn validate_with_rules(
    catalog: &Catalog,
    active: &[ActiveEntry],
    version: &GameVersion,
    rules: &crate::domain::rules::Ruleset,
) -> Vec<Issue> {
    let mut issues = validate(catalog, active, version);
    for (position, entry) in active.iter().enumerate() {
        if let Some(record) = usable_record(catalog, entry)
            && record.source == crate::domain::types::Source::Official
            && let Some(rule) = record
                .parsed
                .package_id
                .as_ref()
                .and_then(|id| rules.get(id))
            && (rule.top() || rule.bottom())
        {
            issues.push(Issue {
                category: IssueCategory::CommunityRule,
                positions: vec![position],
                mod_ids: vec![record.id.clone()],
                tokens: vec![entry.original_token().into()],
                message: "community top/bottom flags ignored for official content".into(),
            });
        }
    }
    if !rules.rules.is_empty() {
        let finding = match crate::domain::sorting::sort_active_with_rules(catalog, active, rules) {
            Err(error) => Some((IssueCategory::CommunityConflict, error.to_string())),
            Ok(order) if order != active => Some((
                IssueCategory::CommunityRule,
                "current order differs from metadata and community rules; Sort to resolve".into(),
            )),
            _ => None,
        };
        if let Some((category, message)) = finding {
            issues.push(Issue {
                category,
                positions: Vec::new(),
                mod_ids: Vec::new(),
                tokens: Vec::new(),
                message,
            });
        }
    }
    issues
}

#[must_use]
pub fn validate(catalog: &Catalog, active: &[ActiveEntry], version: &GameVersion) -> Vec<Issue> {
    let providers = ActiveProviders::build(catalog, active);
    let mut findings = Vec::new();
    entry_findings(catalog, active, &mut findings);
    identity_findings(catalog, active, &mut findings);
    dependency_findings(catalog, active, &providers, &mut findings);
    incompatibility_findings(catalog, active, &providers, &mut findings);
    version_findings(catalog, active, version, &mut findings);
    duplicate_copy_findings(catalog, active, &mut findings);
    order_findings(catalog, active, &providers, &mut findings);
    findings.sort_by(|a, b| {
        (
            a.category,
            a.positions.as_slice(),
            a.mod_ids.as_slice(),
            a.tokens.as_slice(),
        )
            .cmp(&(
                b.category,
                b.positions.as_slice(),
                b.mod_ids.as_slice(),
                b.tokens.as_slice(),
            ))
    });
    findings.dedup();
    findings
}

/// Placeholders, records the catalog does not hold, and unusable records.
fn entry_findings(catalog: &Catalog, active: &[ActiveEntry], findings: &mut Vec<Issue>) {
    for (position, entry) in active.iter().enumerate() {
        let (category, message, mod_ids) = match entry {
            ActiveEntry::Unresolved {
                original_token,
                reason,
            } => (
                IssueCategory::UnresolvedEntry,
                format!(
                    "the active entry {original_token:?} at position {position} names no installed copy ({reason:?})"
                ),
                Vec::new(),
            ),
            ActiveEntry::Resolved {
                mod_id,
                original_token,
            } => match catalog.get(mod_id) {
                None => (
                    IssueCategory::MissingRecord,
                    format!(
                        "the active entry {original_token:?} at position {position} names a copy the catalog does not hold: {mod_id}"
                    ),
                    vec![mod_id.clone()],
                ),
                Some(record) if record.parsed.package_id.is_none() => (
                    IssueCategory::InvalidRecord,
                    format!(
                        "the active entry {original_token:?} at position {position} names the copy at {mod_id}, whose record declares no package id, so no active-list token can name it and no metadata list can constrain it"
                    ),
                    vec![mod_id.clone()],
                ),
                Some(record) if !record.is_valid() => {
                    let reasons: Vec<String> = record
                        .parsed
                        .diagnostics
                        .iter()
                        .filter(|diagnostic| diagnostic.severity() == DiagnosticSeverity::Error)
                        .map(ToString::to_string)
                        .collect();
                    (
                        IssueCategory::InvalidRecord,
                        format!(
                            "the active entry {original_token:?} at position {position} names the copy at {mod_id}, whose record is unusable ({})",
                            if reasons.is_empty() {
                                "no error diagnostic was recorded".to_string()
                            } else {
                                reasons.join(", ")
                            }
                        ),
                        vec![mod_id.clone()],
                    )
                }
                Some(_) => continue,
            },
        };
        findings.push(Issue::new(
            category,
            vec![position],
            mod_ids,
            vec![entry.original_token().to_string()],
            message,
        ));
    }
}

/// Entries that name the same thing twice, and one package id with two copies.
///
/// Each group of positions becomes a single finding: three spellings of one
/// package id is one report naming all three, not pair-wise comparisons.
fn identity_findings(catalog: &Catalog, active: &[ActiveEntry], findings: &mut Vec<Issue>) {
    let mut by_mod_id: HashMap<&ModId, Vec<usize>> = HashMap::new();
    let mut by_token: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_package_id: HashMap<&PackageId, Vec<(usize, &ModId)>> = HashMap::new();

    for (position, entry) in active.iter().enumerate() {
        match entry {
            ActiveEntry::Resolved { mod_id, .. } => {
                by_mod_id.entry(mod_id).or_default().push(position);
                if let Some(package_id) = catalog
                    .get(mod_id)
                    .and_then(|record| record.parsed.package_id.as_ref())
                {
                    by_package_id
                        .entry(package_id)
                        .or_default()
                        .push((position, mod_id));
                }
            }
            // An unresolved token still names something to the user, so two
            // spellings of the same id are as much a repeat as two copies.
            ActiveEntry::Unresolved { original_token, .. } => {
                let key = PackageId::normalize(original_token)
                    .map(|package_id| package_id.to_string())
                    .unwrap_or_else(|| original_token.trim().to_lowercase());
                by_token.entry(key).or_default().push(position);
            }
        }
    }

    let tokens_at = |positions: &[usize]| -> Vec<String> {
        positions
            .iter()
            .map(|&position| active[position].original_token().to_string())
            .collect()
    };

    let mut repeated_copies: Vec<(&ModId, &Vec<usize>)> = by_mod_id
        .iter()
        .filter(|(_, p)| p.len() > 1)
        .map(|(mod_id, positions)| (*mod_id, positions))
        .collect();
    repeated_copies.sort_by_key(|(mod_id, _)| *mod_id);
    for (mod_id, positions) in repeated_copies {
        findings.push(Issue::new(
            IssueCategory::RepeatedEntry,
            positions.clone(),
            vec![(*mod_id).clone()],
            tokens_at(positions),
            format!(
                "the installed copy at {mod_id} is active {} time(s), at positions {positions:?}",
                positions.len()
            ),
        ));
    }

    let mut repeated_tokens: Vec<(&String, &Vec<usize>)> =
        by_token.iter().filter(|(_, p)| p.len() > 1).collect();
    repeated_tokens.sort_by_key(|(key, _)| *key);
    for (key, positions) in repeated_tokens {
        findings.push(Issue::new(
            IssueCategory::RepeatedEntry,
            positions.clone(),
            Vec::new(),
            tokens_at(positions),
            format!(
                "the package id {key} is listed {} times, at positions {positions:?}, and resolves to no installed copy",
                positions.len()
            ),
        ));
    }

    let mut ambiguous: Vec<(&PackageId, Vec<usize>, Vec<ModId>)> = by_package_id
        .iter()
        .filter(|(_, claims)| {
            claims
                .iter()
                .map(|(_, id)| *id)
                .collect::<HashSet<_>>()
                .len()
                > 1
        })
        .map(|(package_id, claims)| {
            let mut ids: Vec<ModId> = claims.iter().map(|(_, id)| (*id).clone()).collect();
            ids.sort_unstable();
            ids.dedup();
            (
                *package_id,
                claims.iter().map(|(position, _)| *position).collect(),
                ids,
            )
        })
        .collect();
    ambiguous.sort_by_key(|(package_id, _, _)| *package_id);
    for (package_id, positions, mod_ids) in ambiguous {
        findings.push(Issue::new(
            IssueCategory::AmbiguousActivePackage,
            positions.clone(),
            mod_ids,
            tokens_at(&positions),
            format!(
                "package id {package_id} is active as {} different installed copies, at positions {positions:?}; the list cannot say which one to load",
                positions.len()
            ),
        ));
    }
}

/// Dependencies no active entry provides.
///
/// A dependency whose provider is installed but inactive is reported apart from
/// one nothing is installed for: the first is an activation the user can make,
/// the second is content the machine does not have.
fn dependency_findings(
    catalog: &Catalog,
    active: &[ActiveEntry],
    providers: &ActiveProviders,
    findings: &mut Vec<Issue>,
) {
    for (position, entry) in active.iter().enumerate() {
        let Some(record) = usable_record(catalog, entry) else {
            continue;
        };
        for dependency in &record.parsed.dependencies {
            if providers.provider(dependency).is_some() {
                continue;
            }
            let candidates: Vec<&PackageId> = std::iter::once(&dependency.package_id)
                .chain(&dependency.alternatives)
                .collect();
            let listed = candidates
                .iter()
                .map(|package_id| package_id.to_string())
                .collect::<Vec<_>>()
                .join(" or ");
            let installed: Vec<String> = candidates
                .iter()
                .filter(|package_id| catalog.copies(package_id).next().is_some())
                .map(ToString::to_string)
                .collect();
            let token = entry.original_token().to_string();
            let (category, message) = if installed.is_empty() {
                (
                    IssueCategory::MissingDependency,
                    format!(
                        "the mod at position {position} ({token:?}) depends on {listed}, and nothing installed provides any of them"
                    ),
                )
            } else {
                (
                    IssueCategory::InactiveDependency,
                    format!(
                        "the mod at position {position} ({token:?}) depends on {listed}; {} is installed but not active",
                        installed.join(" and ")
                    ),
                )
            };
            findings.push(Issue::new(
                category,
                vec![position],
                vec![record.id.clone()],
                vec![token],
                message,
            ));
        }
    }
}

/// Active pairs that declare each other incompatible.
///
/// One finding per unordered pair of positions, named in ascending position
/// order, whichever side declared it. A pair set indexed by the canonical
/// `(earlier, later)` positions decides on first encounter, in the order the
/// list is read: the other record's own `incompatibleWith` list is never
/// searched again, so a hub with many leaves costs one step per declaration
/// instead of one search per declaration pair.
fn incompatibility_findings(
    catalog: &Catalog,
    active: &[ActiveEntry],
    providers: &ActiveProviders,
    findings: &mut Vec<Issue>,
) {
    let mut reported_pairs: HashSet<(usize, usize)> = HashSet::new();
    for (position, entry) in active.iter().enumerate() {
        let Some(record) = usable_record(catalog, entry) else {
            continue;
        };
        for package_id in &record.parsed.incompatible_with {
            let Some(other) = providers.first(package_id) else {
                continue;
            };
            if other == position {
                continue;
            }
            let (first, second) = if position < other {
                (position, other)
            } else {
                (other, position)
            };
            if !reported_pairs.insert((first, second)) {
                continue;
            }
            let (Some(first_record), Some(second_record)) = (
                usable_record(catalog, &active[first]),
                usable_record(catalog, &active[second]),
            ) else {
                continue;
            };
            findings.push(Issue::new(
                IssueCategory::IncompatibleActive,
                vec![first, second],
                vec![first_record.id.clone(), second_record.id.clone()],
                vec![
                    active[first].original_token().to_string(),
                    active[second].original_token().to_string(),
                ],
                format!(
                    "the mods at positions {first} and {second} are declared incompatible: {:?} cannot load with {:?} (package id {package_id})",
                    active[first].original_token(),
                    active[second].original_token(),
                ),
            ));
        }
    }
}

/// Game-version support of every usable active record.
fn version_findings(
    catalog: &Catalog,
    active: &[ActiveEntry],
    version: &GameVersion,
    findings: &mut Vec<Issue>,
) {
    for (position, entry) in active.iter().enumerate() {
        let Some(record) = usable_record(catalog, entry) else {
            continue;
        };
        let supported: Vec<String> = record
            .parsed
            .supported_versions
            .iter()
            .map(ToString::to_string)
            .collect();
        let (category, message) = if supported.is_empty() {
            (
                IssueCategory::UnknownSupportedVersions,
                format!(
                    "the mod at position {position} ({:?}) declares no supportedVersions, so whether it works with the target release {version} is unknown",
                    entry.original_token(),
                ),
            )
        } else if record.parsed.supports_version(version) {
            continue;
        } else {
            (
                IssueCategory::UnsupportedGameVersion,
                format!(
                    "the mod at position {position} ({:?}) supports {}, not the target release {version}",
                    entry.original_token(),
                    supported.join(", "),
                ),
            )
        };
        findings.push(Issue::new(
            category,
            vec![position],
            vec![record.id.clone()],
            vec![entry.original_token().to_string()],
            message,
        ));
    }
}

/// Package ids in use that more than one installed copy declares.
///
/// Reported once per package id. `mod_ids` lists every installed copy in the
/// catalog's bare-token preference order, while the message names the copy this
/// entry actually selected, which is not necessarily the first one: a
/// `_steam`-qualified token deliberately chooses a different copy.
fn duplicate_copy_findings(catalog: &Catalog, active: &[ActiveEntry], findings: &mut Vec<Issue>) {
    let mut reported: HashSet<&PackageId> = HashSet::new();
    for (position, entry) in active.iter().enumerate() {
        let Some(record) = usable_record(catalog, entry) else {
            continue;
        };
        let Some(package_id) = &record.parsed.package_id else {
            continue;
        };
        // The first row that asks about this package id decides, before the copy
        // group is collected or cloned: a list that repeats one installed copy
        // many times must not clone that group once per repeating row.
        if !reported.insert(package_id) {
            continue;
        }
        let copies: Vec<ModId> = catalog
            .copies(package_id)
            .map(|copy| copy.id.clone())
            .collect();
        if copies.len() < 2 {
            continue;
        }
        let installed = copies.len();
        findings.push(Issue::new(
            IssueCategory::DuplicateInstalledCopies,
            vec![position],
            copies,
            vec![entry.original_token().to_string()],
            format!(
                "package id {package_id} is installed {} times; the entry at position {position} ({:?}) selects {mod_id}",
                installed,
                entry.original_token(),
                mod_id = record.id,
            ),
        ));
    }
}

/// Load-before and load-after declarations the list violates.
///
/// `forceLoadBefore` and `forceLoadAfter` were already merged into the parsed
/// ordering fields by the metadata reader, so they are honored here without a
/// second rule. Each violated declaration is one finding naming the declarer
/// first.
fn order_findings(
    catalog: &Catalog,
    active: &[ActiveEntry],
    providers: &ActiveProviders,
    findings: &mut Vec<Issue>,
) {
    for (position, entry) in active.iter().enumerate() {
        let Some(record) = usable_record(catalog, entry) else {
            continue;
        };
        let mut declarations: Vec<(&PackageId, &str)> = record
            .parsed
            .load_before
            .iter()
            .map(|target| (target, "loadBefore"))
            .chain(
                record
                    .parsed
                    .load_after
                    .iter()
                    .map(|target| (target, "loadAfter")),
            )
            .collect();
        // Deterministic for a record that names the same target twice.
        declarations.sort_unstable_by(|a, b| (*a.0).cmp(b.0).then_with(|| a.1.cmp(b.1)));
        for (package_id, field) in declarations {
            let after = field == "loadAfter";
            let Some(target) = providers.first(package_id) else {
                continue;
            };
            if target == position {
                continue;
            }
            // `loadBefore` wants this entry earlier than the target, `loadAfter`
            // later; either way the violation is the list disagreeing with it.
            if (after && position > target) || (!after && position < target) {
                continue;
            }
            let direction = if after { "after" } else { "before" };
            let Some(target_record) = usable_record(catalog, &active[target]) else {
                continue;
            };
            findings.push(Issue::new(
                IssueCategory::LoadOrderViolation,
                vec![position, target],
                vec![record.id.clone(), target_record.id.clone()],
                vec![
                    entry.original_token().to_string(),
                    active[target].original_token().to_string(),
                ],
                format!(
                    "the mod at position {position} ({:?}) declares {field} {package_id}, so it should load {direction} the entry at position {target} ({:?})",
                    entry.original_token(),
                    active[target].original_token(),
                ),
            ));
        }
    }
}
