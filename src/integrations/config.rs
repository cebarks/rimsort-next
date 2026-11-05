//! Reading and failure-safe writing of a `ModsConfig.xml` active mod list.
//!
//! A config file is small, opaque user data: we keep the bytes exactly as they
//! were read, record which bytes the `<activeMods>` element occupies, and report
//! the tokens in file order. A save replaces precisely those bytes with the
//! entries it was handed and copies every other byte unchanged, so nothing here
//! re-serializes the document through an object model. Nothing here resolves a
//! token against an installed mod, sorts the list, drops an entry or invents a
//! spelling; the session owns edits and the catalog owns resolution.
//!
//! XML validity is the shared strict reader's job ([`crate::integrations::xml`]):
//! a document type declaration, an entity reference outside the predefined set,
//! a malformed attribute or stray content outside the root is an error, never a
//! silently skipped construct. Locating the active list needs byte positions and
//! untrimmed entry text, which the element tree does not carry, so the same
//! bytes are read a second time as events. These files are tiny, and two linear
//! passes beat a lossy whole-document model.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use quick_xml::escape::EscapeError;
use quick_xml::escape::partial_escape;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use tempfile::NamedTempFile;

use crate::domain::catalog::{ActiveEntry, Catalog};
use crate::domain::types::Error;
use crate::integrations::xml;

/// The element a `ModsConfig.xml` is built around.
const ROOT_NAME: &str = "ModsConfigData";
/// The element whose bytes a save replaces.
const ACTIVE_MODS_NAME: &str = "activeMods";
/// The only element allowed inside the active list.
const ENTRY_NAME: &str = "li";
/// The element naming the game release that wrote the file.
const VERSION_NAME: &str = "version";
/// A UTF-8 byte order mark, which the reader consumes without counting it.
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Something true of a loaded active list that the user should see.
///
/// A diagnostic never drops an entry: the token keeps its position in
/// [`ConfigSnapshot::active_tokens`], and deciding what two identity-equivalent
/// copies mean is the catalog's later business.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigDiagnostic {
    /// The same decoded token text appeared before, at `first_index`. Every
    /// later copy reports that original first occurrence, blanks included.
    RepeatedToken {
        token: String,
        first_index: usize,
        index: usize,
    },
    /// The entry at `index` carries no non-whitespace text, so it can resolve
    /// to nothing. Emptiness is reported for the entry itself; an exact repeat
    /// of it is still reported as `RepeatedToken` as well.
    EmptyToken { index: usize },
}

/// One loaded `ModsConfig.xml`, kept as the bytes it was read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigSnapshot {
    /// The file exactly as loaded, declaration, BOM, comments and all.
    pub raw: Vec<u8>,
    /// Byte range of the whole `<activeMods>` element, tags included.
    ///
    /// A save replaces precisely these bytes and copies everything outside the
    /// range unchanged, so positions are byte offsets into `raw`, not character
    /// counts.
    pub active_mods_span: Range<usize>,
    /// `<li>` text in document order, entity-decoded but otherwise untouched:
    /// case, surrounding whitespace and every `_steam` spelling survive
    /// verbatim.
    pub active_tokens: Vec<String>,
    /// `<version>` text as written, padding included, when the file carries a
    /// root-level version with something in it. Never parsed into a release and
    /// never invented when absent; the first root-level version is the config's
    /// version even when it is blank.
    pub version: Option<String>,
    /// Diagnostics about the active list, in entry order.
    pub diagnostics: Vec<ConfigDiagnostic>,
}

/// Reads `ModsConfig.xml` bytes into a snapshot.
///
/// The bytes are the whole input: no file is opened and nothing is written. A
/// document that is not a `ModsConfigData` config, that lacks `<activeMods>`,
/// that declares it more than once, or that hides markup where a token belongs,
/// is an error rather than a partially loaded list.
pub fn parse_config(bytes: &[u8]) -> Result<ConfigSnapshot, Error> {
    let root = xml::parse_document(bytes)?;
    if !root.has_name(ROOT_NAME) {
        return Err(Error::XmlSyntax {
            element: root.name().to_string(),
            message: format!(
                "expected a {ROOT_NAME} root element, found {:?}",
                root.name()
            ),
        });
    }

    let containers = root.children_named(ACTIVE_MODS_NAME).count();
    if containers != 1 {
        let message = if containers == 0 {
            format!(
                "{ROOT_NAME} has no {ACTIVE_MODS_NAME} element, so no active list is configured"
            )
        } else {
            format!(
                "{ROOT_NAME} declares {containers} {ACTIVE_MODS_NAME} elements; exactly one is expected"
            )
        };
        return Err(Error::XmlSyntax {
            element: root.name().to_string(),
            message,
        });
    }

    let ConfigFields {
        active_mods_span,
        tokens,
        version,
    } = read_config_fields(bytes)?;
    let diagnostics = token_diagnostics(&tokens);

    Ok(ConfigSnapshot {
        raw: bytes.to_vec(),
        active_mods_span,
        active_tokens: tokens,
        // A version with only spacing in it is not a display string.
        version: version.filter(|text| !text.trim().is_empty()),
        diagnostics,
    })
}

/// Flags repeated and blank entries while keeping every position.
///
/// One pass over the entries in file order, so the diagnostics come out in that
/// same order; `first_index` is a scratch table of at most one entry per
/// distinct token text, which keeps the lookup linear instead of comparing each
/// entry against everything before it.
fn token_diagnostics(tokens: &[String]) -> Vec<ConfigDiagnostic> {
    let mut first_index: HashMap<&str, usize> = HashMap::new();
    let mut diagnostics = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.trim().is_empty() {
            diagnostics.push(ConfigDiagnostic::EmptyToken { index });
        }
        match first_index.get(token.as_str()) {
            Some(&seen_at) => diagnostics.push(ConfigDiagnostic::RepeatedToken {
                token: token.clone(),
                first_index: seen_at,
                index,
            }),
            None => {
                first_index.insert(token.as_str(), index);
            }
        }
    }
    diagnostics
}

/// The located config fields: the active list's bytes, the text of its entries,
/// and the file's own version string.
struct ConfigFields {
    active_mods_span: Range<usize>,
    tokens: Vec<String>,
    version: Option<String>,
}

/// Where the byte scan stands relative to the active list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Position {
    /// Outside the list, before it or inside unrelated fields.
    Elsewhere,
    /// Between `<activeMods>` and `</activeMods>`, not inside an entry.
    InList,
    /// Inside a `<li>` entry, collecting its text.
    InEntry,
}

/// The two texts the byte scan collects: the entry being read and the root's own
/// `<version>` text.
#[derive(Default)]
struct Texts {
    entry: String,
    version: String,
}

