//! The editing session: one loaded configuration, bound to the catalog and
//! target version it was read with.
//!
//! A session is the only place the four things that must stay together live:
//! the [`Catalog`] of installed copies, the [`ConfigSnapshot`] bytes as loaded,
//! the configuration path they came from and the target [`GameVersion`]. None of
//! them is re-read from settings, none is rebound later: an edit made against a
//! session can only ever be about the list this session loaded, which is what
//! keeps a stale session from being written through a newly edited
//! `Settings::config_path`.
//!
//! Two entry lists are held: `saved`, the entries as loaded, and `working`, the
//! entries as edited, plus a private provenance string per working row that keeps
//! track of the raw text each occurrence came in with. Dirty status is
//! `working != saved`, never a sticky flag, so an edit that is reversed really
//! does clear it. Every mutation is checked
//! against the whole command before anything is applied: a rejected command
//! leaves the working list, the revision and the dirty state exactly as they
//! were, so a failed edit can never lose the edits before it.
//!
//! # Which copies a session may put into the list
//!
//! An active-list token names a package id, optionally with the trailing `_steam`
//! marker; it cannot name an arbitrary physical directory. So a new activation or
//! an explicit copy switch is only accepted when a token this catalog's resolver
//! understands — the bare package id, or that id plus a trailing `_steam` —
//! resolves back to exactly the copy the user chose. An unencodable choice is
//! rejected up front, before any state changes, instead of being saved as a
//! different copy. Every installed copy stays inspectable through the catalog,
//! and loaded tokens that resolve to nothing are preserved untouched until the
//! user removes or replaces them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::domain::catalog::{ActiveEntry, Catalog, resolve_active};
use crate::domain::rules::Ruleset;
use crate::domain::sorting::{SortError, sort_active_with_rules};
use crate::domain::types::{Error, GameVersion, ModId, PackageId};
use crate::domain::validation::{Issue, validate_with_rules};
use crate::integrations::config::{ConfigSnapshot, parse_config};

/// Why a requested edit was refused.
///
/// Every variant names what the caller has to change; a refusal never means the
/// session is partly edited, because checks run before any mutation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The catalog holds no copy with this identity.
    #[error("no installed copy is at {mod_id}")]
    UnknownMod { mod_id: ModId },
    /// The copy exists, but its metadata is unusable, so it declares nothing.
    #[error("the copy at {mod_id} has unusable metadata, so it cannot be activated")]
    InvalidRecord { mod_id: ModId },
    /// The copy exists but declares no package id, so no token can name it.
    #[error("the copy at {mod_id} declares no package id, so no active-list token can name it")]
    MissingPackageId { mod_id: ModId },
    /// Another copy of the same package id is already active.
    #[error(
        "package id {package_id} is already active at position {active_position} as {active_mod_id}; \
         switching to {wanted_mod_id} needs an explicit replacement of that position"
    )]
    AlreadyActive {
        package_id: PackageId,
        active_position: usize,
        active_mod_id: ModId,
        wanted_mod_id: ModId,
    },
    /// No supported token resolves back to the chosen copy.
    #[error(
        "no supported active-list token names the copy at {mod_id} (package id {package_id}), \
         so this copy choice cannot be encoded"
    )]
    Unencodable {
        mod_id: ModId,
        package_id: PackageId,
    },
    /// The position is not part of the working list.
    #[error("position {position} is outside the working list of {working_len} entries")]
    PositionOutOfRange { position: usize, working_len: usize },
    /// The position holds a token that names no installed copy.
    #[error(
        "the entry at position {position} is the unresolved token {token:?}, which names no copy"
    )]
    UnresolvedPosition { position: usize, token: String },
    /// The position holds a different package id than the replacement copy.
    #[error(
        "the entry at position {position} is package id {entry_package_id}, \
         so it cannot be switched to {wanted_package_id}"
    )]
    PackageIdMismatch {
        position: usize,
        entry_package_id: PackageId,
        wanted_package_id: PackageId,
    },
}

/// Distinguishes one loaded session from every other session this process has
/// built, so a workflow can tell "the session I was started with" from another one
/// holding coincidentally identical bytes. Never reused, never guessed from data.
static NEXT_SESSION_GENERATION: AtomicU64 = AtomicU64::new(1);

/// The loaded configuration, the edits made to it and the identity of both.
#[derive(Debug)]
pub struct Session {
    /// Shared so a later background job can read the exact records the edits
    /// were validated against, without the session handing out mutable state.
    catalog: Arc<Catalog>,
    rules: Arc<Ruleset>,
    loaded_config: ConfigSnapshot,
    config_path: PathBuf,
    target_version: GameVersion,
    saved: Vec<ActiveEntry>,
    working: Vec<ActiveEntry>,
    /// The raw text each working row came in with, position by position.
    ///
    /// A loaded row's provenance is that row's own loaded token, so two inherited
    /// occurrences of one copy under different spellings stay distinguishable even
    /// though [`ActiveEntry`] itself carries no occurrence identity. This vector is
    /// private: the public lists stay plain [`ActiveEntry`] slices. Every command
    /// that adds, removes, reorders or discards a working row applies the same
    /// change to this one, so the two are always the same length and row `i` of the
    /// working list owns provenance `i`. A refused or no-op command touches
    /// neither. A row the session adds itself owns the token it was written with,
    /// which is what lets a later copy switch restore it instead of re-spelling
    /// the row.
    provenance: Vec<String>,
    revision: u64,
    /// This session's own identity, assigned when it was built.
    generation: u64,
}

