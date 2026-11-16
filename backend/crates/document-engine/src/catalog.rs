use crate::markdown::Document;
use serde::{Deserialize, Serialize};
use specspace_domain::RepositoryPath;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema_version: u32,
    pub documents: Vec<CatalogEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub document_id: String,
    pub path: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct CatalogFinding {
    pub code: &'static str,
    pub path: String,
}

/// Validate the complete Git snapshot, including Markdown outside the catalog.
/// The return value is ordered by catalog, never filesystem enumeration order.
pub fn validate<'a>(
    catalog: &Catalog,
    documents: &'a [(&str, Document)],
) -> Result<Vec<&'a Document>, Vec<CatalogFinding>> {
    let mut findings = vec![];
    inspect(catalog, documents, |code, path| {
        findings.push(CatalogFinding {
            code,
            path: path.into(),
        })
    })
    .ok_or(findings)
}

/// Stream diagnostics without first allocating every finding. Callers can keep
/// bounded output and full counts while retaining the complete validation pass.
pub fn inspect<'a>(
    catalog: &Catalog,
    documents: &'a [(&str, Document)],
    mut emit: impl FnMut(&'static str, &str),
) -> Option<Vec<&'a Document>> {
    let mut has_errors = false;
    let mut report = |code, path: &str| {
        has_errors = true;
        emit(code, path);
    };
    if catalog.schema_version != 1 {
        report("UNSUPPORTED_SCHEMA", ".specspace/catalog.json");
    }
    if documents.len() > 1000
        || documents
            .iter()
            .map(|(_, doc)| doc.source.len())
            .sum::<usize>()
            > 20 * 1024 * 1024
    {
        report("BOOK_TOO_LARGE", ".specspace/catalog.json");
    }
    let mut files = HashMap::new();
    let mut path_keys = HashSet::new();
    let mut document_ids = HashSet::new();
    let mut section_ids = HashSet::new();
    for (path, document) in documents {
        if RepositoryPath::parse(*path).is_err()
            || !path.starts_with("docs/")
            || !path.ends_with(".md")
        {
            report("PATH_FORBIDDEN", path);
        }
        if !path_keys.insert(path.to_lowercase()) {
            report("DUPLICATE_PATH", path);
        }
        files.insert(*path, document);
        if !document_ids.insert(&document.metadata.document_id) {
            report("DUPLICATE_DOCUMENT_ID", path);
        }
        for block in &document.blocks {
            if block.kind == "heading"
                && !block.section_id.is_empty()
                && !section_ids.insert(&block.section_id)
            {
                report("DUPLICATE_SECTION_ID", path);
            }
        }
    }
    let mut catalog_paths = HashSet::new();
    let mut catalog_ids = HashSet::new();
    let mut ordered = vec![];
    for entry in &catalog.documents {
        if !catalog_paths.insert(entry.path.as_str())
            || !catalog_ids.insert(entry.document_id.as_str())
        {
            report("DUPLICATE_CATALOG_ENTRY", &entry.path);
        }
        match files.get(entry.path.as_str()) {
            Some(document) if document.metadata.document_id == entry.document_id => {
                ordered.push(*document)
            }
            Some(_) => report("DOCUMENT_ID_MISMATCH", &entry.path),
            None => report("CATALOG_FILE_MISSING", &entry.path),
        }
    }
    for (path, _) in documents {
        if !catalog_paths.contains(path) {
            report("UNCATALOGUED_DOCUMENT", path);
        }
    }
    if has_errors { None } else { Some(ordered) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::parse;

    fn document(id: &str, section: &str) -> Document {
        parse(&format!("---\ndocument_id: {id}\ntitle: Test\nowner_team: test\nschema_version: 1\n---\n\n<!-- ss:section:{section} -->\n## Test\n")).unwrap()
    }

    #[test]
    fn book_follows_catalog_order_and_orphans_are_not_silently_dropped() {
        let catalog = Catalog {
            schema_version: 1,
            documents: vec![
                CatalogEntry {
                    document_id: "DOC-B".into(),
                    path: "docs/b.md".into(),
                },
                CatalogEntry {
                    document_id: "DOC-A".into(),
                    path: "docs/a.md".into(),
                },
            ],
        };
        let mut documents = vec![
            ("docs/a.md", document("DOC-A", "SEC-A")),
            ("docs/b.md", document("DOC-B", "SEC-B")),
        ];
        let ordered = validate(&catalog, &documents).unwrap();
        assert_eq!(ordered[0].metadata.document_id, "DOC-B");
        documents.push(("docs/orphan.md", document("DOC-ORPHAN", "SEC-ORPHAN")));
        assert!(
            validate(&catalog, &documents)
                .unwrap_err()
                .iter()
                .any(|f| f.code == "UNCATALOGUED_DOCUMENT")
        );
    }

    #[test]
    fn duplicate_ids_across_documents_and_missing_files_block_publication() {
        let catalog = Catalog {
            schema_version: 1,
            documents: vec![CatalogEntry {
                document_id: "DOC-MISSING".into(),
                path: "docs/missing.md".into(),
            }],
        };
        let documents = vec![
            ("docs/a.md", document("DOC-A", "SEC-SAME")),
            ("docs/b.md", document("DOC-A", "SEC-SAME")),
        ];
        let findings = validate(&catalog, &documents).unwrap_err();
        for code in [
            "DUPLICATE_DOCUMENT_ID",
            "DUPLICATE_SECTION_ID",
            "CATALOG_FILE_MISSING",
        ] {
            assert!(findings.iter().any(|finding| finding.code == code));
        }
    }

    #[test]
    fn six_document_fixture_forms_one_ordered_searchable_book() {
        let catalog: Catalog = serde_json::from_str(include_str!(
            "../../../../fixtures/repository/.specspace/catalog.json"
        ))
        .unwrap();
        let sources = [
            (
                "docs/overview.md",
                include_str!("../../../../fixtures/repository/docs/overview.md"),
            ),
            (
                "docs/permissions.md",
                include_str!("../../../../fixtures/repository/docs/permissions.md"),
            ),
            (
                "docs/media.md",
                include_str!("../../../../fixtures/repository/docs/media.md"),
            ),
            (
                "docs/recordings.md",
                include_str!("../../../../fixtures/repository/docs/recordings.md"),
            ),
            (
                "docs/player.md",
                include_str!("../../../../fixtures/repository/docs/player.md"),
            ),
            (
                "docs/operations.md",
                include_str!("../../../../fixtures/repository/docs/operations.md"),
            ),
        ];
        let documents: Vec<_> = sources
            .iter()
            .map(|(path, source)| (*path, parse(source).unwrap()))
            .collect();
        let book = validate(&catalog, &documents).unwrap();
        assert_eq!(book.len(), 6);
        assert!(book.iter().all(|document| document.findings.is_empty()));
        let blocks: Vec<_> = book
            .iter()
            .flat_map(|document| document.visible_blocks())
            .collect();
        let results = crate::find::find("fixture-snapshot", &blocks, "מחיקה", 0, 100).unwrap();
        assert_eq!(results.total_matches, 4);
        assert_eq!(results.matches[3].document_id, "DOC-OPERATIONS");
    }
}