/// Reads the active list's byte span, its entry texts and the root's own
/// `<version>` text out of an already validated document.
///
/// Element names are matched the way the shared reader matches them, ignoring
/// ASCII case, so a second `<activemods>` is a duplicate list rather than an
/// unknown field. Comments and processing instructions carry no text and are
/// skipped wherever they appear. The scan runs to the end of the document: a
/// version element written after the list is still this config's version, as
/// long as no earlier root-level one claimed it.
fn read_config_fields(bytes: &[u8]) -> Result<ConfigFields, Error> {
    // The reader consumes a leading byte order mark without counting it in its
    // positions, so every offset it reports needs the mark's length added back.
    let bom = usize::from(bytes.starts_with(UTF8_BOM)) * UTF8_BOM.len();
    let mut reader = Reader::from_reader(bytes);
    // Same strictness as the shared reader: a comment still has to be legal XML.
    reader.config_mut().check_comments = true;

    // How many elements are open while the event is read: the root is one, so
    // a Start seen at one open element is a direct child of the root, and that
    // is the only place the active list belongs.
    let mut open = 0usize;
    let mut position = Position::Elsewhere;
    let mut list_start = 0;
    let mut texts = Texts::default();
    let mut tokens: Vec<String> = Vec::new();
    let mut span: Option<Range<usize>> = None;
    // The first root-level `<version>` owns the version field, blank or not, and
    // `version_open` is the depth whose character data belongs to it.
    let mut version: Option<String> = None;
    let mut version_taken = false;
    let mut version_open = 0;

    loop {
        // The reader's position is the byte just after the previous event, and
        // every byte of the document belongs to some event, so this is also the
        // offset where the event read next begins.
        let start = bom + reader.buffer_position() as usize;
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(source) => return Err(list_error(position, source.to_string())),
        };
        let end = bom + reader.buffer_position() as usize;
        let self_closing = matches!(event, Event::Empty(_));

        match event {
            Event::Eof => break,
            Event::Start(tag) | Event::Empty(tag) => {
                let name = declared_name(&tag, position)?;
                match position {
                    Position::InEntry => {
                        return Err(nested_markup(position));
                    }
                    Position::InList => {
                        if !name_is(&name, ENTRY_NAME) {
                            return Err(unsupported_child(position, &name));
                        }
                        if self_closing {
                            tokens.push(String::new());
                        } else {
                            texts.entry.clear();
                            position = Position::InEntry;
                            open += 1;
                        }
                    }
                    Position::Elsewhere => {
                        let is_the_list =
                            open == 1 && span.is_none() && name_is(&name, ACTIVE_MODS_NAME);
                        if !is_the_list {
                            // An unrelated field: remember its depth, ignore its
                            // content, and let the raw bytes carry it forward.
                            if open == 1 && !version_taken && name_is(&name, VERSION_NAME) {
                                version_taken = true;
                                if self_closing {
                                    version = Some(String::new());
                                } else {
                                    texts.version.clear();
                                    version_open = open + 1;
                                }
                            }
                            if !self_closing {
                                open += 1;
                            }
                        } else if self_closing {
                            span = Some(start..end);
                        } else {
                            list_start = start;
                            position = Position::InList;
                            open += 1;
                        }
                    }
                }
            }
            Event::End(_) => {
                match position {
                    Position::InEntry => {
                        tokens.push(std::mem::take(&mut texts.entry));
                        position = Position::InList;
                    }
                    Position::InList => {
                        span = Some(list_start..end);
                        position = Position::Elsewhere;
                    }
                    Position::Elsewhere => {}
                }
                if version_taken && version.is_none() && open == version_open {
                    version = Some(std::mem::take(&mut texts.version));
                    version_open = 0;
                }
                open = open.saturating_sub(1);
            }
            Event::Text(text) => {
                let decoded = text.decode().map_err(|source| {
                    list_error(position, format!("text is not valid UTF-8: {source}"))
                })?;
                collect_text(&mut texts, position, open, version_open, &decoded)?;
            }
            Event::CData(data) => {
                // Character data inside CDATA is literal: entities stay as they
                // are, exactly as the shared reader treats them.
                let decoded = data.decode().map_err(|source| {
                    list_error(position, format!("CDATA is not valid UTF-8: {source}"))
                })?;
                collect_text(&mut texts, position, open, version_open, &decoded)?;
            }
            Event::GeneralRef(reference) => {
                let name = reference.decode().map_err(|source| {
                    list_error(
                        position,
                        format!("entity reference is not valid UTF-8: {source}"),
                    )
                })?;
                let expanded = expand_reference(&name, position)?;
                collect_text(&mut texts, position, open, version_open, &expanded)?;
            }
            // Comments, processing instructions and declarations carry no entry
            // text, and the shared reader has already refused any document type
            // and validated what it accepted.
            Event::Comment(_) | Event::PI(_) | Event::Decl(_) | Event::DocType(_) => {}
        }
    }

    let active_mods_span = span.ok_or_else(|| {
        list_error(
            position,
            format!("{ACTIVE_MODS_NAME} is not a closed element, so its bytes cannot be located"),
        )
    })?;

    Ok(ConfigFields {
        active_mods_span,
        tokens,
        version,
    })
}

/// Adds one run of character data to the entry or the version element being
/// read, or checks that text between entries is only formatting.
///
/// Outside the list, only the version element's own depth is collected; text in
/// a field nested deeper belongs to that unknown field and stays in the raw
/// bytes instead.
fn collect_text(
    texts: &mut Texts,
    position: Position,
    open: usize,
    version_open: usize,
    text: &str,
) -> Result<(), Error> {
    match position {
        Position::InEntry => {
            texts.entry.push_str(text);
            Ok(())
        }
        Position::InList if is_formatting(text) => Ok(()),
        Position::InList => Err(list_error(
            position,
            format!("{ACTIVE_MODS_NAME} holds {ENTRY_NAME} entries, not the text {text:?}"),
        )),
        Position::Elsewhere if version_open == open && open > 0 => {
            texts.version.push_str(text);
            Ok(())
        }
        Position::Elsewhere => Ok(()),
    }
}

/// True for text made only of the whitespace XML allows between markup.
fn is_formatting(text: &str) -> bool {
    text.chars()
        .all(|ch| matches!(ch, ' ' | '\t' | '\r' | '\n'))
}

fn unsupported_child(position: Position, name: &str) -> Error {
    list_error(
        position,
        format!("only a {ENTRY_NAME} entry may appear inside {ACTIVE_MODS_NAME}, found {name}"),
    )
}

fn nested_markup(position: Position) -> Error {
    list_error(
        position,
        format!("a {ENTRY_NAME} entry holds text, not nested elements"),
    )
}

/// The declared name of an opening or self-closing element.
fn declared_name(tag: &BytesStart<'_>, position: Position) -> Result<String, Error> {
    std::str::from_utf8(tag.name().as_ref())
        .map(str::to_string)
        .map_err(|source| list_error(position, source.to_string()))
}

/// True when `name` is the wanted element, ignoring ASCII case.
fn name_is(name: &str, wanted: &str) -> bool {
    name.eq_ignore_ascii_case(wanted)
}

/// Where a byte-scan problem was found, as an element path.
fn list_context(position: Position) -> String {
    match position {
        Position::Elsewhere => ROOT_NAME.to_string(),
        Position::InList => format!("{ROOT_NAME}/{ACTIVE_MODS_NAME}"),
        Position::InEntry => format!("{ROOT_NAME}/{ACTIVE_MODS_NAME}/{ENTRY_NAME}"),
    }
}

fn list_error(position: Position, message: impl Into<String>) -> Error {
    Error::XmlSyntax {
        element: list_context(position),
        message: message.into(),
    }
}

/// Expands one reference between `&` and `;`, following the shared reader:
/// character references and the predefined XML entities resolve, and anything
/// that would need the refused DTD is reported instead of substituted.
fn expand_reference(name: &str, position: Position) -> Result<String, Error> {
    match quick_xml::escape::unescape(&format!("&{name};")) {
        Ok(Cow::Borrowed(text)) => Ok(text.to_string()),
        Ok(Cow::Owned(text)) => Ok(text),
        Err(EscapeError::UnrecognizedEntity(_, entity)) => Err(Error::UnsupportedEntity {
            name: entity,
            element: list_context(position),
        }),
        Err(source) => Err(list_error(position, source.to_string())),
    }
}

/// Suffix appended to the complete target file name to name its backup.
const BACKUP_SUFFIX: &str = ".rimsort-rust.bak";

/// The one step of indentation a new entry gets when the loaded list holds no
/// entry whose own surrounding whitespace can be copied.
const DEFAULT_ENTRY_INDENT: &[u8] = b"  ";