impl Session {
    /// Loads the snapshot's tokens against `catalog` and starts clean.
    ///
    /// Every token becomes an entry, in file order: repeated tokens keep their
    /// repeated positions and tokens that resolve to nothing stay unresolved, so
    /// a malformed list a user inherited is still editable and still preservable.
    pub fn new(
        catalog: Catalog,
        loaded_config: ConfigSnapshot,
        config_path: PathBuf,
        target_version: GameVersion,
    ) -> Self {
        let catalog = Arc::new(catalog);
        let saved = resolve_active(&loaded_config, &catalog);
        let provenance = Self::loaded_provenance(&saved);
        Self {
            catalog,
            rules: Arc::default(),
            loaded_config,
            config_path,
            target_version,
            working: saved.clone(),
            saved,
            provenance,
            revision: 0,
            generation: NEXT_SESSION_GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// The identity of this loaded session, for the workflow that bound itself to
    /// it. Crate-private: an editing session is not a token to hand out.
    #[must_use]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// The raw text of each entry, position by position.
    ///
    /// One pass, and it initializes every row separately, so repeated ids,
    /// differently spelled duplicates and opaque placeholders each keep their own
    /// provenance instead of sharing one.
    fn loaded_provenance(entries: &[ActiveEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| entry.original_token().to_string())
            .collect()
    }

    #[must_use]
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The installed copies these edits were validated against, shared rather than
    /// copied.
    ///
    /// A background pass has to read the exact records the session was built with, and
    /// one reference-count bump is the whole cost of handing them over: the catalog is
    /// never rebuilt and never copied once per row. What is shared is the same
    /// immutable records the session itself reads, not a mutable handle on the session.
    #[must_use]
    pub(crate) fn catalog_arc(&self) -> Arc<Catalog> {
        Arc::clone(&self.catalog)
    }

    /// The bytes and spans exactly as loaded, never a re-serialization.
    #[must_use]
    pub fn loaded_config(&self) -> &ConfigSnapshot {
        &self.loaded_config
    }

    /// The file these entries came from. A save writes here, or not at all.
    #[must_use]
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// The game release validation reports version support against.
    #[must_use]
    pub fn target_version(&self) -> &GameVersion {
        &self.target_version
    }

    /// The entries as loaded.
    #[must_use]
    pub fn saved_entries(&self) -> &[ActiveEntry] {
        &self.saved
    }

    /// The entries as edited, in load order.
    #[must_use]
    pub fn working_entries(&self) -> &[ActiveEntry] {
        &self.working
    }

    /// Bumped by every effective change, never by a refused or no-op command.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// True when the working list differs from the loaded list.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.working != self.saved
    }

    /// What a user should see about the working list, right now.
    #[must_use]
    pub fn validate(&self) -> Vec<Issue> {
        validate_with_rules(
            &self.catalog,
            &self.working,
            &self.target_version,
            &self.rules,
        )
    }

    pub fn rules_arc(&self) -> Arc<Ruleset> {
        Arc::clone(&self.rules)
    }

    /// Invalidate rule-dependent work without changing the saved game configuration.
    pub fn set_rules(&mut self, rules: Arc<Ruleset>) -> bool {
        if self.rules == rules {
            return false;
        }
        self.rules = rules;
        self.revision += 1;
        true
    }

    /// Sorts the working list, publishing it only when sorting succeeds.
    ///
    /// A refusal returns the [`SortError`] and leaves the pending edits, the
    /// revision and the dirty state untouched. Sorting an already ordered list is
    /// a no-op that does not bump the revision.
    pub fn sort(&mut self) -> Result<(), SortError> {
        let sorted = sort_active_with_rules(&self.catalog, &self.working, &self.rules)?;
        // `sort_active` returns exactly the rows it was handed, rearranged, so the
        // publish step a background order also goes through cannot refuse this one.
        let _moved = self.publish_order(&sorted);
        Ok(())
    }

    /// Publishes a load order that was produced elsewhere for this session's list.
    ///
    /// Crate-private by owner decision: the accepted public editing verbs stay
    /// [`Session::sort`], [`Session::activate`] and friends, and an order a background
    /// pass computed reaches the list through this one gate instead of a new public
    /// setter.
    ///
    /// The order is accepted only when it rearranges the rows this session holds now:
    /// the same number of rows, each matched exactly once, by physical copy identity
    /// together with its own token for a resolved row and by its own token for an
    /// unresolved one. An order made for a list that has since been edited is therefore
    /// refused, so a result that arrived late cannot quietly overwrite what the user did
    /// while it was running. `apply_sort_order` in [`crate::application::jobs`] checks
    /// the session generation and revision in front of this gate.
    ///
    /// Every check runs before anything is applied, so a refusal leaves the working
    /// list, each row's provenance, the revision and the dirty state exactly as they
    /// were. Publishing an order the list already has does nothing and spends no
    /// revision, which is why the answer says whether anything moved.
    pub(crate) fn apply_sorted(&mut self, sorted: &[ActiveEntry]) -> Result<bool, ApplySortError> {
        if sorted.len() != self.working.len() {
            return Err(ApplySortError::LengthMismatch {
                sorted: sorted.len(),
                working: self.working.len(),
            });
        }
        // The owner's rule for this gate: it may publish exactly what the accepted sorter
        // was willing to order, and nothing else. Unresolved placeholders, copies this
        // catalog cannot use, a repeated physical copy and two active copies of one package
        // id are refused here for the same reason the sorter refuses them - none of them
        // says which row carries which history. After row eligibility and membership,
        // exact ordering is verified against this session's immutable rules as well.
        self.sortable_rows()?;
        for entry in sorted {
            Self::sortable_row(entry)?;
        }
        // One count table over the rows held now, so a candidate order is checked in one
        // pass and every row of it has to be consumed exactly once. Both lists are made of
        // unique resolved copies by the checks above, so copy identity plus the row's own
        // text is exact, and the provenance reorder below may index by copy.
        let mut remaining: HashMap<(Option<ModId>, &str), usize> =
            HashMap::with_capacity(self.working.len());
        for entry in &self.working {
            *remaining.entry(row_identity(entry)).or_insert(0) += 1;
        }
        for entry in sorted {
            match remaining.get_mut(&row_identity(entry)) {
                Some(count) if *count > 0 => *count -= 1,
                _ => {
                    return Err(ApplySortError::ForeignEntry {
                        description: describe(entry),
                    });
                }
            }
        }
        if sort_active_with_rules(&self.catalog, &self.working, &self.rules).as_deref()
            != Ok(sorted)
        {
            return Err(ApplySortError::UnexpectedOrder);
        }
        Ok(self.publish_order(sorted))
    }

    /// Checks that this session's own list is one the sorter could have ordered.
    fn sortable_rows(&self) -> Result<(), ApplySortError> {
        let mut seen_mod_ids: HashSet<&ModId> = HashSet::with_capacity(self.working.len());
        let mut seen_packages: HashMap<&PackageId, &ActiveEntry> =
            HashMap::with_capacity(self.working.len());
        for entry in &self.working {
            let mod_id = Self::sortable_row(entry)?;
            if !seen_mod_ids.insert(mod_id) {
                return Err(ApplySortError::RepeatedCopy {
                    description: describe(entry),
                });
            }
            let package_id = self.package_of(mod_id)?;
            if let Some(other) = seen_packages.insert(package_id, entry) {
                return Err(ApplySortError::AmbiguousPackage {
                    package_id: package_id.clone(),
                    first: describe(other),
                    second: describe(entry),
                });
            }
        }
        Ok(())
    }

    /// Checks one row and returns the copy it names.
    ///
    /// Mirrors the sorter's own up-front refusals: a token that resolves to nothing, a copy
    /// the catalog does not hold, a copy whose metadata is unusable and a copy that declares
    /// no package id all stay out of any published order.
    fn sortable_row(entry: &ActiveEntry) -> Result<&ModId, ApplySortError> {
        let ActiveEntry::Resolved { mod_id, .. } = entry else {
            return Err(ApplySortError::UnresolvedRow {
                description: describe(entry),
            });
        };
        Ok(mod_id)
    }

    /// The package id a resolved copy declares, or the refusal that says it declares none.
    fn package_of(&self, mod_id: &ModId) -> Result<&PackageId, ApplySortError> {
        let Some(record) = self.catalog.get(mod_id) else {
            return Err(ApplySortError::UnusableRecord {
                description: format!("the copy at {mod_id}"),
                reason: "is not in the catalog this session was loaded with",
            });
        };
        if !record.is_valid() {
            return Err(ApplySortError::UnusableRecord {
                description: format!("the copy at {mod_id}"),
                reason: "has unusable metadata",
            });
        }
        match &record.parsed.package_id {
            Some(package_id) => Ok(package_id),
            None => Err(ApplySortError::UnusableRecord {
                description: format!("the copy at {mod_id}"),
                reason: "declares no package id",
            }),
        }
    }

    /// Reorders the working list and each row's own provenance together.
    ///
    /// This is the one place either of them moves, so an order published from a
    /// background pass and an order this session computed itself cannot drift apart in
    /// how a row's history travels with it. Reordering a list that is already in this
    /// order does nothing at all and spends no revision.
    fn publish_order(&mut self, sorted: &[ActiveEntry]) -> bool {
        if sorted == self.working {
            return false;
        }
        let provenance = self.provenance_in(sorted);
        self.working = sorted.to_vec();
        self.provenance = provenance;
        self.revision += 1;
        true
    }

    /// The provenance vector reordered the same way as the working list.
    ///
    /// Sorting only accepts unambiguous input, so each accepted copy's identity
    /// names exactly one row of the list it came from and the association can be
    /// rebuilt with one index pass, never a search of every row per output row. A
    /// row that somehow arrives without a known identity keeps its own text, which
    /// describes no new provenance rather than inventing one.
    fn provenance_in(&self, sorted: &[ActiveEntry]) -> Vec<String> {
        let mut by_identity: HashMap<&ModId, &String> = HashMap::with_capacity(self.working.len());
        for (entry, provenance) in self.working.iter().zip(&self.provenance) {
            if let Some(mod_id) = entry.mod_id() {
                by_identity.insert(mod_id, provenance);
            }
        }
        sorted
            .iter()
            .map(|entry| {
                entry
                    .mod_id()
                    .and_then(|mod_id| by_identity.get(mod_id).map(|text| (*text).clone()))
                    .unwrap_or_else(|| entry.original_token().to_string())
            })
            .collect()
    }

    /// Appends `mod_id` to the working list under the token that names it.
    ///
    /// Refuses a copy the list format cannot encode, a copy with no usable
    /// record, and a second copy of a package id that is already active; those
    /// need an explicit [`Session::replace_copy`] on the position that holds the
    /// first copy. Activating a copy that is already active changes nothing.
    ///
    /// A copy the loaded list once named is written back with its loaded token,
    /// so removing an entry and activating that same copy again is a complete
    /// reversal rather than a re-spelling of the same package id.
    pub fn activate(&mut self, mod_id: &ModId) -> Result<(), EditError> {
        let package_id = self.usable_copy(mod_id)?;
        if let Some((position, active)) = self.active_position(&package_id) {
            if active == *mod_id {
                return Ok(());
            }
            return Err(EditError::AlreadyActive {
                package_id,
                active_position: position,
                active_mod_id: active,
                wanted_mod_id: mod_id.clone(),
            });
        }

        let loaded_token = self.loaded_token_for(mod_id);
        let token = self.encodable_token(mod_id, &package_id, loaded_token.as_deref())?;
        self.provenance.push(token.clone());
        self.working.push(ActiveEntry::Resolved {
            mod_id: mod_id.clone(),
            original_token: token,
        });
        self.revision += 1;
        Ok(())
    }

    /// Removes the working entry at `position`, resolved or unresolved.
    ///
    /// An unresolved placeholder is removable on purpose: it is the explicit
    /// discard of a token the user no longer wants preserved, and nothing else
    /// in this session drops a loaded token.
    pub fn deactivate(&mut self, position: usize) -> Result<(), EditError> {
        if position >= self.working.len() {
            return Err(EditError::PositionOutOfRange {
                position,
                working_len: self.working.len(),
            });
        }
        self.working.remove(position);
        self.provenance.remove(position);
        self.revision += 1;
        Ok(())
    }

    /// Moves the entry at `from` so it lands at index `to`.
    ///
    /// Positions are working-list positions, never a filtered view's row index.
    /// `to` is an index in the resulting list, so the largest destination is the
    /// last position of the current list; an entry moved onto its own index is
    /// already there and changes nothing, and neither does moving an entry across
    /// a run of rows that are already equal to it.
    pub fn move_entry(&mut self, from: usize, to: usize) -> Result<(), EditError> {
        let working_len = self.working.len();
        for position in [from, to] {
            if position >= working_len {
                return Err(EditError::PositionOutOfRange {
                    position,
                    working_len,
                });
            }
        }
        if from == to {
            return Ok(());
        }
        // Repeated inherited rows are indistinguishable, so shifting an entry
        // across a run of entries equal to it produces the same vector. That is
        // not an effective change: it must not spend a revision, and dirty state
        // keeps comparing the real vectors instead of tracking a flag.
        let shifted = if from < to {
            &self.working[from + 1..=to]
        } else {
            &self.working[to..from]
        };
        if shifted.iter().all(|entry| entry == &self.working[from]) {
            return Ok(());
        }
        let entry = self.working.remove(from);
        let provenance = self.provenance.remove(from);
        self.working.insert(to, entry);
        self.provenance.insert(to, provenance);
        self.revision += 1;
        Ok(())
    }

    /// Switches the copy at `position` to another installed copy of the same
    /// package id, writing the token that names the new copy.
    ///
    /// This is the only way a session changes which physical copy is used, and
    /// it never changes which package id is active: the position has to name the
    /// same package id, the chosen copy has to be encodable, and it must not
    /// already be active at another position. Reversing a switch writes that row's
    /// own provenance back verbatim - the text the occurrence came in with, carried
    /// with the row through moves, sorting and refusals - so the pending-edit flag
    /// clears even after the list has been reordered, and two inherited occurrences
    /// of one copy never trade spellings. A copy the row never came in as is written
    /// with the canonical supported spelling.
    pub fn replace_copy(&mut self, position: usize, mod_id: &ModId) -> Result<(), EditError> {
        let working_len = self.working.len();
        let entry = self
            .working
            .get(position)
            .ok_or(EditError::PositionOutOfRange {
                position,
                working_len,
            })?;
        let ActiveEntry::Resolved {
            mod_id: current,
            original_token: _,
        } = entry
        else {
            return Err(EditError::UnresolvedPosition {
                position,
                token: entry.original_token().to_string(),
            });
        };
        let entry_package_id = self.usable_copy(current)?;
        if current == mod_id {
            return Ok(());
        }
        let wanted_package_id = self.usable_copy(mod_id)?;
        if wanted_package_id != entry_package_id {
            return Err(EditError::PackageIdMismatch {
                position,
                entry_package_id,
                wanted_package_id,
            });
        }
        if let Some((other, _)) = self
            .working
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.mod_id() == Some(mod_id))
        {
            return Err(EditError::AlreadyActive {
                package_id: wanted_package_id,
                active_position: other,
                active_mod_id: mod_id.clone(),
                wanted_mod_id: mod_id.clone(),
            });
        }

        // The row's own provenance is the candidate, not the first saved row that
        // happens to name this copy: an inherited list can hold one copy under two
        // spellings, and the surviving occurrence has to come back as itself.
        let row_provenance = self.provenance.get(position).cloned();
        let token = self.encodable_token(mod_id, &wanted_package_id, row_provenance.as_deref())?;
        let replacement = ActiveEntry::Resolved {
            mod_id: mod_id.clone(),
            original_token: token,
        };
        if self.working[position] == replacement {
            return Ok(());
        }
        // The provenance stays with the row: it records where this occurrence came
        // from, which a copy switch does not change.
        self.working[position] = replacement;
        self.revision += 1;
        Ok(())
    }

