//! Ephemeral private payload cache. Authorization is resolved outside this
//! module on every read; cached context is evidence only, never authority.
use super::{FileReadContext, PreparedText};
use std::{
    collections::VecDeque,
    mem::size_of,
    sync::Arc,
    time::{Duration, Instant},
};

const CACHE_BYTES: usize = 32 * 1024 * 1024;
const CACHE_ENTRIES: usize = 32;
const CACHE_TTL: Duration = Duration::from_secs(300);
struct Entry {
    evidence: FileReadContext,
    text: Arc<PreparedText>,
    inserted: Instant,
    weight: usize,
}
pub(super) struct FileCache {
    entries: VecDeque<Entry>,
    bytes: usize,
    max_bytes: usize,
    max_entries: usize,
    ttl: Duration,
}
impl Default for FileCache {
    fn default() -> Self {
        Self::new(CACHE_BYTES, CACHE_ENTRIES, CACHE_TTL)
    }
}
impl FileCache {
    pub(super) fn new(max_bytes: usize, max_entries: usize, ttl: Duration) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_bytes,
            max_entries,
            ttl,
        }
    }
    fn prune(&mut self, now: Instant) {
        let mut removed = 0;
        self.entries.retain(|entry| {
            let fresh = now.saturating_duration_since(entry.inserted) < self.ttl;
            if !fresh {
                removed += entry.weight;
            }
            fresh
        });
        self.bytes -= removed;
    }
    pub(super) fn get(
        &mut self,
        context: &FileReadContext,
        now: Instant,
    ) -> Option<Arc<PreparedText>> {
        self.prune(now);
        let index = self
            .entries
            .iter()
            .position(|entry| same_evidence(&entry.evidence, context))?;
        let entry = self.entries.remove(index)?;
        let text = entry.text.clone();
        self.entries.push_back(entry); // Access changes LRU order, not absolute expiry.
        Some(text)
    }
    pub(super) fn insert(
        &mut self,
        context: &FileReadContext,
        text: Arc<PreparedText>,
        now: Instant,
    ) {
        self.prune(now);
        let weight = weight(context, &text);
        if self.max_entries == 0 || weight > self.max_bytes {
            return;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| same_evidence(&entry.evidence, context))
        {
            let previous = self.entries.remove(index).expect("located entry exists");
            self.bytes -= previous.weight;
        }
        while self.entries.len() >= self.max_entries || self.bytes > self.max_bytes - weight {
            let Some(previous) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= previous.weight;
        }
        self.bytes += weight;
        self.entries.push_back(Entry {
            evidence: context.clone(),
            text,
            inserted: now,
            weight,
        });
    }
}
fn same_evidence(a: &FileReadContext, b: &FileReadContext) -> bool {
    a.scope.actor == b.scope.actor
        && a.scope.organization == b.scope.organization
        && a.scope.proposal == b.scope.proposal
        && a.scope.connection == b.scope.connection
        && a.scope.branch == b.scope.branch
        && a.snapshot == b.snapshot
        && a.content_id == b.content_id
        && a.merge_base == b.merge_base
        && a.head == b.head
        && a.change == b.change
        && a.published_after_image == b.published_after_image
}
fn weight(context: &FileReadContext, text: &PreparedText) -> usize {
    let mut total = size_of::<Entry>() + size_of::<PreparedText>() + 2 * size_of::<usize>();
    if let Some(proof) = &context.published_after_image {
        total = total
            .saturating_add(proof.source_sha256.capacity())
            .saturating_add(proof.display_sha256.capacity());
    }
    for source in [&text.before, &text.after].into_iter().flatten() {
        total = total.saturating_add(source.capacity());
    }
    total = total.saturating_add(
        text.diff
            .chunks
            .capacity()
            .saturating_mul(size_of::<specspace_document_engine::diff::Chunk>()),
    );
    for value in [
        &text.diff.old_sha256,
        &text.diff.new_sha256,
        &context.content_id,
        &context.change.id,
        &context.scope.state,
    ] {
        total = total.saturating_add(value.capacity());
    }
    for value in [
        context.merge_base.as_str(),
        context.head.as_str(),
        context.scope.branch.as_str(),
        context.scope.recorded_head.as_str(),
        context.scope.recorded_base.as_str(),
    ] {
        total = total.saturating_add(value.len());
    }
    for side in [&context.change.before, &context.change.after]
        .into_iter()
        .flatten()
    {
        for value in [
            Some(&side.path),
            Some(&side.blob_oid),
            side.sha256.as_ref(),
            side.document_id.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            total = total.saturating_add(value.capacity());
        }
    }
    total
}