/// A validated document ready to be written, bound to the load it was made from.
///
/// Both halves are private on purpose: [`prepare_save`] is the only way to build
/// one and [`commit_save`] is the only way to write it, and it refuses any
/// snapshot other than the one these bytes were prepared against. Prepared bytes
/// therefore cannot be handed to a different configuration, and the type cannot
/// be assembled by hand to skip validation.
#[derive(Clone, Debug)]
pub struct PreparedSave {
    /// The whole file as it reads once the commit has succeeded.
    committed: ConfigSnapshot,
    /// The loaded snapshot these bytes replace.
    baseline: ConfigSnapshot,
}

impl PreparedSave {
    /// The exact document these bytes write.
    ///
    /// Crate-private and read-only: a caller that has to recognize the result of
    /// its own write - the workflow, before it adopts a completion as a session
    /// baseline - compares against this instead of settling for a document that
    /// merely reads alike. Nothing here can be built or changed from outside, so
    /// the view cannot become a way to forge an expectation.
    pub(crate) fn expected_committed(&self) -> &ConfigSnapshot {
        &self.committed
    }
}

/// Builds the bytes a save would write, validating everything before any disk access.
///
/// Every entry is written as its own [`ActiveEntry::original_token`], escaped for
/// XML text and changed in no other way: nothing is sorted, resolved, dropped,
/// renamed, deduplicated or substituted here, so an inherited token that names no
/// installed copy, a repeated token and a token that holds nothing but spacing
/// each survive a save exactly as they survived the load. A resolved entry is
/// refused when its own token does not resolve back to the copy it is stored
/// against, because writing it would silently activate a different physical copy.
///
/// Only the bytes of the `<activeMods>` element are replaced. Everything else in
/// the loaded file — declaration, byte order mark, CRLF, `version`,
/// `knownExpansions`, comments, unknown fields, non-ASCII text — is copied
/// through untouched, and the entries are written with the element's own start
/// tag, entry spelling, surrounding whitespace and closing tag carried over from
/// the load, the spacing a legal end tag may hold before its bracket included.
pub fn prepare_save(
    snapshot: &ConfigSnapshot,
    active: &[ActiveEntry],
    catalog: &Catalog,
) -> Result<PreparedSave, Error> {
    let baseline = loaded_again(snapshot)?;
    let regions = split_span(&baseline.raw, &baseline.active_mods_span)?;
    let layout = ListLayout::locate(
        regions.element,
        regions.before,
        !baseline.active_tokens.is_empty(),
    )?;
    let tokens = serialized_entries(active, catalog)?;

    let mut bytes = Vec::with_capacity(baseline.raw.len());
    bytes.extend_from_slice(regions.before);
    let list_start = bytes.len();
    layout.write(&tokens, &mut bytes);
    bytes.extend_from_slice(regions.after);

    let committed = validated_output(bytes, &tokens, list_start)?;
    Ok(PreparedSave {
        committed,
        baseline,
    })
}

/// Loads the snapshot's own bytes and insists they say exactly what it says.
///
/// Every field of [`ConfigSnapshot`] is public, so a caller could hand over
/// offsets or tokens its bytes do not produce. Re-loading is cheap for a file
/// this small, and it is what lets the rest of this module trust an offset at all.
fn loaded_again(snapshot: &ConfigSnapshot) -> Result<ConfigSnapshot, Error> {
    let reloaded = parse_config(&snapshot.raw)?;
    if reloaded != *snapshot {
        return Err(prepare_error(
            "the snapshot is not the load of its own bytes, so its active-list span \
             and tokens cannot be trusted"
                .to_string(),
        ));
    }
    Ok(reloaded)
}

/// The three regions of a loaded file that a save is assembled from: whatever
/// comes before the active list, the list's own bytes and whatever follows it.
struct SpanRegions<'bytes> {
    before: &'bytes [u8],
    element: &'bytes [u8],
    after: &'bytes [u8],
}

/// The bytes before, inside and after a caller-supplied span, or an explicit
/// refusal. Each index is checked, so no range can panic a save.
fn split_span<'bytes>(
    raw: &'bytes [u8],
    span: &Range<usize>,
) -> Result<SpanRegions<'bytes>, Error> {
    let length = raw.len();
    let out_of_range = || {
        prepare_error(format!(
            "the active-list span {}..{} is not a byte range of {length} bytes",
            span.start, span.end
        ))
    };
    let before = raw.get(..span.start).ok_or_else(out_of_range)?;
    let element = raw.get(span.start..span.end).ok_or_else(out_of_range)?;
    let after = raw.get(span.end..).ok_or_else(out_of_range)?;
    Ok(SpanRegions {
        before,
        element,
        after,
    })
}

/// The token text each entry will be written with, checked against the catalog.
///
/// A resolved entry is accepted only while its own token still resolves back to
/// the copy it is stored against, because a token names a package id and never a
/// directory: writing a mismatch would silently activate a different copy.
/// An unresolved entry is not checked at all, since preserving the text the file
/// carried is the whole point of it.
fn serialized_entries<'entries>(
    active: &'entries [ActiveEntry],
    catalog: &Catalog,
) -> Result<Vec<&'entries str>, Error> {
    let mut tokens = Vec::with_capacity(active.len());
    for entry in active {
        let token = entry.original_token();
        if let ActiveEntry::Resolved { mod_id, .. } = entry {
            let named = catalog.resolve_token(token);
            if named.mod_id() != Some(mod_id) {
                let found = named
                    .mod_id()
                    .map_or_else(|| "no installed copy".to_string(), ToString::to_string);
                return Err(prepare_error(format!(
                    "the token {token:?} resolves to {found}, not to the copy at {mod_id} \
                     it is stored against, so saving it would silently activate another copy"
                )));
            }
        }
        if let Some(forbidden) = token.chars().find(|ch| !is_xml_text_char(*ch)) {
            return Err(prepare_error(format!(
                "the token {token:?} holds U+{:04X}, which XML element text cannot hold, \
                 so the saved document would not be readable",
                u32::from(forbidden)
            )));
        }
        tokens.push(token);
    }
    Ok(tokens)
}

/// True for the characters XML allows inside element text.
fn is_xml_text_char(ch: char) -> bool {
    matches!(ch, '\t' | '\n' | '\r')
        || ('\u{20}'..='\u{d7ff}').contains(&ch)
        || ('\u{e000}'..='\u{fffd}').contains(&ch)
        || ch >= '\u{10000}'
}

/// Re-reads the prepared document, before a single byte of it is written.
///
/// The strict loader is the same one a later session will use, so a document it
/// refuses, or one that would read back as different entries than the caller
/// asked for, is a preparation failure rather than a corrupted file.
fn validated_output(
    bytes: Vec<u8>,
    expected: &[&str],
    list_start: usize,
) -> Result<ConfigSnapshot, Error> {
    let parsed = parse_config(&bytes)?;
    if parsed.active_mods_span.start != list_start {
        return Err(prepare_error(
            "the prepared list does not begin where the loaded one did".to_string(),
        ));
    }
    if parsed.active_tokens.len() != expected.len() {
        return Err(prepare_error(format!(
            "the prepared document reads as {} entries, not the {} that were given",
            parsed.active_tokens.len(),
            expected.len()
        )));
    }
    for (index, (wanted, found)) in expected.iter().zip(&parsed.active_tokens).enumerate() {
        if wanted != found {
            return Err(prepare_error(format!(
                "entry {index} would read back as {found:?} instead of {wanted:?}"
            )));
        }
    }
    Ok(parsed)
}