    /// Discards the pending edits and goes back to the loaded list.
    ///
    /// Returns true when there was something to discard: a session that is
    /// already clean keeps its revision.
    pub fn reset(&mut self) -> bool {
        let provenance = Self::loaded_provenance(&self.saved);
        if self.working == self.saved {
            // Re-assert the loaded provenance even when the list already is the
            // loaded list, so no earlier edit can leave a row holding another
            // occurrence's history. This is not a change: no revision is spent.
            self.provenance = provenance;
            return false;
        }
        self.working = self.saved.clone();
        self.provenance = provenance;
        self.revision += 1;
        true
    }

    /// Adopts a save that committed as this session's persistence baseline.
    ///
    /// Crate-private by owner decision: acknowledging a write belongs to the
    /// workflow, which is the one place that has already checked the revision the
    /// bytes were prepared at. Nothing else may move the baseline, so there is no
    /// new external `Session` API for it.
    ///
    /// `committed` has to describe the list this session is holding: it must read
    /// back as the load of its own bytes, hold exactly one entry per working row,
    /// and carry each row's own [`ActiveEntry::original_token`] as its saved text.
    /// A list that was edited, sorted, deduplicated or re-spelled in between is
    /// refused, because adopting it would have the session claim somebody else's
    /// bytes are what the user is looking at.
    ///
    /// Every check runs before anything is applied, so a refused or stale
    /// acknowledgement leaves the loaded bytes, the saved list, the provenance
    /// and the revision exactly as they were: an acknowledgement cannot lose the
    /// edits it just rejected.
    ///
    /// Adopting a save is not an edit. The working list did not change, so no
    /// revision is spent; `is_dirty` goes false because the saved side caught up
    /// with it, and the next save is prepared against these bytes instead of the
    /// original load. Each row's provenance becomes the token it was actually
    /// written with, which is what makes a later reversal restore the spelling on
    /// disk rather than the spelling the first load happened to use.
    pub(crate) fn acknowledge_save(&mut self, committed: &ConfigSnapshot) -> Result<(), Error> {
        if parse_config(&committed.raw)? != *committed {
            return Err(self.refused_acknowledgement(
                "the snapshot is not the load of its own bytes, so it cannot be the new baseline"
                    .to_string(),
            ));
        }
        if committed.active_tokens.len() != self.working.len() {
            return Err(self.refused_acknowledgement(format!(
                "the saved file reads as {} entries, not the {} the session is holding",
                committed.active_tokens.len(),
                self.working.len()
            )));
        }
        for (position, (token, entry)) in committed
            .active_tokens
            .iter()
            .zip(&self.working)
            .enumerate()
        {
            if token != entry.original_token() {
                return Err(self.refused_acknowledgement(format!(
                    "entry {position} was saved as {token:?} while the session holds {:?}, so the \
                     acknowledged list is not the edited one",
                    entry.original_token()
                )));
            }
        }

        self.loaded_config = committed.clone();
        self.saved = self.working.clone();
        self.provenance = Self::loaded_provenance(&self.saved);
        Ok(())
    }

