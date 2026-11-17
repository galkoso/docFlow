//! Complete candidate Markdown validation for a Git draft save. No I/O, URL
//! fetching, authorization or Git mutation occurs here. Referenced image bytes
//! still require pinned media verification before a caller writes to Git.
use crate::{
    catalog::{self, Catalog},
    markdown::{self, Document},
    render,
};
use percent_encoding::percent_decode_str;
use pulldown_cmark::{BrokenLink, CowStr, Event, Options, Parser, Tag, TagEnd};
use serde::Serialize;
use specspace_domain::RepositoryPath;
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ops::Range,
};
use unicode_normalization::UnicodeNormalization;

pub const MAX_FINDINGS: usize = 1000;
const MAX_FILE: usize = 2 * 1024 * 1024;
const MAX_BOOK: usize = 20 * 1024 * 1024;
// Reference definitions can expand a short source into many large resolved
// destinations. Bound that work explicitly instead of truncating a valid result.
const MAX_REFERENCE_WORK: usize = 64 * 1024 * 1024;
const MAX_DEFINITION_WORK: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Warning,
    Blocker,
}

#[derive(Debug, Serialize)]
pub struct SaveFinding {
    pub code: &'static str,
    pub path: String,
    pub path_truncated: bool,
    pub severity: Severity,
    pub source_start_byte: Option<usize>,
    pub source_end_byte: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct SaveValidation {
    pub findings: Vec<SaveFinding>,
    pub findings_total: usize,
    pub findings_truncated: bool,
    pub blockers_total: usize,
    pub complete: bool,
    #[serde(skip_serializing)]
    pub section_ids: BTreeMap<String, Vec<String>>,
    #[serde(skip_serializing)]
    pub required_assets: BTreeSet<String>,
}
impl Default for SaveValidation {
    fn default() -> Self {
        Self {
            findings: vec![],
            findings_total: 0,
            findings_truncated: false,
            blockers_total: 0,
            complete: true,
            section_ids: BTreeMap::new(),
            required_assets: BTreeSet::new(),
        }
    }
}
impl SaveValidation {
    pub fn ready(&self) -> bool {
        self.complete && self.blockers_total == 0
    }
    pub fn findings_truncated(&self) -> bool {
        self.findings_truncated
    }
    fn add(
        &mut self,
        code: &'static str,
        path: &str,
        range: Option<Range<usize>>,
        severity: Severity,
    ) {
        self.findings_total += 1;
        self.blockers_total += usize::from(severity == Severity::Blocker);
        if self.findings.len() < MAX_FINDINGS {
            let mut characters = path.chars();
            let path: String = characters.by_ref().take(240).collect();
            let path_truncated = characters.next().is_some();
            self.findings.push(SaveFinding {
                code,
                path,
                path_truncated,
                severity,
                source_start_byte: range.as_ref().map(|range| range.start),
                source_end_byte: range.map(|range| range.end),
            });
        } else {
            self.findings_truncated = true;
        }
    }
    fn blocker(&mut self, code: &'static str, path: &str, range: Option<Range<usize>>) {
        self.add(code, path, range, Severity::Blocker);
    }
    fn work(&mut self, bytes: &mut usize, length: usize, path: &str, range: Range<usize>) -> bool {
        *bytes = bytes.saturating_add(length);
        if *bytes > MAX_REFERENCE_WORK {
            self.complete = false;
            self.blocker("REFERENCE_WORK_LIMIT", path, Some(range));
            return false;
        }
        true
    }
}

/// Normalize changed source only. Never serialize Markdown or rewrite unrelated
/// blocks. The raw recovery buffer remains separate and byte-preserving.
pub fn normalize(source: &str) -> Result<String, &'static str> {
    if source.len() > MAX_FILE {
        return Err("FILE_TOO_LARGE");
    }
    if source.contains('\0') {
        return Err("INVALID_CONTENT");
    }
    let lines = source.replace("\r\n", "\n").replace('\r', "\n");
    let mut result = String::with_capacity(lines.len());
    for character in lines.nfc() {
        result.push(character);
        if result.len() > MAX_FILE {
            return Err("FILE_TOO_LARGE");
        }
    }
    Ok(result)
}

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// Identity comparison for an old source that may need repair. Failure means
/// removed identities are unknown, not an empty set. No references are resolved.
pub fn section_ids(source: &str) -> Result<Vec<String>, &'static str> {
    let document = markdown::parse(source)?;
    if document
        .findings
        .iter()
        .any(|finding| matches!(finding.code, "SECTION_ID_REQUIRED" | "DUPLICATE_SECTION_ID"))
    {
        return Err("INVALID_SECTION_IDENTITIES");
    }
    let mut report = SaveValidation::default();
    let mut owners = HashMap::new();
    let mut work = 0;
    if !collect_sections("", &document, &mut owners, &mut report, &mut work) || !report.ready() {
        return Err("INVALID_SECTION_IDENTITIES");
    }
    Ok(report
        .section_ids
        .remove(&document.metadata.document_id)
        .unwrap_or_default())
}
fn body_offset(source: &str) -> Option<usize> {
    Some(4 + source.strip_prefix("---\n")?.find("\n---\n")? + 5)
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 120
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
}
fn decoded(value: &str) -> Result<String, &'static str> {
    let bytes = value.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            if bytes
                .get(at + 1)
                .is_none_or(|byte| !byte.is_ascii_hexdigit())
                || bytes
                    .get(at + 2)
                    .is_none_or(|byte| !byte.is_ascii_hexdigit())
            {
                return Err("INVALID_INTERNAL_REFERENCE");
            }
            at += 3;
        } else {
            at += 1;
        }
    }
    percent_decode_str(value)
        .decode_utf8()
        .map(String::from)
        .map_err(|_| "INVALID_INTERNAL_REFERENCE")
}