/// The bytes a prepared list is written between.
///
/// Every part of it comes from the loaded element, so a save changes the entries
/// and nothing else: the start tag with its attributes, the entry spelling the
/// file used, the whitespace the file used around its entries and the end tag.
struct ListLayout {
    /// The loaded start tag, without a self-closing slash.
    open: Vec<u8>,
    /// The bytes written before each entry.
    entry: Vec<u8>,
    /// The entry element's name, as the loaded file spells it.
    entry_name: Vec<u8>,
    /// The bytes written after the last entry.
    tail: Vec<u8>,
    /// The loaded end tag, or the pair written for a self-closing element.
    close: Vec<u8>,
}

impl ListLayout {
    /// Reads the layout out of the loaded element's own bytes.
    ///
    /// A list that held entries copies their surrounding whitespace verbatim. A
    /// list that held none - empty or self-closing - has no entry indentation to
    /// copy, so entries are written one step deeper than the line the list
    /// starts on, and a file that never breaks a line stays on one line.
    fn locate(element: &[u8], before: &[u8], had_entries: bool) -> Result<Self, Error> {
        let (name, self_closing, open_end) = start_tag(element)?;
        let (open, inner, close, first_entry) = if self_closing {
            // Dropping the self-closing slash leaves the loaded start tag, with
            // its attributes, as the opening half of a paired element. Such a list
            // held no entry, so it has no closing bytes of its own to keep.
            let mut open = element[..open_end - 2].to_vec();
            open.push(b'>');
            (open, &[][..], end_tag(&name), None)
        } else {
            let events = list_events(element).ok_or_else(mismatched_close_error)?;
            if events.closing.end != element.len() || events.closing.start < open_end {
                return Err(mismatched_close_error());
            }
            let close = &element[events.closing.start..];
            if !closing_names(close, &name) {
                return Err(mismatched_close_error());
            }
            (
                element[..open_end].to_vec(),
                &element[open_end..events.closing.start],
                close.to_vec(),
                events.entry_name,
            )
        };

        let (entry, entry_name, tail) = if had_entries {
            let entry = leading_whitespace(inner);
            (
                entry.to_vec(),
                first_entry.unwrap_or_else(|| ENTRY_NAME.as_bytes().to_vec()),
                trailing_whitespace(&inner[entry.len()..]).to_vec(),
            )
        } else {
            let template = line_template(inner, before);
            let entry = if template.is_empty() {
                template.clone()
            } else {
                [template.as_slice(), DEFAULT_ENTRY_INDENT].concat()
            };
            (entry, ENTRY_NAME.as_bytes().to_vec(), template)
        };

        Ok(Self {
            open,
            entry,
            entry_name,
            tail,
            close,
        })
    }

    /// Writes the whole replacement element into `bytes`.
    fn write(&self, tokens: &[&str], bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(&self.open);
        for token in tokens {
            bytes.extend_from_slice(&self.entry);
            bytes.push(b'<');
            bytes.extend_from_slice(&self.entry_name);
            bytes.push(b'>');
            bytes.extend_from_slice(partial_escape(*token).as_bytes());
            bytes.extend_from_slice(&end_tag(&self.entry_name));
        }
        bytes.extend_from_slice(&self.tail);
        bytes.extend_from_slice(&self.close);
    }
}

/// The end tag that closes an element named `name`.
fn end_tag(name: &[u8]) -> Vec<u8> {
    let mut tag = Vec::with_capacity(name.len() + 3);
    tag.push(b'<');
    tag.push(b'/');
    tag.extend_from_slice(name);
    tag.push(b'>');
    tag
}

/// True when `closing` is the end tag of an element named `name`.
///
/// XML permits whitespace between an end tag's name and its `>`, so the bytes are
/// compared up to that spacing rather than against a canonical `</name>`.
fn closing_names(closing: &[u8], name: &[u8]) -> bool {
    let Some(inner) = closing.strip_prefix(b"</") else {
        return false;
    };
    let declared = inner.strip_suffix(b">").unwrap_or(inner);
    let name_end = declared
        .iter()
        .position(|byte| is_formatting_byte(*byte))
        .unwrap_or(declared.len());
    declared[..name_end] == name[..]
}

fn mismatched_close_error() -> Error {
    prepare_error(
        "the loaded active list is not closed by an end tag naming its own start tag".to_string(),
    )
}

/// The declared name of the element, whether it closes itself, and the byte just
/// after its start tag.
fn start_tag(element: &[u8]) -> Result<(Vec<u8>, bool, usize), Error> {
    let Some((b'<', rest)) = element.split_first() else {
        return Err(prepare_error(
            "the loaded active-list span does not start at an element".to_string(),
        ));
    };
    let mut quote: Option<u8> = None;
    let mut tag_end = None;
    for (offset, byte) in rest.iter().enumerate() {
        match quote {
            Some(mark) if *byte == mark => quote = None,
            Some(_) => {}
            None if matches!(byte, b'"' | b'\'') => quote = Some(*byte),
            None if *byte == b'>' => {
                // `offset` is counted from after the '<', and the tag ends after
                // the '>' itself.
                tag_end = Some(offset + 2);
                break;
            }
            None => {}
        }
    }
    let tag_end = tag_end.ok_or_else(|| {
        prepare_error("the loaded active list never closes its start tag".to_string())
    })?;
    let declared = &element[1..tag_end - 1];
    let name_end = declared
        .iter()
        .position(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b'/'))
        .unwrap_or(declared.len());
    let name = declared[..name_end].to_vec();
    if !String::from_utf8_lossy(&name).eq_ignore_ascii_case(ACTIVE_MODS_NAME) {
        return Err(prepare_error(format!(
            "the loaded span names {:?}, not the active list",
            String::from_utf8_lossy(&name)
        )));
    }
    Ok((name, element.get(tag_end - 2) == Some(&b'/'), tag_end))
}

/// The entry name and the closing bytes of a loaded active-list element, taken
/// from whole XML events.
///
/// A comment or a processing instruction is a single event to the reader, so text
/// that merely looks like markup inside one is never mistaken for the list's first
/// entry, and the element's own closing tag - spacing or line break between its
/// name and its `>` included - is kept instead of being reconstructed.
struct ListEvents {
    /// The declared name of the first entry element, when the list holds one.
    entry_name: Option<Vec<u8>>,
    /// The byte range of the element's own closing event inside its bytes.
    closing: Range<usize>,
}

/// Reads the active-list element's entry and closing events.
///
/// `None` reports that these bytes do not hold one closed element with the name
/// this module asked for, which is a preparation error rather than something to
/// write over.
fn list_events(element: &[u8]) -> Option<ListEvents> {
    let mut reader = Reader::from_reader(element);
    reader.config_mut().check_comments = true;
    // The element itself counts as one open tag, so an entry is a Start or Empty
    // event seen while exactly one tag is open.
    let mut open = 0usize;
    let mut entry_name: Option<Vec<u8>> = None;
    let mut closing: Option<Range<usize>> = None;

    loop {
        let start = reader.buffer_position() as usize;
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(_) => return None,
        };
        let end = reader.buffer_position() as usize;
        match event {
            Event::Eof => break,
            Event::Start(tag) => {
                if open == 1 && entry_name.is_none() {
                    entry_name = Some(tag.local_name().as_ref().to_vec());
                }
                open += 1;
            }
            Event::Empty(tag) => {
                if open == 1 && entry_name.is_none() {
                    entry_name = Some(tag.local_name().as_ref().to_vec());
                }
            }
            Event::End(_) => {
                open = open.checked_sub(1)?;
                if open == 0 {
                    closing = Some(start..end);
                }
            }
            // Comments, processing instructions, character data, references and the
            // declaration are whole events whose bytes name no element at all.
            _ => {}
        }
    }

    let closing = closing?;
    Some(ListEvents {
        entry_name,
        closing,
    })
}

/// The whitespace XML allows between markup, as a byte.
fn is_formatting_byte(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

/// The run of formatting whitespace at the start of `bytes`, which may be empty.
fn leading_whitespace(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|byte| !is_formatting_byte(*byte))
        .unwrap_or(bytes.len());
    &bytes[..end]
}