    /// Names the session whose acknowledgement was refused, with the reason.
    fn refused_acknowledgement(&self, message: String) -> Error {
        Error::InvalidInput {
            context: format!("save acknowledgement for {}", self.config_path.display()),
            message,
        }
    }

    /// The loaded text that named `mod_id`, for a copy the user is bringing back.
    ///
    /// Activation adds a new row, so it has no row provenance of its own yet, and
    /// a moved or shortened list can no longer align positions with the file it
    /// came from. Where the loaded list named this copy more than once, the
    /// earliest loaded spelling is the one brought back; a copy the loaded list
    /// named once is restored exactly. [`Session::replace_copy`] does not use
    /// this lookup, because an edited row carries its own provenance. Either way
    /// the value is only a candidate for [`Session::encodable_token`], which
    /// resolves it through the catalog and publishes it only when it names this
    /// very copy.
    ///
    /// A copy already active at another position never reaches this point: the
    /// same-package guard refuses an added duplicate first, so a repeated loaded
    /// occurrence is not a licence to activate another copy of it.
    fn loaded_token_for(&self, mod_id: &ModId) -> Option<String> {
        self.saved.iter().find_map(|entry| match entry {
            ActiveEntry::Resolved {
                mod_id: saved_id,
                original_token,
            } if saved_id == mod_id => Some(original_token.clone()),
            _ => None,
        })
    }