/// Every docs/*.md file in the pinned tree must be supplied, including orphans.
/// assets contains exact paths from that same tree, not caller-entered URLs.
pub fn validate(
    catalog: &Catalog,
    files: &[(String, String)],
    assets: &HashSet<String>,
) -> SaveValidation {
    let mut report = SaveValidation::default();
    let total = files
        .iter()
        .try_fold(0usize, |total, (_, source)| total.checked_add(source.len()));
    if files.len() > 1000 || total.is_none_or(|bytes| bytes > MAX_BOOK) {
        report.blocker("BOOK_TOO_LARGE", ".specspace/catalog.json", None);
        report.complete = false;
        return report;
    }
    if catalog.documents.len() > 1000 {
        report.blocker("BOOK_TOO_LARGE", ".specspace/catalog.json", None);
        report.complete = false;
        return report;
    }
    for path in files
        .iter()
        .map(|(path, _)| path)
        .chain(catalog.documents.iter().map(|entry| &entry.path))
    {
        if path.chars().take(241).count() > 240 {
            report.blocker("PATH_FORBIDDEN", path, None);
            report.complete = false;
            return report;
        }
    }
    for entry in &catalog.documents {
        if !identifier(&entry.document_id) {
            report.blocker("INVALID_DOCUMENT_ID", &entry.path, None);
            report.complete = false;
            return report;
        }
    }
    let mut documents = vec![];
    let mut definition_work = 0usize;
    for (path, source) in files {
        if !reference_budget(path, source, &mut report, &mut definition_work) {
            return report;
        }
        if source.contains('\0') {
            report.blocker("INVALID_CONTENT", path, None);
        }
        match markdown::parse(source) {
            Ok(document) => {
                for finding in &document.findings {
                    let severity = match finding.code {
                        // Draft saves retain protected source. Alt is mandatory
                        // at review/merge, rather than silently discarded here.
                        "UNSUPPORTED_HTML" | "UNSUPPORTED_HEADING_LEVEL" | "IMAGE_ALT_REQUIRED" => {
                            Severity::Warning
                        }
                        _ => Severity::Blocker,
                    };
                    report.add(
                        finding.code,
                        path,
                        Some(finding.source_start..finding.source_end),
                        severity,
                    );
                }
                if let Err(code) = render::events(&document) {
                    report.blocker(code, path, None);
                }
                documents.push((path.as_str(), document));
            }
            Err(code) => report.blocker(code, path, None),
        }
    }
    catalog::inspect(catalog, &documents, |code, path| {
        // The event scan below covers empty/nested headings with exact ranges.
        if code != "DUPLICATE_SECTION_ID" {
            report.blocker(code, path, None);
        }
    });
    for entry in &catalog.documents {
        if RepositoryPath::parse(&entry.path).is_err()
            || !entry.path.starts_with("docs/")
            || !entry.path.ends_with(".md")
        {
            report.blocker("PATH_FORBIDDEN", &entry.path, None);
        }
    }
    let mut owners = HashMap::<String, &str>::new();
    let mut work = 0;
    for (path, document) in &documents {
        if !collect_sections(path, document, &mut owners, &mut report, &mut work) {
            return report;
        }
    }
    let by_path: HashMap<_, _> = documents
        .iter()
        .map(|(path, document)| (*path, document.metadata.document_id.as_str()))
        .collect();
    let ids: HashSet<_> = documents
        .iter()
        .map(|(_, document)| document.metadata.document_id.as_str())
        .collect();
    for (path, document) in &documents {
        let offset = body_offset(&document.source).expect("parsed front matter");
        for (event, range) in
            Parser::new_ext(&document.source[offset..], options()).into_offset_iter()
        {
            let range = offset + range.start..offset + range.end;
            match event {
                Event::Start(Tag::Image { dest_url, .. }) => {
                    if !report.work(&mut work, dest_url.len(), path, range.clone()) {
                        return report;
                    }
                    if markdown::allowed_destination(&dest_url, true) {
                        report.required_assets.insert(dest_url.to_string());
                        if !assets.contains(dest_url.as_ref()) {
                            report.blocker("ASSET_MISSING", path, Some(range));
                        }
                    }
                }
                Event::Start(Tag::Link { dest_url, .. }) => {
                    if !report.work(&mut work, dest_url.len(), path, range.clone()) {
                        return report;
                    }
                    if (dest_url.starts_with('#')
                        || dest_url.starts_with("ss:")
                        || dest_url.starts_with("docs/"))
                        && let Err(code) = resolve(
                            &dest_url,
                            &document.metadata.document_id,
                            &ids,
                            &by_path,
                            &owners,
                        )
                    {
                        report.blocker(code, path, Some(range));
                    }
                }
                _ => {}
            }
        }
    }
    report
}