/// The run of formatting whitespace at the end of `bytes`, which may be empty.
fn trailing_whitespace(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .rposition(|byte| !is_formatting_byte(*byte))
        .map_or(0, |index| index + 1);
    &bytes[start..]
}

/// The line break and indentation for a list with no entry of its own to copy:
/// the loaded element's whitespace if it breaks a line, else the bytes that
/// introduced the element, else nothing at all.
fn line_template(inner: &[u8], before: &[u8]) -> Vec<u8> {
    [inner, before]
        .into_iter()
        .find_map(last_line_break_run)
        .unwrap_or_default()
}

/// The whitespace from the last line break of `bytes` to their end, when the
/// whitespace ending them holds a line break. The carriage return in front of that
/// line feed belongs to the break, so a file written with CRLF is carried forward
/// with its carriage returns rather than half of its line terminator.
fn last_line_break_run(bytes: &[u8]) -> Option<Vec<u8>> {
    let run = trailing_whitespace(bytes);
    let line_feed = run.iter().rposition(|byte| *byte == b'\n')?;
    let carriage_return = usize::from(line_feed > 0 && run[line_feed - 1] == b'\r');
    Some(run[line_feed - carriage_return..].to_vec())
}

/// Writes `prepared` to `path`, replacing the file only while it still holds the
/// bytes `snapshot` was loaded from.
///
/// The destination has to be an existing regular file: a symbolic link or any
/// other kind of name is refused with the path it came from, because this is the
/// game's configuration, not a file for this build to initialize. Loaded bytes
/// that no longer match the file are [`Error::ConfigConflict`], which asks for a
/// refresh or a cancel and never overwrites what someone else wrote. The same
/// comparison is made again immediately before the rename, so the window a second
/// writer can slip through is the check-to-rename gap and nothing larger; there
/// is no lock here, because the game takes no notice of one, and no claim of
/// power-loss safety, because the staging file's directory is not synced.
///
/// The loaded bytes are staged as the next backup first: a temporary file in the
/// destination directory is written, flushed and synced, and only then renamed
/// over the backup name. Any failure before that rename - including a backup that
/// cannot be written at all - leaves both the target and the backup that was
/// already there exactly as they were, and a link standing at the backup name is
/// replaced as a name rather than written through. A failure after the backup is
/// replaced leaves the target holding the loaded bytes and the backup holding
/// those same bytes, so nothing written is lost either way. The new bytes go to a
/// temporary file in the destination directory, which is written, synced and
/// renamed over the target; the target is never deleted first, so any failure
/// before that rename leaves its bytes exactly as they were. A successful save
/// returns the snapshot of the bytes it wrote, without reading them back.
pub fn commit_save(
    path: &Path,
    snapshot: &ConfigSnapshot,
    prepared: &PreparedSave,
) -> Result<ConfigSnapshot, Error> {
    if prepared.baseline != *snapshot {
        return Err(Error::InvalidInput {
            context: format!("commit_save {}", path.display()),
            message: "these bytes were prepared against a different loaded configuration"
                .to_string(),
        });
    }
    commit_with(&FileSystem, path, snapshot, prepared)
}

/// The filesystem operations a commit performs, one method per boundary.
///
/// Keeping them behind one trait is what lets the tests in this module fail a
/// single boundary deterministically instead of arranging permissions that behave
/// differently for a privileged account. Each method performs the real operation
/// it names; an injected fault stops the commit at that boundary and never
/// pretends a save succeeded.
trait CommitOperations {
    /// The destination's own bytes, refusing a symbolic link and anything that is
    /// not a regular file before the name is opened.
    fn read_target(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// Creates the staging file that will become the next backup, in the same
    /// directory as the target. The name the backup holds is not touched here, so
    /// a failure at any later boundary leaves the previous backup standing.
    fn create_backup_staging(&self, directory: &Path) -> io::Result<NamedTempFile>;

    /// Writes the loaded bytes into the backup staging file.
    fn write_backup_staging(&self, staged: &mut NamedTempFile, bytes: &[u8]) -> io::Result<()>;

    /// Flushes and syncs the backup staging file, so its bytes are complete before
    /// the backup name is replaced.
    fn sync_backup_staging(&self, staged: &mut NamedTempFile) -> io::Result<()>;

    /// Renames the completed staging file over `backup`. The previous backup is
    /// replaced as a name, never written into, and a symbolic link standing there
    /// is swapped rather than followed.
    fn replace_backup(&self, staged: NamedTempFile, backup: &Path) -> io::Result<()>;

    /// Creates the staging file inside the destination directory.
    fn create_staging(&self, directory: &Path) -> io::Result<NamedTempFile>;

    /// Writes the prepared bytes to the staging file and carries the target's
    /// permissions over where the filesystem allows it.
    fn write_staging(
        &self,
        temporary: &mut NamedTempFile,
        bytes: &[u8],
        target: &Path,
    ) -> io::Result<()>;

    /// Flushes what is buffered and asks the filesystem to hold it.
    fn sync_staging(&self, temporary: &mut NamedTempFile) -> io::Result<()>;

    /// Renames the staged file over `target`, never deleting the target first.
    fn replace_target(&self, temporary: NamedTempFile, target: &Path) -> io::Result<()>;
}

/// The real filesystem behind [`CommitOperations`].
struct FileSystem;

impl CommitOperations for FileSystem {
    fn read_target(&self, path: &Path) -> io::Result<Vec<u8>> {
        let before_open = fs::symlink_metadata(path)?;
        if before_open.is_symlink() {
            return Err(not_a_regular_file(path, "it is a symbolic link"));
        }
        // A fifo, a socket or a device is refused from this look rather than
        // opened: opening a fifo for reading waits for a writer that may never
        // come, and none of these is the game's configuration file.
        if !before_open.is_file() {
            return Err(not_a_regular_file(path, "it is not a regular file"));
        }
        let mut file = File::open(path)?;
        let opened = file.metadata()?;
        // The name can be swapped between the look above and this open, so the
        // object actually opened is checked as well. What happens after both
        // checks is the pathname race described on commit_save, not something an
        // open call closes.
        if !opened.is_file() {
            return Err(not_a_regular_file(path, "it is not a regular file"));
        }
        // The bytes are read from the handle that was just checked, not from a
        // second lookup of the same name.
        let mut bytes = Vec::with_capacity(usize::try_from(opened.len()).unwrap_or(0));
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn create_backup_staging(&self, directory: &Path) -> io::Result<NamedTempFile> {
        NamedTempFile::new_in(directory)
    }

    fn write_backup_staging(&self, staged: &mut NamedTempFile, bytes: &[u8]) -> io::Result<()> {
        staged.write_all(bytes)
    }

    fn sync_backup_staging(&self, staged: &mut NamedTempFile) -> io::Result<()> {
        staged.flush()?;
        staged.as_file().sync_all()
    }

    fn replace_backup(&self, staged: NamedTempFile, backup: &Path) -> io::Result<()> {
        staged
            .persist(backup)
            .map(|_| ())
            .map_err(|failure| failure.error)
    }

    fn create_staging(&self, directory: &Path) -> io::Result<NamedTempFile> {
        NamedTempFile::new_in(directory)
    }

    fn write_staging(
        &self,
        temporary: &mut NamedTempFile,
        bytes: &[u8],
        target: &Path,
    ) -> io::Result<()> {
        temporary.write_all(bytes)?;
        // The destination's own mode is carried on to the staged file where the
        // filesystem supports it. A lookup or copy that fails is not a reason to
        // lose a save the user asked for correctly: the staged file then keeps the
        // private mode its temporary file was created with, so the saved file's
        // mode can differ from the one it replaced.
        if let Ok(metadata) = fs::metadata(target) {
            let _ = temporary.as_file().set_permissions(metadata.permissions());
        }
        Ok(())
    }

    fn sync_staging(&self, temporary: &mut NamedTempFile) -> io::Result<()> {
        temporary.flush()?;
        temporary.as_file().sync_all()
    }

    fn replace_target(&self, temporary: NamedTempFile, target: &Path) -> io::Result<()> {
        // A rename, never a delete: the destination either becomes the staged file
        // or is left exactly as it was.
        temporary
            .persist(target)
            .map(|_| ())
            .map_err(|failure| failure.error)
    }
}

/// The commit itself, over an injected set of operations.
fn commit_with(
    operations: &dyn CommitOperations,
    path: &Path,
    snapshot: &ConfigSnapshot,
    prepared: &PreparedSave,
) -> Result<ConfigSnapshot, Error> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let backup = backup_path(path);

