//! Prepared immutable index data. Database publication cannot accept an arbitrary
//! browser payload: it consumes a snapshot built from the validated Git book.
use crate::book::{BookError, BookSnapshot};
use sha2::{Digest, Sha256};
use specspace_document_engine::index::{IndexedText, NORMALIZER_VERSION};
use specspace_git_adapter::CommitSha;
use std::collections::HashSet;

pub struct IndexBlock {
    pub book_order: i32,
    pub document_id: String,
    pub section_id: String,
    pub block_id: String,
    pub text_hash: String,
    pub original: String,
    pub normalized: String,
    pub source_map: Vec<u8>,
}

pub struct IndexSnapshot {
    commit: CommitSha,
    manifest_hash: String,
    document_count: i32,
    original_bytes: i64,
    normalized_bytes: i64,
    blocks: Vec<IndexBlock>,
}

impl IndexSnapshot {
    pub fn build(book: &BookSnapshot) -> Result<Self, BookError> {
        if !book.findings.is_empty() || book.documents.len() > 1000 {
            return Err(BookError::SearchSnapshotNotReady);
        }
        let mut manifest = Sha256::new();
        manifest.update(NORMALIZER_VERSION.to_le_bytes());
        add(&mut manifest, book.commit.as_str());
        let mut ids = HashSet::new();
        let mut paths = HashSet::new();
        let mut blocks = vec![];
        let mut original_bytes = 0i64;
        let mut normalized_bytes = 0i64;
        let mut source_bytes = 0usize;
        for entry in &book.documents {
            let document = entry
                .content
                .as_ref()
                .ok_or(BookError::SearchSnapshotNotReady)?;
            source_bytes = source_bytes.saturating_add(document.source.len());
            if source_bytes > 20 * 1024 * 1024 || document.source.len() > 2 * 1024 * 1024 {
                return Err(BookError::BookTooLarge);
            }
            if format!("{:x}", Sha256::digest(document.source.as_bytes())) != document.sha256 {
                return Err(BookError::SearchSnapshotNotReady);
            }
            if entry.error_code.is_some()
                || !document.findings.is_empty()
                || entry.document_id != document.metadata.document_id
                || !ids.insert(&entry.document_id)
                || !paths.insert(&entry.path)
            {
                return Err(BookError::SearchSnapshotNotReady);
            }
            for value in [&entry.document_id, &entry.path, &document.sha256] {
                add(&mut manifest, value);
            }
            let mut block_ids = HashSet::new();
            for block in &document.blocks {
                if !block_ids.insert(&block.block_id) {
                    return Err(BookError::SearchSnapshotNotReady);
                }
                let text = IndexedText::build(&block.text).map_err(BookError::InvalidQuery)?;
                let source_map = text.encode_map();
                let text_hash =
                    IndexedText::fingerprint(&block.text, text.normalized(), &source_map);
                original_bytes += block.text.len() as i64;
                normalized_bytes += text.normalized().len() as i64;
                if original_bytes > 20 * 1024 * 1024 {
                    return Err(BookError::BookTooLarge);
                }
                blocks.push(IndexBlock {
                    book_order: i32::try_from(blocks.len()).map_err(|_| BookError::BookTooLarge)?,
                    document_id: entry.document_id.clone(),
                    section_id: block.section_id.clone(),
                    block_id: block.block_id.clone(),
                    text_hash,
                    original: block.text.clone(),
                    normalized: text.normalized().into(),
                    source_map,
                });
            }
        }
        Ok(Self {
            commit: book.commit.clone(),
            manifest_hash: format!("{:x}", manifest.finalize()),
            document_count: book.documents.len() as i32,
            original_bytes,
            normalized_bytes,
            blocks,
        })
    }
    pub fn commit(&self) -> &CommitSha {
        &self.commit
    }
    pub fn manifest_hash(&self) -> &str {
        &self.manifest_hash
    }
    pub fn document_count(&self) -> i32 {
        self.document_count
    }
    pub fn original_bytes(&self) -> i64 {
        self.original_bytes
    }
    pub fn normalized_bytes(&self) -> i64 {
        self.normalized_bytes
    }
    pub fn blocks(&self) -> &[IndexBlock] {
        &self.blocks
    }
}

fn add(hash: &mut Sha256, value: &str) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::BookDocument;
    use specspace_document_engine::markdown;

    fn book() -> BookSnapshot {
        let document = markdown::parse("---\ndocument_id: DOC\ntitle: Synthetic\nowner_team: TEAM\nschema_version: 1\n---\n<!-- ss:section:SEC -->\n# כותרת\n\nמְחיקה\n\nמְחיקה\n").unwrap();
        BookSnapshot {
            commit: CommitSha::parse("1".repeat(40)).unwrap(),
            updated_at: "2026-11-14T00:00:00Z".into(),
            documents: vec![BookDocument {
                document_id: "DOC".into(),
                path: "docs/one.md".into(),
                content: Some(document),
                error_code: None,
            }],
            findings: vec![],
        }
    }

    #[test]
    fn prepared_snapshot_binds_manifest_and_order_to_the_whole_book_and_reuses_text_identity() {
        let mut book = book();
        let first = IndexSnapshot::build(&book).unwrap();
        assert_eq!(first.document_count(), 1);
        assert_eq!(
            first
                .blocks()
                .iter()
                .map(|block| block.book_order)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(first.blocks()[1].text_hash, first.blocks()[2].text_hash);
        assert_eq!(first.blocks()[1].normalized, "מחיקה");
        let again = IndexSnapshot::build(&book).unwrap();
        assert_eq!(first.manifest_hash(), again.manifest_hash());
        book.commit = CommitSha::parse("2".repeat(40)).unwrap();
        let changed_commit = IndexSnapshot::build(&book).unwrap();
        assert_ne!(changed_commit.manifest_hash(), first.manifest_hash());
        book.documents[0].path = "docs/renamed.md".into();
        assert_ne!(
            IndexSnapshot::build(&book).unwrap().manifest_hash(),
            changed_commit.manifest_hash()
        );
    }

    #[test]
    fn invalid_or_incomplete_documents_never_become_prepared_indexes() {
        let mut book = book();
        book.documents[0]
            .content
            .as_mut()
            .unwrap()
            .findings
            .push(markdown::Finding {
                code: "INVALID",
                source_start: 0,
                source_end: 1,
            });
        assert!(matches!(
            IndexSnapshot::build(&book),
            Err(BookError::SearchSnapshotNotReady)
        ));
        book.documents[0].content = None;
        assert!(matches!(
            IndexSnapshot::build(&book),
            Err(BookError::SearchSnapshotNotReady)
        ));
    }
}
