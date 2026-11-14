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