/// Shared read-side guard against the parser silently treating exhausted
/// reference expansions as plain text. This performs no save or whole-book
/// validation; callers must bound and validate their source separately.
pub(crate) fn check_reference_expansion(source: &str) -> Result<(), &'static str> {
    let mut report = SaveValidation::default();
    let mut work = 0;
    if reference_budget("", source, &mut report, &mut work) {
        Ok(())
    } else {
        Err(report
            .findings
            .first()
            .map_or("REFERENCE_EXPANSION_LIMIT", |finding| finding.code))
    }
}

/// pulldown-cmark 0.13 bounds reference expansion and then treats later uses as
/// plain text. Probe all resolution attempts with tiny callback destinations,
/// before trusting canonical events. Definition spans are replaced by spaces
/// (with line endings/byte positions preserved), so every lookup goes through
/// the callback against the original definitions. This also counts candidates
/// discarded by inline parsing rather than just the final emitted links.
fn reference_budget(
    path: &str,
    source: &str,
    report: &mut SaveValidation,
    scan_work: &mut usize,
) -> bool {
    let Some(offset) = body_offset(source) else {
        return true;
    };
    let body = &source[offset..];
    if !definition_work(path, body.len(), report, scan_work) {
        return false;
    }
    let parser = Parser::new_ext(body, options());
    let definitions = parser.reference_definitions();
    if definitions.iter().next().is_none() {
        return true;
    }
    let mut probe = body.as_bytes().to_vec();
    for (_, definition) in definitions.iter() {
        scrub(&mut probe, definition.span.clone());
    }
    let spent = Cell::new(0usize);
    let exceeded = Cell::new(None::<(usize, usize)>);
    let limit = body.len().max(100_000);
    loop {
        if !definition_work(path, body.len(), report, scan_work) {
            return false;
        }
        let text = std::str::from_utf8(&probe).expect("whole UTF-8 definition spans replaced");
        let callback = |link: BrokenLink<'_>| {
            let definition = definitions.get(&link.reference)?;
            let total = spent
                .get()
                .saturating_add(definition.dest.len())
                .saturating_add(definition.title.as_ref().map_or(0, |title| title.len()));
            spent.set(total);
            if total >= limit {
                if exceeded.get().is_none() {
                    exceeded.set(Some((link.span.start, link.span.end)));
                }
                return None;
            }
            Some((CowStr::Borrowed("#"), CowStr::Borrowed("")))
        };
        let mut parser = Parser::new_with_broken_link_callback(text, options(), Some(callback));
        let remaining: Vec<_> = parser
            .reference_definitions()
            .iter()
            .map(|(_, definition)| definition.span.clone())
            .collect();
        if !remaining.is_empty() {
            // CommonMark duplicates use the first definition. Remove later
            // shadow definitions too, while retaining the original lookup map.
            // Bound aggregate rescanning so adversarial duplicates cannot make
            // this probe quadratic without an explicit blocking limit result.
            drop(parser);
            for range in remaining {
                scrub(&mut probe, range);
            }
            continue;
        }
        while parser.next().is_some() {
            if let Some((start, end)) = exceeded.get() {
                report.complete = false;
                report.blocker(
                    "REFERENCE_EXPANSION_LIMIT",
                    path,
                    Some(offset + start..offset + end),
                );
                return false;
            }
        }
        return true;
    }
}
fn scrub(probe: &mut [u8], range: Range<usize>) {
    for byte in &mut probe[range] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}
