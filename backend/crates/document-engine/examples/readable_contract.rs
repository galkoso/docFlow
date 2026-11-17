//! Cross-language fixtures for the bounded readable client. Include continuation
//! through lists and code, protected source and semantic/source-only changes.
use serde_json::json;
use specspace_document_engine::diff::readable::{
    pair::ReadablePair,
    window::{EventCursor, UnitSummary},
};

fn main() {
    let preamble = "---\ndocument_id: DOC-READABLE\ntitle: בדיקת השוואה\nowner_team: test\nschema_version: 1\n---\n";
    let body = format!(
        "\n<!-- ss:section:SEC-ONE -->\n## כותרת\n\nOriginal **bold** and [safe](https://example.test) paragraph.\n\n3. First {}\n4. Second with `inline`\n   7. Nested\n   8. Last\n\n~~~rust\nlet value = \"{}\";\n~~~\n\n| Left | Right |\n| :--- | ---: |\n| a | **text** |\n\n<script>alert('inert')</script>\n",
        "שלום ".repeat(300),
        "אב".repeat(600),
    );
    let before = format!("{preamble}{body}");
    let after = before
        .replace("Original **bold**", "Changed __bold__")
        .replace("## כותרת", "## כותרת חדשה")
        .replace("| a |", "| b |")
        .replace("4. Second", "4. Updated second");
    let pair = ReadablePair::prepare(Some(&before), Some(&after));
    assert!(pair.ready);
    let mut units = vec![];
    for (side, document) in [("before", &pair.before), ("after", &pair.after)] {
        let document = document.document().unwrap();
        for (index, unit) in document.units.iter().enumerate() {
            let mut windows = vec![];
            if unit.protected_reason.is_none() {
                let mut cursor = EventCursor::default();
                loop {
                    let window = document.event_window(index, cursor, 1024).unwrap();
                    let next = window.next;
                    windows.push(window);
                    match next {
                        Some(next) => cursor = next,
                        None => break,
                    }
                }
            }
            units.push(
                json!({"side":side,"unit":UnitSummary::from_unit(index,unit),"windows":windows}),
            );
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"page":pair.page(0,50).unwrap(),"units":units}))
            .unwrap()
    );
}