    let loaded = operations
        .read_target(path)
        .map_err(|source| io_error(path, source))?;
    if loaded != snapshot.raw {
        return Err(conflict(path));
    }

    stage_backup(operations, directory, &backup, &loaded)
        .map_err(|source| io_error(&backup, source))?;

    let mut temporary = operations
        .create_staging(directory)
        .map_err(|source| io_error(path, source))?;
    operations
        .write_staging(&mut temporary, &prepared.committed.raw, path)
        .map_err(|source| io_error(path, source))?;
    operations
        .sync_staging(&mut temporary)
        .map_err(|source| io_error(path, source))?;

    // The last check that can be made from here. What another writer does
    // between this read and the rename below is the race that remains.
    let current = operations
        .read_target(path)
        .map_err(|source| io_error(path, source))?;
    if current != snapshot.raw {
        return Err(conflict(path));
    }

    operations
        .replace_target(temporary, path)
        .map_err(|source| io_error(path, source))?;

    Ok(prepared.committed.clone())
}

/// Stages the next backup completely before its name changes.
///
/// Creation, write, flush and sync all happen on an owned temporary file in the
/// target's own directory, so a failure at any of them leaves the target and the
/// backup that already stands at the backup name exactly as they were. Only a
/// fully staged file is renamed over that name.
fn stage_backup(
    operations: &dyn CommitOperations,
    directory: &Path,
    backup: &Path,
    bytes: &[u8],
) -> io::Result<()> {
    let mut staged = operations.create_backup_staging(directory)?;
    operations.write_backup_staging(&mut staged, bytes)?;
    operations.sync_backup_staging(&mut staged)?;
    operations.replace_backup(staged, backup)
}

/// The backup name: the complete target file name plus [`BACKUP_SUFFIX`].
///
/// `OsString` does the appending, so a target name that is not valid UTF-8 keeps
/// its own bytes instead of a lossy rendering of them.
fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(BACKUP_SUFFIX);
    PathBuf::from(name)
}

fn conflict(path: &Path) -> Error {
    Error::ConfigConflict {
        path: path.to_path_buf(),
    }
}

fn io_error(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn not_a_regular_file(path: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("cannot save to {}: {why}", path.display()),
    )
}

fn prepare_error(message: String) -> Error {
    Error::InvalidInput {
        context: "prepare_save".to_string(),
        message,
    }
}

/// Fails the named boundary and delegates every other operation to the real
/// filesystem, so a case exercises the whole commit path up to that boundary.
#[cfg(test)]
struct Faulting {
    at: Fault,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    BackupCreate,
    BackupWrite,
    BackupSync,
    BackupReplace,
    TargetWrite,
    TargetSync,
    TargetReplace,
}

#[cfg(test)]
impl Faulting {
    fn at(at: Fault) -> Self {
        Self { at }
    }

    fn injected(&self, name: &str) -> io::Error {
        io::Error::other(format!("injected {name} failure"))
    }
}

#[cfg(test)]
impl CommitOperations for Faulting {
    fn read_target(&self, path: &Path) -> io::Result<Vec<u8>> {
        FileSystem.read_target(path)
    }

    fn create_backup_staging(&self, directory: &Path) -> io::Result<NamedTempFile> {
        if self.at == Fault::BackupCreate {
            return Err(self.injected("backup staging creation"));
        }
        FileSystem.create_backup_staging(directory)
    }

    fn write_backup_staging(&self, staged: &mut NamedTempFile, bytes: &[u8]) -> io::Result<()> {
        if self.at == Fault::BackupWrite {
            return Err(self.injected("backup write"));
        }
        FileSystem.write_backup_staging(staged, bytes)
    }

    fn sync_backup_staging(&self, staged: &mut NamedTempFile) -> io::Result<()> {
        if self.at == Fault::BackupSync {
            return Err(self.injected("backup sync"));
        }
        FileSystem.sync_backup_staging(staged)
    }

    fn replace_backup(&self, staged: NamedTempFile, backup: &Path) -> io::Result<()> {
        if self.at == Fault::BackupReplace {
            return Err(self.injected("backup replacement"));
        }
        FileSystem.replace_backup(staged, backup)
    }

    fn create_staging(&self, directory: &Path) -> io::Result<NamedTempFile> {
        FileSystem.create_staging(directory)
    }

    fn write_staging(
        &self,
        temporary: &mut NamedTempFile,
        bytes: &[u8],
        target: &Path,
    ) -> io::Result<()> {
        if self.at == Fault::TargetWrite {
            return Err(self.injected("target temporary write"));
        }
        FileSystem.write_staging(temporary, bytes, target)
    }

    fn sync_staging(&self, temporary: &mut NamedTempFile) -> io::Result<()> {
        if self.at == Fault::TargetSync {
            return Err(self.injected("target file sync"));
        }
        FileSystem.sync_staging(temporary)
    }

    fn replace_target(&self, temporary: NamedTempFile, target: &Path) -> io::Result<()> {
        if self.at == Fault::TargetReplace {
            return Err(self.injected("target replacement"));
        }
        FileSystem.replace_target(temporary, target)
    }
}

/// Writes `external` bytes to the target right after the first read, which is the
/// check that precedes staging. The commit's own last check must then see them.
#[cfg(test)]
struct ChangedAfterFirstRead<'a> {
    reads: std::cell::Cell<usize>,
    external: &'a [u8],
}

#[cfg(test)]
impl CommitOperations for ChangedAfterFirstRead<'_> {
    fn read_target(&self, path: &Path) -> io::Result<Vec<u8>> {
        let bytes = FileSystem.read_target(path)?;
        self.reads.set(self.reads.get() + 1);
        if self.reads.get() == 1 {
            std::fs::write(path, self.external)?;
        }
        Ok(bytes)
    }

    fn create_backup_staging(&self, directory: &Path) -> io::Result<NamedTempFile> {
        FileSystem.create_backup_staging(directory)
    }

    fn write_backup_staging(&self, staged: &mut NamedTempFile, bytes: &[u8]) -> io::Result<()> {
        FileSystem.write_backup_staging(staged, bytes)
    }

    fn sync_backup_staging(&self, staged: &mut NamedTempFile) -> io::Result<()> {
        FileSystem.sync_backup_staging(staged)
    }

    fn replace_backup(&self, staged: NamedTempFile, backup: &Path) -> io::Result<()> {
        FileSystem.replace_backup(staged, backup)
    }

    fn create_staging(&self, directory: &Path) -> io::Result<NamedTempFile> {
        FileSystem.create_staging(directory)
    }

    fn write_staging(
        &self,
        temporary: &mut NamedTempFile,
        bytes: &[u8],
        target: &Path,
    ) -> io::Result<()> {
        FileSystem.write_staging(temporary, bytes, target)
    }

    fn sync_staging(&self, temporary: &mut NamedTempFile) -> io::Result<()> {
        FileSystem.sync_staging(temporary)
    }

    fn replace_target(&self, temporary: NamedTempFile, target: &Path) -> io::Result<()> {
        FileSystem.replace_target(temporary, target)
    }
}

