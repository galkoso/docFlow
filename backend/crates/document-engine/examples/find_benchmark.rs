//! Reproducible engine-only feasibility measurement; excludes DB/network/DOM.
use specspace_document_engine::find::{VisibleBlock, find};
use std::time::Instant;

fn main() {
    let document_count = 1000;
    let target_bytes = 20 * 1024 * 1024;
    let phrase = "טקסט הקשר עברי רגיל לבדיקת ביצועים. ";
    let mut content = phrase.repeat(target_bytes / document_count / phrase.len());
    content.push_str("מחיקה");
    let contents: Vec<_> = (0..document_count)
        .map(|index| {
            let mut text = content.clone();
            let size =
                target_bytes / document_count + usize::from(index < target_bytes % document_count);
            text.extend(std::iter::repeat_n('x', size - text.len()));
            text
        })
        .collect();
    let ids: Vec<_> = (0..document_count)
        .map(|index| format!("DOC-{index}"))
        .collect();
    let blocks: Vec<_> = ids
        .iter()
        .zip(&contents)
        .map(|(id, text)| VisibleBlock {
            document_id: id,
            section_id: "SEC",
            block_id: "block",
            text,
        })
        .collect();
    if std::env::args().any(|argument| argument == "--indexed") {
        use specspace_document_engine::index::{self, IndexedBlock, IndexedText};
        let build_start = Instant::now();
        let texts: Vec<_> = contents
            .iter()
            .map(|text| IndexedText::build(text).unwrap())
            .collect();
        let build_seconds = build_start.elapsed().as_secs_f64();
        let map_bytes: usize = texts.iter().map(|text| text.encode_map().len()).sum();
        let indexed: Vec<_> = blocks
            .iter()
            .zip(&texts)
            .enumerate()
            .map(|(book_order, (block, text))| IndexedBlock {
                block: VisibleBlock {
                    document_id: block.document_id,
                    section_id: block.section_id,
                    block_id: block.block_id,
                    text: block.text,
                },
                book_order,
                text,
            })
            .collect();
        let mut durations = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..6)
                .map(|_| {
                    let indexed = &indexed;
                    scope.spawn(move || {
                        (0..20)
                            .map(|_| {
                                let start = Instant::now();
                                let page =
                                    index::find("synthetic-snapshot", indexed, "מחיקה", 0, 100)
                                        .unwrap();
                                assert_eq!(page.total_matches, document_count);
                                assert_eq!(page.matches.len(), 100);
                                start.elapsed().as_secs_f64()
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        durations.sort_by(f64::total_cmp);
        println!(
            "documents={document_count} bytes={target_bytes} readers=6 samples={} build_seconds={build_seconds:.3} map_bytes={map_bytes} indexed_engine_p95_seconds={:.3}",
            durations.len(),
            durations[durations.len() * 95 / 100 - 1]
        );
        return;
    }
    let mut durations = vec![];
    for _ in 0..20 {
        let start = Instant::now();
        let page = find("synthetic-snapshot", &blocks, "מחיקה", 0, 100).unwrap();
        assert_eq!(page.total_matches, document_count);
        assert_eq!(page.matches.len(), 100);
        durations.push(start.elapsed().as_secs_f64());
    }
    durations.sort_by(f64::total_cmp);
    println!(
        "documents={document_count} bytes={target_bytes} samples=20 engine_p95_seconds={:.3}",
        durations[18]
    );
}