    /// The package id of a copy this session may put into the active list.
    ///
    /// A copy the catalog does not hold, or whose record is unusable, or that
    /// declares no package id, cannot be named by any token, so it is refused
    /// here rather than encoded as something else.
    fn usable_copy(&self, mod_id: &ModId) -> Result<PackageId, EditError> {
        let record = self
            .catalog
            .get(mod_id)
            .ok_or_else(|| EditError::UnknownMod {
                mod_id: mod_id.clone(),
            })?;
        if !record.is_valid() {
            return Err(EditError::InvalidRecord {
                mod_id: mod_id.clone(),
            });
        }
        record
            .parsed
            .package_id
            .clone()
            .ok_or_else(|| EditError::MissingPackageId {
                mod_id: mod_id.clone(),
            })
    }

    /// The first working position whose copy declares `package_id`.
    fn active_position(&self, package_id: &PackageId) -> Option<(usize, ModId)> {
        self.working
            .iter()
            .enumerate()
            .find_map(|(position, entry)| {
                let mod_id = entry.mod_id()?;
                let declares = self
                    .catalog
                    .get(mod_id)?
                    .parsed
                    .package_id
                    .as_ref()
                    .is_some_and(|candidate| candidate == package_id);
                declares.then(|| (position, mod_id.clone()))
            })
    }

    /// The token that names `mod_id`, or [`EditError::Unencodable`].
    ///
    /// Only spellings the catalog's own resolver understands are offered, and
    /// each is accepted only after resolving it back to the very copy asked for.
    /// `preferred` is tried first so a reversal can restore loaded text; it is
    /// not trusted blindly, it is resolved like everything else.
    fn encodable_token(
        &self,
        mod_id: &ModId,
        package_id: &PackageId,
        preferred: Option<&str>,
    ) -> Result<String, EditError> {
        let candidates = preferred
            .map(str::to_string)
            .into_iter()
            .chain(encodings_for(package_id));
        for candidate in candidates {
            if self
                .catalog
                .resolve_token(&candidate)
                .mod_id()
                .is_some_and(|found| found == mod_id)
            {
                return Ok(candidate);
            }
        }
        Err(EditError::Unencodable {
            mod_id: mod_id.clone(),
            package_id: package_id.clone(),
        })
    }
}

/// Why a load order that was produced elsewhere cannot be published in this session.
///
/// Each variant names what the caller has to do about it, and a refusal changes nothing:
/// the working list, every row's provenance, the revision and the dirty state stay
/// exactly as they were.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ApplySortError {
    #[error("order does not match the current immutable rules and metadata")]
    UnexpectedOrder,
    /// The order was made for a different number of rows.
    #[error(
        "the order holds {sorted} entries, not this session's {working}, so it was not produced from this list"
    )]
    LengthMismatch { sorted: usize, working: usize },
    /// The order holds a row this session is not holding.
    #[error("the order holds {description}, which is not a row this session is holding")]
    ForeignEntry { description: String },
    /// A row names no installed copy, so no load order can carry it - which is also why an
    /// inherited placeholder keeps its own reason instead of adopting another one.
    #[error(
        "the list holds {description}, which names no installed copy, so it cannot take part in a load order"
    )]
    UnresolvedRow { description: String },
    /// A named copy cannot be ordered at all.
    #[error("the list holds {description}, which {reason}, so it cannot take part in a load order")]
    UnusableRecord {
        description: String,
        reason: &'static str,
    },
    /// One physical copy appears more than once, so the rows cannot be told apart by copy.
    #[error(
        "{description} appears more than once, so an order cannot say which occurrence carries which history"
    )]
    RepeatedCopy { description: String },
    /// Two active copies declare one package id, which no order can resolve.
    #[error(
        "package id {package_id} is active as {first} and {second}, so an order cannot say which copy carries which history"
    )]
    AmbiguousPackage {
        package_id: PackageId,
        first: String,
        second: String,
    },
}

/// What makes one working row itself, for comparing an order against the list it claims
/// to rearrange.
///
/// A resolved row is named by the physical copy it points at together with the text it
/// was read as, so two inherited occurrences of one copy stay two rows rather than one
/// interchangeable pair; an unresolved row is named by its own text, because that text
/// is all it has.
fn row_identity(entry: &ActiveEntry) -> (Option<ModId>, &str) {
    (entry.mod_id().cloned(), entry.original_token())
}

/// The reported text for one row of a candidate order.
fn describe(entry: &ActiveEntry) -> String {
    match entry {
        ActiveEntry::Resolved { mod_id, .. } => {
            format!("the copy at {mod_id} (token {:?})", entry.original_token())
        }
        ActiveEntry::Unresolved { .. } => {
            format!("the unresolved token {:?}", entry.original_token())
        }
    }
}