/// Renames for real and then lets another writer overwrite the target, which is
/// what a commit that read the file back would have reported instead.
#[cfg(test)]
struct WrittenElsewhereAfterRename;

#[cfg(test)]
impl CommitOperations for WrittenElsewhereAfterRename {
    fn read_target(&self, path: &Path) -> io::Result<Vec<u8>> {
        FileSystem.read_target(path)
    }

    fn create_backup_staging(&self, directory: &Path) -> io::Result<NamedTempFile> {
        FileSystem.create_backup_staging(directory)
    }

    fn write_backup_staging(&self, staged: &mut NamedTempFile, bytes: &[u8]) -> io::Result<()> {
        FileSystem.write_backup_staging(staged, bytes)
    }

    fn sync_backup_staging(&self, staged: &mut NamedTempFile) -> io::Result<()> {
        FileSystem.sync_backup_staging(staged)
    }

    fn replace_backup(&self, staged: NamedTempFile, backup: &Path) -> io::Result<()> {
        FileSystem.replace_backup(staged, backup)
    }

    fn create_staging(&self, directory: &Path) -> io::Result<NamedTempFile> {
        FileSystem.create_staging(directory)
    }

    fn write_staging(
        &self,
        temporary: &mut NamedTempFile,
        bytes: &[u8],
        target: &Path,
    ) -> io::Result<()> {
        FileSystem.write_staging(temporary, bytes, target)
    }

    fn sync_staging(&self, temporary: &mut NamedTempFile) -> io::Result<()> {
        FileSystem.sync_staging(temporary)
    }

    fn replace_target(&self, temporary: NamedTempFile, target: &Path) -> io::Result<()> {
        FileSystem.replace_target(temporary, target)?;
        std::fs::write(target, b"written by someone else")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::types::{ModId, ModMetadata, PackageId, ParsedMetadata, Source};
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// A file in the shape the game writes: declaration, a `version`, a `list`
    /// attribute on the list, indented entries and a field we do not know.
    const DOCUMENT: &[u8] = concat!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n",
        "<ModsConfigData>\n",
        "  <version>1.5.4104 rev435</version>\n",
        "  <activeMods list=\"true\">\n",
        "    <li>one.mod</li>\n",
        "  </activeMods>\n",
        "  <knownExpansions>\n",
        "    <li>ludeon.rimworld</li>\n",
        "  </knownExpansions>\n",
        "</ModsConfigData>\n",
    )
    .as_bytes();

