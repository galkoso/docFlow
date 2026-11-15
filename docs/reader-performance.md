# Reader performance evidence

Measured locally on 2026-11-15 against PostgreSQL, using optimized Rust builds.
This records the HTTP backend gate. Browser rendering, first visible content,
scrolling to unloaded results and corporate-network latency remain unverified.

## Authenticated HTTP workload

The ignored `authenticated_http_six_readers_20mib_benchmark` test serves the real
Axum router over a loopback TCP socket. It uses six separate active users and
server sessions, a ten-connection PostgreSQL pool, and the production reader,
index worker, durable search store and renderer. Only the read-only Git source
is synthetic; no fixture source exists in the running application.

The book contains exactly 20,971,520 Markdown source bytes in 1,000 chapters.
Distinct stored visible payloads total 20,638,554 bytes, with an assertion above
19MiB to prevent repeated fixture text from deduplicating away the workload.
Matching queries produce 8,000 results across the complete book. Six query forms
cover literal Hebrew, niqqud, bidi marks, substrings, English case folding and
literal punctuation.

The test first runs 120 no-result searches against a fresh payload cache, then
one positive query. Six users then each perform 20 sequences of paged search,
result chapter fetch and book metadata fetch. Durations include HTTP body
transfer and client JSON decoding. Current access and immutable SHA checks run
on every request; result counts, chapter identity and rendering success are
asserted. Warm requests must perform no Git content fetches.

| Measurement | Seconds |
|---|---:|
| Worker build and publication after the reader loaded source | 2.821 |
| Fresh-cache no-result HTTP search p95, 120 samples | 0.112 |
| Subsequent first positive HTTP search | 0.655 |
| Warm HTTP search p95, 120 samples | 0.079 |
| Warm HTTP chapter response p95, 120 samples | 0.035 |
| Warm HTTP metadata response p95, 120 samples | 0.047 |

The executable assertions require search p95 below 1 second, chapter HTTP p95
below 1.5 seconds and metadata HTTP p95 below 2 seconds. Chapter and metadata
timing alone do not prove the corresponding browser-visible acceptance targets.
This test is excluded from routine unit runs; execute it explicitly from
`backend/`, with the local database running and `DATABASE_URL` configured:

```sh
cargo test --release -p specspace-api --features postgres-tests \
  --test book_postgres authenticated_http_six_readers_20mib_benchmark \
  -- --ignored --nocapture
```

Set `RUST_LOG=specspace_infrastructure::search_index=debug` to see existing phase
timings. Logs contain durations and counts, without query or source text.

## Bottleneck found and fixed

The first distinct-content HTTP run measured search p95 at 1.875 seconds. A
repeat with phase timings measured 2.021 seconds: repeated normalized-text SQL
scans took about 0.6 seconds, and the bounded two-request processing limit added
queueing under six readers. Warm requests fetched zero new payloads, but still
rescanned all text in PostgreSQL before using their cached maps.

Search now reads ordered references under the same repeatable-read transaction,
uses verified cached payloads directly and performs exact scoped SQL lookups
only for uncached matching text. Rust matching preserves canonical ranges,
excerpts, global book order and identities. The cache remains bounded at 128MiB
and 4,096 entries; every read still checks current access and main-index
provenance. Regression tests compare disjoint successive queries, nonmatches,
SQL metacharacters and paging against direct canonical search.

An earlier fixture without distinct large paragraph text measured 0.076 seconds
while deduplicating most payloads. It was rejected as insufficient evidence and
is not the benchmark recorded above.