/// The active-list spellings that may name one installed copy.
///
/// A `ModsConfig.xml` entry carries a package id and, at most, the trailing
/// `_steam` marker the resolver already supports. Nothing else is offered: no
/// `_git`, no path, no index. Both spellings are still checked by
/// [`Catalog::resolve_token`] back to the wanted copy before they are used.
fn encodings_for(package_id: &PackageId) -> [String; 2] {
    [
        package_id.as_str().to_string(),
        format!("{}_steam", package_id.as_str()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::catalog::UnresolvedReason;
    use crate::domain::sorting::sort_active;
    use crate::domain::types::{ModMetadata, ParsedMetadata, Source};

    const CORE: &str = "/game/Data";
    const HARMONY: &str = "/mods/harmony";
    const CORE_TOKEN: &str = "Ludeon.RimWorld";
    const HARMONY_TOKEN: &str = "brrainz.harmony";

    fn target() -> GameVersion {
        GameVersion::parse("1.5.4171").expect("the target release must parse")
    }

    fn record(path: &str, package_id: &str, source: Source) -> ModMetadata {
        let normalized = PackageId::normalize(package_id)
            .unwrap_or_else(|| panic!("{package_id:?} must normalize"));
        ModMetadata::new(
            ModId::new(path),
            source,
            ParsedMetadata {
                package_id: Some(normalized),
                name: Some("Test copy".to_string()),
                ..ParsedMetadata::default()
            },
        )
    }

    fn catalog() -> Catalog {
        Catalog::new(vec![
            record(CORE, "ludeon.rimworld", Source::Official),
            record(HARMONY, "brrainz.harmony", Source::Local),
        ])
        .expect("the catalog must build")
    }

    /// A `ModsConfig.xml` holding `tokens` verbatim, loaded exactly as a session
    /// would load the file a save wrote.
    fn loaded(tokens: &[&str]) -> ConfigSnapshot {
        let mut document = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ModsConfigData>\n  \
             <version>1.5</version>\n  <activeMods>\n",
        );
        for token in tokens {
            document.push_str(&format!("    <li>{token}</li>\n"));
        }
        document.push_str("  </activeMods>\n</ModsConfigData>\n");
        parse_config(document.as_bytes()).expect("the document must load")
    }

    fn session(loaded_tokens: &[&str]) -> Session {
        Session::new(
            catalog(),
            loaded(loaded_tokens),
            PathBuf::from("/config/RimWorld/ModsConfig.xml"),
            target(),
        )
    }

    /// A session holding one pending edit: the loaded list plus Harmony.
    fn edited() -> Session {
        let mut edited = session(&[CORE_TOKEN]);
        edited
            .activate(&ModId::new(HARMONY))
            .expect("activating an installed copy is encodable");
        edited
    }

    #[test]
    fn an_acknowledged_save_becomes_the_baseline_without_spending_a_revision() {
        let mut edited = edited();
        let revision = edited.revision();
        let committed = loaded(&[CORE_TOKEN, HARMONY_TOKEN]);

        edited
            .acknowledge_save(&committed)
            .expect("the list this session is holding is the list that was saved");

        assert_eq!(
            edited.loaded_config(),
            &committed,
            "the bytes that landed become the next save's baseline"
        );
        assert_eq!(
            edited.saved_entries(),
            edited.working_entries(),
            "the saved list is the working vector that was actually written"
        );
        assert!(!edited.is_dirty(), "the pending flag goes with the write");
        assert_eq!(
            edited.revision(),
            revision,
            "adopting a save changes no row, so it is not an edit"
        );
    }

    #[test]
    fn an_acknowledged_save_moves_each_row_origin_to_the_written_token() {
        // The loaded file named Harmony nowhere; the save wrote the session's own
        // token for it. After the acknowledgement that spelling is the row's
        // origin, so removing the row and activating the copy again reverses the
        // edit completely instead of leaving a pending difference behind.
        let mut edited = edited();
        edited
            .acknowledge_save(&loaded(&[CORE_TOKEN, HARMONY_TOKEN]))
            .expect("the save must be acknowledged");

        edited.deactivate(1).expect("the added row is removable");
        assert!(edited.is_dirty(), "one row is now missing again");
        edited
            .activate(&ModId::new(HARMONY))
            .expect("and activatable again");

        assert_eq!(
            edited.working_entries(),
            edited.saved_entries(),
            "the row comes back with the spelling the acknowledged save wrote"
        );
        assert!(
            !edited.is_dirty(),
            "so the reversal clears the pending edits against the new baseline"
        );
        assert!(
            !edited.reset(),
            "and there is nothing left to discard back to the first load"
        );
    }

    #[test]
    fn a_clean_session_acknowledges_a_save_of_the_list_it_loaded() {
        let mut clean = session(&[CORE_TOKEN]);

        clean
            .acknowledge_save(&loaded(&[CORE_TOKEN]))
            .expect("saving an unedited list is a save like any other");

        assert!(!clean.is_dirty());
        assert_eq!(clean.revision(), 0, "and it is still not an edit");
        assert_eq!(clean.saved_entries(), clean.working_entries());
    }

    #[test]
    fn an_acknowledgement_for_a_different_list_is_refused_and_changes_nothing() {
        // Two shapes of the same mistake: a file that holds a different number of
        // rows, and a file whose rows are re-spelled. Neither may move the
        // baseline, because the session would then save against bytes it never
        // wrote while claiming the user's edits were safe.
        let mut edited = edited();
        let baseline = edited.loaded_config().clone();
        let revision = edited.revision();

        let wrong_length = edited.acknowledge_save(&loaded(&[CORE_TOKEN]));
        let error = wrong_length.expect_err("a one-row file is not this two-row list");
        assert!(
            error.to_string().contains("not the 2"),
            "the refusal counts what it found: {error}"
        );

        let re_spelled = edited.acknowledge_save(&loaded(&[CORE_TOKEN, "brrainz.HARMONY"]));
        let error = re_spelled.expect_err("a re-spelled row is not the row that was saved");
        assert!(
            error.to_string().contains("not the edited one"),
            "and names the row it could not match: {error}"
        );

        assert_eq!(
            edited.loaded_config(),
            &baseline,
            "a refused acknowledgement installs no bytes"
        );
        assert_eq!(edited.revision(), revision, "and spends no revision");
        assert!(edited.is_dirty(), "and never drops the pending edits");
    }

    #[test]
    fn a_snapshot_that_is_not_the_load_of_its_own_bytes_is_refused() {
        let mut edited = edited();
        let baseline = edited.loaded_config().clone();
        let mut forged = loaded(&[CORE_TOKEN, HARMONY_TOKEN]);
        forged.version = None;

        let error = edited
            .acknowledge_save(&forged)
            .expect_err("a snapshot whose fields its bytes do not produce is not a baseline");
        assert!(
            error.to_string().contains("not the load of its own bytes"),
            "the refusal says what failed: {error}"
        );
        assert_eq!(edited.loaded_config(), &baseline);
        assert!(edited.is_dirty());
    }

    #[test]
    fn bytes_that_do_not_load_as_a_configuration_are_refused() {
        let mut edited = edited();
        let baseline = edited.loaded_config().clone();
        let mut unreadable = loaded(&[CORE_TOKEN, HARMONY_TOKEN]);
        unreadable.raw = b"<?xml version=\"1.0\"?><NotConfig/>".to_vec();

        let error = edited
            .acknowledge_save(&unreadable)
            .expect_err("bytes that are not a ModsConfig cannot be adopted");
        assert!(
            matches!(error, Error::XmlSyntax { .. }),
            "the loader's own reason is what comes back: {error}"
        );
        assert_eq!(edited.loaded_config(), &baseline);
        assert!(
            edited.is_dirty(),
            "and the edits are still there to save again"
        );
    }

    const PROVIDER: &str = "/mods/provider";
    const DEPENDENT: &str = "/mods/dependent";
    const PROVIDER_TOKEN: &str = "provider.mod";
    const DEPENDENT_TOKEN: &str = "dependent.mod";

    /// Game content, a user mod, and a user mod that declares it loads after it, so an
    /// unsorted list has exactly one honest order to publish.
    fn orderable_catalog() -> Catalog {
        let mut dependent = record(DEPENDENT, "dependent.mod", Source::Local);
        dependent.parsed.load_after =
            vec![PackageId::normalize("provider.mod").expect("provider must normalize")];
        Catalog::new(vec![
            record(CORE, "ludeon.rimworld", Source::Official),
            record(PROVIDER, "provider.mod", Source::Local),
            dependent,
        ])
        .expect("the catalog must build")
    }

    /// A session whose loaded list has the dependent copy ahead of its provider.
    fn unsorted_session() -> Session {
        Session::new(
            orderable_catalog(),
            loaded(&[CORE_TOKEN, DEPENDENT_TOKEN, PROVIDER_TOKEN]),
            PathBuf::from("/config/RimWorld/ModsConfig.xml"),
            target(),
        )
    }

    /// The text of each working row, which is row identity and provenance together.
    fn spelled(session: &Session) -> Vec<String> {
        session
            .working_entries()
            .iter()
            .map(|entry| entry.original_token().to_string())
            .collect()
    }

    #[test]
    fn a_published_order_moves_the_list_and_spends_one_revision() {
        let mut session = unsorted_session();
        let order = sort_active(session.catalog(), session.working_entries())
            .expect("this list has one unambiguous order");

        assert_eq!(
            session.apply_sorted(&order),
            Ok(true),
            "an order made from the rows this session holds is published"
        );
        assert_eq!(
            spelled(&session),
            vec![CORE_TOKEN, PROVIDER_TOKEN, DEPENDENT_TOKEN],
            "the working list is that order now, with each row keeping its own text"
        );
        assert_eq!(session.revision(), 1, "reordering is one edit");
        assert!(
            session.is_dirty(),
            "and the loaded order is still what was saved"
        );
    }

    #[test]
    fn a_noncanonical_unchanged_order_is_refused_without_spending_revision() {
        let mut session = unsorted_session();
        let as_loaded = session.working_entries().to_vec();
        assert_eq!(
            spelled(&session),
            [CORE_TOKEN, DEPENDENT_TOKEN, PROVIDER_TOKEN]
        );

        assert_eq!(
            session.apply_sorted(&as_loaded),
            Err(ApplySortError::UnexpectedOrder),
            "even an unchanged forged result must pass exact-order verification"
        );
        assert_eq!(session.revision(), 0, "so it costs no revision");
        assert!(
            !session.is_dirty(),
            "and it does not make a clean session dirty"
        );
    }

    #[test]
    fn publishing_the_current_canonical_order_spends_no_revision() {
        let mut session = unsorted_session();
        // Official Core is first; the metadata edge requires Provider before Dependent.
        let order = [
            (CORE, CORE_TOKEN),
            (PROVIDER, PROVIDER_TOKEN),
            (DEPENDENT, DEPENDENT_TOKEN),
        ]
        .map(|(path, token)| ActiveEntry::Resolved {
            mod_id: ModId::new(path),
            original_token: token.into(),
        });
        assert_eq!(session.apply_sorted(&order), Ok(true));
        let revision = session.revision();
        assert_eq!(session.apply_sorted(&order), Ok(false));
        assert_eq!(session.revision(), revision);
        assert!(session.is_dirty());
    }

    #[test]
    fn an_order_made_for_a_different_list_is_refused_and_changes_nothing() {
        // Two shapes of one mistake, both because the order is not a rearrangement of the
        // rows this session holds: a different number of rows, and a row this session never
        // loaded. Neither may touch the list, the revision or the dirty state.
        let order = sort_active(&orderable_catalog(), unsorted_session().working_entries())
            .expect("this list has one unambiguous order");
        let mut shortened = unsorted_session();
        shortened
            .deactivate(2)
            .expect("the last row of the loaded list is removable");
        let revision = shortened.revision();

        let refused = shortened
            .apply_sorted(&order)
            .expect_err("an order for a longer list cannot be published into a shorter one");
        assert!(
            matches!(
                refused,
                ApplySortError::LengthMismatch {
                    sorted: 3,
                    working: 2
                }
            ),
            "the refusal counts what it found: {refused}"
        );

        let mut foreign_row = order.clone();
        foreign_row[1] = ActiveEntry::Resolved {
            mod_id: ModId::new(HARMONY),
            original_token: HARMONY_TOKEN.to_string(),
        };
        let mut full = unsorted_session();
        let refused = full
            .apply_sorted(&foreign_row)
            .expect_err("nor can an order that names a row this session never held");
        assert!(
            matches!(&refused, ApplySortError::ForeignEntry { description } if
                description.contains(HARMONY) && description.contains(HARMONY_TOKEN)),
            "and it names the row it could not match: {refused}"
        );

        assert_eq!(
            spelled(&full),
            vec![CORE_TOKEN, DEPENDENT_TOKEN, PROVIDER_TOKEN],
            "a refused order reorders nothing"
        );
        assert_eq!(full.revision(), 0, "spends no revision");
        assert!(!full.is_dirty(), "and leaves the dirty state alone");
        assert_eq!(
            shortened.revision(),
            revision,
            "the shortened list still holds exactly the edit it had"
        );
    }

    /// The same physical copy, named twice by one loaded file.
    fn duplicated_copy_session() -> Session {
        Session::new(
            Catalog::new(vec![record(HARMONY, "brrainz.harmony", Source::Local)])
                .expect("the catalog must build"),
            loaded(&["brrainz.harmony", "Brrainz.Harmony"]),
            PathBuf::from("/config/RimWorld/ModsConfig.xml"),
            target(),
        )
    }

    #[test]
    fn an_order_for_repeated_occurrences_of_one_copy_is_refused_and_changes_nothing() {
        // The owner's rule for this gate is the sorter's own eligibility: a list holding one
        // physical copy twice is not orderable, because copy identity alone cannot say which
        // occurrence carries which history. The gate therefore refuses the swap instead of
        // publishing it and writing one row's provenance to both.
        let mut session = duplicated_copy_session();
        let rows = session.working_entries().to_vec();
        assert_eq!(
            rows[0].mod_id(),
            rows[1].mod_id(),
            "both spellings resolve to one copy, which is what the file says"
        );
        let mut swapped = rows.clone();
        swapped.swap(0, 1);
        assert_ne!(
            swapped, rows,
            "and trading the two occurrences is a real change to the list"
        );

        let refused = session
            .apply_sorted(&swapped)
            .expect_err("an order cannot tell two occurrences of one copy apart");
        assert!(
            matches!(refused, ApplySortError::RepeatedCopy { .. }),
            "the refusal names the repeated copy: {refused}"
        );
        assert_eq!(session.working_entries(), &rows[..], "no row moved");
        assert_eq!(session.revision(), 0, "no revision was spent");
        assert!(!session.is_dirty(), "and the list is still what was loaded");
    }

    #[test]
    fn an_order_for_two_active_copies_of_one_package_id_is_refused() {
        // The other half of the sorter's eligibility: two different copies declaring one
        // package id leave no honest order at all, so the gate refuses before the
        // permutation question even arises.
        let mut session = Session::new(
            Catalog::new(vec![
                record(HARMONY, "brrainz.harmony", Source::Local),
                record("/mods/harmony-steam", "brrainz.harmony", Source::Workshop),
            ])
            .expect("the catalog must build"),
            loaded(&["brrainz.harmony", "brrainz.harmony_steam"]),
            PathBuf::from("/config/RimWorld/ModsConfig.xml"),
            target(),
        );
        let rows = session.working_entries().to_vec();
        assert_ne!(
            rows[0].mod_id(),
            rows[1].mod_id(),
            "this fixture is two distinct copies of one package id"
        );
        let mut swapped = rows.clone();
        swapped.swap(0, 1);

        let refused = session
            .apply_sorted(&swapped)
            .expect_err("one package id may not be active as two copies here either");
        assert!(
            matches!(refused, ApplySortError::AmbiguousPackage { .. }),
            "the refusal names both copies and the package id: {refused}"
        );
        assert_eq!(session.working_entries(), &rows[..]);
        assert_eq!(session.revision(), 0);
    }

    #[test]
    fn an_order_holding_a_placeholder_row_is_refused_whatever_its_token_says() {
        // A placeholder is not a row a load order can carry, and its reason is part of what
        // it says: a candidate that keeps the text while changing the reason is not this
        // session's list either, so it is refused rather than adopted.
        let mut session = Session::new(
            catalog(),
            loaded(&[CORE_TOKEN, "Not.Installed"]),
            PathBuf::from("/config/RimWorld/ModsConfig.xml"),
            target(),
        );
        let rows = session.working_entries().to_vec();
        assert!(
            matches!(rows[1], ActiveEntry::Unresolved { .. }),
            "the loaded file holds a placeholder the session preserves"
        );
        let mut changed_reason = rows.clone();
        changed_reason[1] = ActiveEntry::Unresolved {
            original_token: "Not.Installed".to_string(),
            reason: UnresolvedReason::EmptyToken,
        };

        let refused = session
            .apply_sorted(&changed_reason)
            .expect_err("a placeholder row is not orderable, with any reason");
        assert!(
            matches!(refused, ApplySortError::UnresolvedRow { .. }),
            "and the refusal says which row it could not take: {refused}"
        );
        assert_eq!(
            session.working_entries(),
            &rows[..],
            "the placeholder keeps the text and reason it was loaded with"
        );
        assert_eq!(session.revision(), 0, "and nothing else moved");
    }

    #[test]
    fn a_refused_order_leaves_each_rows_own_spelling_behind() {
        // Provenance is the part a refusal must not disturb. This row came in as a `_steam`
        // token, so after a rejected order, removing it and activating the same copy again
        // writes that same spelling rather than one the refusal invented.
        let mut session = Session::new(
            orderable_catalog(),
            loaded(&["provider.mod_steam"]),
            PathBuf::from("/config/RimWorld/ModsConfig.xml"),
            target(),
        );
        let other = sort_active(&orderable_catalog(), session.working_entries())
            .expect("one row sorts to itself");
        let mut foreign = vec![ActiveEntry::Resolved {
            mod_id: ModId::new(CORE),
            original_token: CORE_TOKEN.to_string(),
        }];
        foreign.extend(other);

        assert!(
            session.apply_sorted(&foreign).is_err(),
            "an order for rows this session does not hold is refused"
        );
        session
            .deactivate(0)
            .expect("the loaded row is removable on its own");
        session
            .activate(&ModId::new(PROVIDER))
            .expect("and activatable again");

        assert_eq!(
            spelled(&session),
            vec!["provider.mod_steam"],
            "the reversal comes back with the text this row came in with"
        );
        assert!(
            !session.is_dirty(),
            "so nothing was lost while it was refused"
        );
    }
}