fn definition_work(
    path: &str,
    length: usize,
    report: &mut SaveValidation,
    work: &mut usize,
) -> bool {
    *work = work.saturating_add(length);
    if *work > MAX_DEFINITION_WORK {
        report.complete = false;
        report.blocker("REFERENCE_DEFINITION_WORK_LIMIT", path, None);
        return false;
    }
    true
}

fn collect_sections<'a>(
    path: &str,
    document: &'a Document,
    owners: &mut HashMap<String, &'a str>,
    report: &mut SaveValidation,
    work: &mut usize,
) -> bool {
    let offset = body_offset(&document.source).expect("parsed front matter");
    let id = document.metadata.document_id.as_str();
    let titled: HashSet<_> = document
        .blocks
        .iter()
        .filter(|block| block.kind == "heading")
        .map(|block| block.source_start)
        .collect();
    let mut pending: Option<(String, Range<usize>)> = None;
    for (event, range) in Parser::new_ext(&document.source[offset..], options()).into_offset_iter()
    {
        let range = offset + range.start..offset + range.end;
        match event {
            Event::Html(ref html) | Event::InlineHtml(ref html)
                if markdown::marker(html).is_some() =>
            {
                if let Some((_, previous)) = pending.take() {
                    report.blocker("ORPHAN_SECTION_MARKER", path, Some(previous));
                }
                pending = Some((markdown::marker(html).unwrap().into(), range));
            }
            Event::Start(Tag::Heading { .. }) => {
                if !titled.contains(&range.start) {
                    report.blocker("EMPTY_HEADING", path, Some(range.clone()));
                }
                if let Some((section, _)) = pending.take() {
                    if owners.insert(section.clone(), id).is_some() {
                        report.blocker("DUPLICATE_SECTION_ID", path, Some(range));
                    }
                    report
                        .section_ids
                        .entry(id.into())
                        .or_default()
                        .push(section);
                }
            }
            Event::Start(Tag::HtmlBlock) | Event::End(TagEnd::HtmlBlock) => {}
            event => {
                if let Some((_, previous)) = pending.take() {
                    report.blocker("ORPHAN_SECTION_MARKER", path, Some(previous));
                }
                if let Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) =
                    event
                    && !report.work(work, dest_url.len(), path, range)
                {
                    return false;
                }
            }
        }
    }
    if let Some((_, range)) = pending {
        report.blocker("ORPHAN_SECTION_MARKER", path, Some(range));
    }
    true
}

fn resolve(
    destination: &str,
    current: &str,
    ids: &HashSet<&str>,
    by_path: &HashMap<&str, &str>,
    owners: &HashMap<String, &str>,
) -> Result<(), &'static str> {
    if destination.len() > 4096 {
        return Err("INVALID_INTERNAL_REFERENCE");
    }
    let (target, section) = destination
        .split_once('#')
        .map_or((destination, None), |(target, section)| {
            (target, Some(section))
        });
    let target = decoded(target)?;
    let document = if target.is_empty() {
        current
    } else if let Some(id) = target.strip_prefix("ss:") {
        if !identifier(id) {
            return Err("INVALID_INTERNAL_REFERENCE");
        }
        if !ids.contains(id) {
            return Err("DOCUMENT_REFERENCE_MISSING");
        }
        id
    } else {
        if RepositoryPath::parse(&target).is_err()
            || !target.starts_with("docs/")
            || !target.ends_with(".md")
        {
            return Err("INVALID_INTERNAL_REFERENCE");
        }
        by_path
            .get(target.as_str())
            .copied()
            .ok_or("DOCUMENT_REFERENCE_MISSING")?
    };
    if let Some(section) = section.filter(|section| !section.is_empty()) {
        let section = decoded(section)?;
        if !identifier(&section) {
            return Err("INVALID_INTERNAL_REFERENCE");
        }
        if owners.get(&section).copied() != Some(document) {
            return Err("SECTION_REFERENCE_MISSING");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
