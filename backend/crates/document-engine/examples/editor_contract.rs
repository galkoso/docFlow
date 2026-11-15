//! Deterministic cross-language editor fixtures. CI compares this output with
//! the checked-in contract consumed by the browser editor tests.
use serde_json::json;
use specspace_document_engine::{
    editing::{self, EditorBlockEdit, EditorKind},
    markdown,
};

fn main() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/repository/docs");
    let mut cases = vec![];
    for name in [
        "overview",
        "permissions",
        "media",
        "recordings",
        "player",
        "operations",
    ] {
        cases.push((
            name.to_owned(),
            std::fs::read_to_string(root.join(format!("{name}.md"))).unwrap(),
        ));
    }
    let body = format!(
        "\n<!-- ss:section:SEC-RICH -->\n## Rich editor\n\nHebrew שָׁלוֹם __bold__ *italic* ~~strike~~ [reference][target].\n\n- [x] task\n- [ ] incomplete\n\n> <!-- ss:section:SEC-NESTED -->\n> ### Nested\n>\n> Quote with `code`\n\n~~~rust\nlet value = \"```\";\n~~~\n\n| Left | Right |\n| :--- | ---: |\n| a \\| b | **text** |\n\n![תמונה](assets/{}.png)\n\n<script>protected()</script>\n\n[target]: https://example.test/path \"reference title\"\n",
        "a".repeat(64)
    );
    cases.push(("gfm-protected".into(),format!("---\ndocument_id: DOC-RICH\ntitle: Rich editor\nowner_team: synthetic\nschema_version: 1\nextra: [preserved, values]\n---\n{body}")));
    let output:Vec<_>=cases.into_iter().map(|(name,source)| {
        let model=editing::parse(&source).unwrap();
        assert_eq!(model.assemble(&model.unchanged()).unwrap().source,source);
        let index=model.blocks.iter().position(|block|block.kind==EditorKind::Heading && block.protected_reason.is_none()).unwrap();
        let block=&model.blocks[index];
        let parsed=markdown::parse(&source).unwrap();
        let level=parsed.blocks.iter().find(|heading|heading.source_start==block.source_start_byte && heading.kind=="heading").unwrap().heading_level.unwrap();
        let text="כותרת בדיקה";
        let replacement=format!("{} {text}\n\n","#".repeat(level.into()));
        let mut edit=model.unchanged();
        edit.blocks[index]=EditorBlockEdit::Replace {key:block.key.clone(),markdown:replacement};
        let renamed=model.assemble(&edit).unwrap();
        json!({"name":name,"model":model,"rename":{"key":block.key,"text":text,"source":renamed.source,"edit":edit}})
    }).collect();
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}