    #[cfg(unix)]
    #[test]
    fn backup_path_preserves_non_utf8_bytes_without_filesystem_creation() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let path = PathBuf::from(OsString::from_vec(
            b"/fixture/ModsConfig \xe4\x2d name.xml".to_vec(),
        ));
        let backup = backup_path(&path);
        assert_eq!(
            backup.as_os_str().as_bytes(),
            b"/fixture/ModsConfig \xe4\x2d name.xml.rimsort-rust.bak",
            "backup naming must preserve bytes even when the filesystem rejects the name"
        );
    }

    /// A previous save's backup, distinguishable from every byte this case uses.
    const PREVIOUS_BACKUP: &[u8] = b"the backup a previous save left behind";

    /// One loaded, prepared case inside its own temporary directory.
    struct Staged {
        directory: TempDir,
        path: PathBuf,
        snapshot: ConfigSnapshot,
        prepared: PreparedSave,
    }

    impl Staged {
        /// Writes `document`, loads it and prepares a save holding exactly
        /// `tokens`, each as the catalog resolves it.
        fn with(document: &[u8], tokens: &[&str]) -> Self {
            let directory = TempDir::new().expect("a temporary directory");
            let path = directory.path().join("ModsConfig.xml");
            fs::write(&path, document).expect("the target file must be written");
            let snapshot = parse_config(document).expect("the document must load");
            let catalog = catalog();
            let entries: Vec<ActiveEntry> = tokens
                .iter()
                .map(|token| catalog.resolve_token(token))
                .collect();
            let prepared =
                prepare_save(&snapshot, &entries, &catalog).expect("the save must prepare");
            Self {
                directory,
                path,
                snapshot,
                prepared,
            }
        }

        fn commit(&self, operations: &dyn CommitOperations) -> Result<ConfigSnapshot, Error> {
            commit_with(operations, &self.path, &self.snapshot, &self.prepared)
        }

        /// Stands in for an earlier successful save: a backup of different bytes
        /// is already sitting at the backup name.
        fn with_previous_backup(self) -> Self {
            fs::write(self.backup(), PREVIOUS_BACKUP).expect("the previous backup must be written");
            self
        }

        fn backup(&self) -> PathBuf {
            self.directory
                .path()
                .join("ModsConfig.xml.rimsort-rust.bak")
        }

        fn bytes(&self) -> Vec<u8> {
            fs::read(&self.path).expect("the target file must still be readable")
        }

        fn backup_bytes(&self) -> Vec<u8> {
            fs::read(self.backup()).expect("the backup file must be readable")
        }

        /// The names inside the case's directory, sorted.
        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.directory.path())
                .expect("the directory must be readable")
                .map(|entry| {
                    let name = entry.expect("a directory entry").file_name();
                    name.to_string_lossy().into_owned()
                })
                .collect();
            names.sort();
            names
        }
    }

    fn usable(package_id: &str) -> ParsedMetadata {
        ParsedMetadata {
            package_id: Some(
                PackageId::normalize(package_id).expect("a package id must normalize"),
            ),
            name: Some(package_id.to_string()),
            ..ParsedMetadata::default()
        }
    }

    fn catalog() -> Catalog {
        Catalog::new(vec![
            ModMetadata::new(ModId::new("/mods/one"), Source::Local, usable("one.mod")),
            ModMetadata::new(ModId::new("/mods/two"), Source::Local, usable("two.mod")),
        ])
        .expect("the catalog must build")
    }

    #[test]
    fn a_successful_commit_writes_the_prepared_bytes_and_backs_up_the_load() {
        let case = Staged::with(DOCUMENT, &["two.mod", "one.mod"]);
        let loaded = case.bytes();

        let committed = case
            .commit(&FileSystem)
            .expect("a commit on a quiet filesystem must succeed");

        assert_eq!(
            case.bytes(),
            committed.raw,
            "the file holds exactly the bytes the returned snapshot describes"
        );
        assert_eq!(
            committed.active_tokens,
            ["two.mod".to_string(), "one.mod".to_string()],
            "and the returned snapshot describes the saved list in the order it was written"
        );
        assert_eq!(
            case.backup_bytes(),
            loaded,
            "the backup holds the bytes that were loaded, not the new ones"
        );
        assert_eq!(
            case.names(),
            [
                "ModsConfig.xml".to_string(),
                "ModsConfig.xml.rimsort-rust.bak".to_string()
            ],
            "a successful save leaves the target and exactly one backup behind"
        );
    }

    /// The refusal of a failed backup has to name the backup file it could not
    /// write, not just say that something went wrong.
    fn assert_backup_error(error: Error, case: &Staged) {
        let Error::Io { path, source } = &error else {
            panic!("a failed backup is an operational failure, got {error}");
        };
        assert_eq!(
            path,
            &case.backup(),
            "the error names the backup file it could not stage, got {source}"
        );
    }

    #[test]
    fn a_backup_creation_failure_leaves_the_target_and_its_directory_alone() {
        let case = Staged::with(DOCUMENT, &["two.mod", "one.mod"]).with_previous_backup();
        let loaded = case.bytes();

        let error = case
            .commit(&Faulting::at(Fault::BackupCreate))
            .expect_err("a failed backup must block the save");

        assert_backup_error(error, &case);
        assert_eq!(case.bytes(), loaded, "the target's bytes are untouched");
        assert_eq!(
            case.backup_bytes(),
            PREVIOUS_BACKUP,
            "the backup that was already there is still there"
        );
        assert_eq!(
            case.names(),
            [
                "ModsConfig.xml".to_string(),
                "ModsConfig.xml.rimsort-rust.bak".to_string()
            ],
            "a failed backup staging leaves no owned temporary file behind"
        );
    }

    #[test]
    fn a_backup_write_failure_keeps_the_previous_backup_and_the_target() {
        let case = Staged::with(DOCUMENT, &["two.mod", "one.mod"]).with_previous_backup();
        let loaded = case.bytes();

        let error = case
            .commit(&Faulting::at(Fault::BackupWrite))
            .expect_err("a failed backup write must block the save");

        assert_backup_error(error, &case);
        assert_eq!(case.bytes(), loaded, "the target's bytes are untouched");
        assert_eq!(
            case.backup_bytes(),
            PREVIOUS_BACKUP,
            "a failure before the backup replacement must not destroy the last good backup"
        );
        assert_eq!(
            case.names(),
            [
                "ModsConfig.xml".to_string(),
                "ModsConfig.xml.rimsort-rust.bak".to_string()
            ],
            "the owned backup staging file is cleaned up"
        );
    }

    #[test]
    fn a_backup_sync_failure_keeps_the_previous_backup_and_the_target() {
        let case = Staged::with(DOCUMENT, &["two.mod", "one.mod"]).with_previous_backup();
        let loaded = case.bytes();

        let error = case
            .commit(&Faulting::at(Fault::BackupSync))
            .expect_err("a failed backup sync must block the save");

        assert_backup_error(error, &case);
        assert_eq!(case.bytes(), loaded, "the target's bytes are untouched");
        assert_eq!(
            case.backup_bytes(),
            PREVIOUS_BACKUP,
            "a failure before the backup replacement must not destroy the last good backup"
        );
        assert_eq!(
            case.names(),
            [
                "ModsConfig.xml".to_string(),
                "ModsConfig.xml.rimsort-rust.bak".to_string()
            ],
            "the owned backup staging file is cleaned up"
        );
    }

    #[test]
    fn a_backup_replacement_failure_keeps_the_previous_backup_and_the_target() {
        let case = Staged::with(DOCUMENT, &["two.mod", "one.mod"]).with_previous_backup();
        let loaded = case.bytes();

        let error = case
            .commit(&Faulting::at(Fault::BackupReplace))
            .expect_err("a failed backup replacement must block the save");

        assert_backup_error(error, &case);
        assert_eq!(case.bytes(), loaded, "the target's bytes are untouched");
        assert_eq!(
            case.backup_bytes(),
            PREVIOUS_BACKUP,
            "the previous backup is still what it was: its replacement never happened"
        );
        assert_eq!(
            case.names(),
            [
                "ModsConfig.xml".to_string(),
                "ModsConfig.xml.rimsort-rust.bak".to_string()
            ],
            "the owned backup staging file is cleaned up"
        );
    }

    #[test]
    fn a_successful_save_replaces_the_previous_backup_with_the_loaded_bytes() {
        let case = Staged::with(DOCUMENT, &["two.mod", "one.mod"]).with_previous_backup();
        let loaded = case.bytes();

        case.commit(&FileSystem)
            .expect("a commit over an existing backup must succeed");

        assert_eq!(
            case.backup_bytes(),
            loaded,
            "the completed backup is the bytes this save loaded, written atomically at the end"
        );
        assert_eq!(
            case.names(),
            [
                "ModsConfig.xml".to_string(),
                "ModsConfig.xml.rimsort-rust.bak".to_string()
            ],
            "and exactly one backup stands when the save is done"
        );
    }

    #[test]
    fn a_temporary_write_failure_leaves_the_target_bytes_intact() {
        let case = Staged::with(DOCUMENT, &["two.mod"]);
        let loaded = case.bytes();

        let error = case
            .commit(&Faulting::at(Fault::TargetWrite))
            .expect_err("a failed temporary write must block the save");

        assert!(
            matches!(error, Error::Io { .. }),
            "an operational failure is reported as one, got {error}"
        );
        assert_eq!(case.bytes(), loaded, "the target's bytes are untouched");
        assert_eq!(
            case.backup_bytes(),
            loaded,
            "the backup was written before the staging failure"
        );
        assert_eq!(
            case.names(),
            [
                "ModsConfig.xml".to_string(),
                "ModsConfig.xml.rimsort-rust.bak".to_string()
            ],
            "the owned temporary file is cleaned up by its drop"
        );
    }

    #[test]
    fn a_sync_failure_leaves_the_target_bytes_intact() {
        let case = Staged::with(DOCUMENT, &["two.mod"]);
        let loaded = case.bytes();

        let error = case
            .commit(&Faulting::at(Fault::TargetSync))
            .expect_err("a failed sync must block the save");

        assert!(
            matches!(error, Error::Io { .. }),
            "an operational failure is reported as one, got {error}"
        );
        assert_eq!(case.bytes(), loaded, "the target's bytes are untouched");
        assert_eq!(
            case.names(),
            [
                "ModsConfig.xml".to_string(),
                "ModsConfig.xml.rimsort-rust.bak".to_string()
            ],
            "the staged file is owned and removed, the target is not renamed over"
        );
    }

    #[test]
    fn a_replacement_failure_leaves_the_target_bytes_intact() {
        let case = Staged::with(DOCUMENT, &["two.mod"]);
        let loaded = case.bytes();

        let error = case
            .commit(&Faulting::at(Fault::TargetReplace))
            .expect_err("a failed rename must block the save");

        assert!(
            matches!(error, Error::Io { .. }),
            "an operational failure is reported as one, got {error}"
        );
        assert_eq!(
            case.bytes(),
            loaded,
            "the target still holds its loaded bytes: it was never deleted or renamed over"
        );
        assert_eq!(
            case.names(),
            [
                "ModsConfig.xml".to_string(),
                "ModsConfig.xml.rimsort-rust.bak".to_string()
            ],
            "the staged file is owned and removed"
        );
    }

    /// The target is changed after the *first* read, so this proves the second
    /// comparison catches an intervening writer; the window between that second
    /// comparison and the rename itself is the race that remains.
    #[test]
    fn a_change_after_the_first_read_is_blocked_by_the_second_comparison() {
        let case = Staged::with(DOCUMENT, &["two.mod"]);
        let external =
            b"<ModsConfigData><activeMods><li>written.by.the.game</li></activeMods></ModsConfigData>";

        let error = case
            .commit(&ChangedAfterFirstRead {
                reads: std::cell::Cell::new(0),
                external,
            })
            .expect_err("a file changed under us must not be overwritten");

        assert!(
            matches!(error, Error::ConfigConflict { .. }),
            "the second comparison reports the conflict variant, got {error}"
        );
        assert_eq!(
            case.bytes(),
            external,
            "the other writer's bytes survive: nothing was renamed over them"
        );
    }

    #[test]
    fn the_returned_snapshot_is_the_committed_bytes_not_a_reread() {
        let case = Staged::with(DOCUMENT, &["two.mod"]);

        let committed = case
            .commit(&WrittenElsewhereAfterRename)
            .expect("a save whose rename succeeded is a successful save");

        assert_eq!(
            committed.active_tokens,
            ["two.mod".to_string()],
            "the result describes the prepared bytes, not whatever the file holds now"
        );
        assert_eq!(
            case.bytes(),
            b"written by someone else",
            "and the later on-disk change is exactly what a post-commit reread would have seen"
        );
    }
}
