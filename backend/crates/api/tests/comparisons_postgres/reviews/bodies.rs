use super::*;

async fn files(f: &Fixture, snapshot: &str) -> (Value, Value, String) {
    let base = format!("{}/{snapshot}/files", history_path(f));
    let response = get(&f.app, &f.actor, &base, true).await;
    assert_eq!(response.0, StatusCode::OK);
    let files = response.1["files"].as_array().unwrap();
    let document = files
        .iter()
        .find(|file| file["kind"] == "document")
        .unwrap()
        .clone();
    let image = files
        .iter()
        .find(|file| file["kind"] == "asset")
        .cloned()
        .unwrap_or(Value::Null);
    (document, image, base)
}
fn body_path(base: &str, file: &Value, suffix: &str) -> String {
    format!("{base}/{}/{suffix}", file["id"].as_str().unwrap())
}
#[sqlx::test(migrations = "../../migrations")]
async fn shared_bodies_are_readable_by_members_and_preserve_exact_historical_source_and_receipts(
    pool: PgPool,
) {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};
    let (f, product, _) = configured(&pool, true).await;
    let (_, snapshot) = publish(&f, 2).await;
    let (document, image, base) = files(&f, &snapshot).await;
    let reader = user(&pool, f.organization, "READER").await;
    sqlx::query("UPDATE proposals SET state='CLOSED',head_sha=$2,target_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal).bind("4".repeat(40)).execute(&pool).await.unwrap();
    let raw = get(&f.app, &reader, &body_path(&base, &document, "raw"), true).await;
    assert_eq!(raw.0, StatusCode::OK, "{}", raw.1);
    assert_eq!(raw.1["file"], document);
    assert_eq!(raw.1["snapshot_id"], snapshot);
    assert_eq!(raw.1["head_sha"], HEAD);
    assert_eq!(raw.1["merge_base_sha"], OLD);
    let segments = raw.1["raw"]["segments"].as_array().unwrap();
    let before: String = segments
        .iter()
        .map(|s| s["old_text"].as_str().unwrap())
        .collect();
    let after: String = segments
        .iter()
        .map(|s| s["new_text"].as_str().unwrap())
        .collect();
    assert!(before.contains("PRIVATE SOURCE SENTINEL before"));
    assert!(after.contains("PRIVATE SOURCE SENTINEL edited"));
    assert_eq!(
        format!("{:x}", Sha256::digest(before.as_bytes())),
        document["before"]["sha256"]
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(after.as_bytes())),
        document["after"]["sha256"]
    );
    let page = get(
        &f.app,
        &product,
        &body_path(&base, &document, "readable"),
        true,
    )
    .await;
    assert_eq!(page.0, StatusCode::OK, "{}", page.1);
    assert_eq!(page.1["content_id"], raw.1["content_id"]);
    assert_eq!(page.1["page"]["ready"], true);
    let index = page.1["page"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["after"]["kind"] == "paragraph")
        .unwrap()["after"]["index"]
        .as_u64()
        .unwrap();
    let unit = get(
        &f.app,
        &reader,
        &body_path(
            &base,
            &document,
            &format!("readable/units/{index}?side=after"),
        ),
        true,
    )
    .await;
    assert_eq!(unit.0, StatusCode::OK, "{}", unit.1);
    assert_eq!(unit.1["snapshot_id"], snapshot);
    assert_eq!(unit.1["state"], "READY");
    assert!(unit.1["window"].is_object());
    let (status, headers, pixels) = image_response(
        &f.app,
        &reader,
        &body_path(&base, &image, "image?side=after"),
        true,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&pixels)
    );
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert!(pixels.starts_with(b"\x89PNG\r\n\x1a\n"));
    let proof: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(headers["X-Comparison-Image"].as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(proof["actor_id"], reader.id.to_string());
    assert_eq!(proof["snapshot_id"], snapshot);
    assert_eq!(proof["file_id"], image["id"]);
    assert_eq!(proof["source_blob_oid"], image["after"]["blob_oid"]);
    assert_eq!(
        proof["display_sha256"],
        format!("{:x}", Sha256::digest(&pixels))
    );
    let receipts = get(
        &f.app,
        &reader,
        &format!("{}/{snapshot}/media", history_path(&f)),
        true,
    )
    .await;
    let receipt = &receipts.1["receipts"][0];
    assert_eq!(proof["source_sha256"], receipt["source_sha256"]);
    assert_eq!(proof["display_sha256"], receipt["display_sha256"]);
    assert_eq!(proof["width"], receipt["display_width"]);
    assert_eq!(proof["height"], receipt["display_height"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn shared_body_queries_reject_alternative_snapshots_refs_paths_and_ambiguous_windows_before_io(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, true).await;
    let (_, snapshot) = publish(&f, 2).await;
    let (document, image, base) = files(&f, &snapshot).await;
    let reads = f.provider.reads.load(Ordering::SeqCst);
    let cases = [
        (&document, "raw?head=main"),
        (&document, "raw?snapshot=other"),
        (&document, "raw?offset=0&offset=1"),
        (&document, "raw?chunk=1025"),
        (&document, "raw?max_bytes=1"),
        (&document, "readable?limit=0"),
        (&document, "readable?offset=0&row=abcd"),
        (&document, "readable?offset=0&offset=1"),
        (&document, "readable/units/0?side=after&side=before"),
        (&document, "readable/units/0?side=after&max_bytes=1"),
        (&document, "readable/units/0?side=after&path=docs/a.md"),
        (&image, "image?side=after&side=before"),
        (&image, "image?side=after&snapshot=other"),
        (&image, "image?side=after&ref=main"),
        (&image, "image"),
    ];
    for (file, suffix) in cases {
        let response =
            image_response(&f.app, &f.actor, &body_path(&base, file, suffix), true).await;
        assert_eq!(response.0, StatusCode::BAD_REQUEST, "{suffix}");
        assert!(!response.1.contains_key("X-Comparison-Image"));
        assert!(!String::from_utf8_lossy(&response.2).contains("PRIVATE SOURCE SENTINEL"));
    }
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn shared_and_private_body_routes_have_separate_scope_and_obey_current_membership_and_binding(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, true).await;
    let (_, snapshot) = publish(&f, 2).await;
    let (document, image, base) = files(&f, &snapshot).await;
    let raw = body_path(&base, &document, "raw");
    let foreign_fixture = fixture(&pool, true).await;
    let foreign = &foreign_fixture.owner;
    // This account has no membership in the proposal's organization.
    assert_eq!(
        get(&f.app, foreign, &raw, true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&f.app, &f.actor, &raw, false).await.0,
        StatusCode::CONFLICT
    );
    let (private, private_file) = ready_file(&f).await;
    assert_eq!(
        get(
            &f.app,
            &f.actor,
            &raw.replace(&snapshot, private["snapshot_id"].as_str().unwrap()),
            true
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let private_route = raw_path(&f, &json!({"snapshot_id":snapshot}), &private_file);
    assert_eq!(
        get(&f.app, &f.actor, &private_route, true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(get(&f.app, &f.actor, &raw, true).await.0, StatusCode::OK);
    let reads = f.provider.reads.load(Ordering::SeqCst);
    sqlx::query("UPDATE repositories SET installation_id='99' WHERE organization_id=$1")
        .bind(f.organization)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&f.app, &f.actor, &raw, true).await.0,
        StatusCode::CONFLICT
    );
    let png = image_response(
        &f.app,
        &f.actor,
        &body_path(&base, &image, "image?side=after"),
        true,
    )
    .await;
    assert_eq!(png.0, StatusCode::CONFLICT);
    assert!(!png.1.contains_key("X-Comparison-Image"));
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
    sqlx::query("UPDATE repositories SET installation_id='7' WHERE organization_id=$1")
        .bind(f.organization)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(f.organization)
        .bind(f.actor.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&f.app, &f.actor, &raw, true).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn shared_body_final_authorization_discards_text_events_and_pixels_after_processing(
    pool: PgPool,
) {
    for (suffix, media, boundary) in [
        ("raw", false, 6),
        ("readable", false, 6),
        ("readable/units/0?side=after", false, 6),
        ("image?side=after", true, 5),
    ] {
        let (f, _, _) = configured_options(&pool, media, Some(boundary)).await;
        let (_, snapshot) = publish(&f, 2).await;
        let (document, image, base) = files(&f, &snapshot).await;
        let file = if media { &image } else { &document };
        let response =
            image_response(&f.app, &f.actor, &body_path(&base, file, suffix), true).await;
        assert_eq!(
            response.0,
            StatusCode::NOT_FOUND,
            "{suffix}: {}",
            String::from_utf8_lossy(&response.2)
        );
        assert!(!response.1.contains_key("X-Comparison-Image"));
        assert!(!response.2.starts_with(b"\x89PNG"));
        assert!(!String::from_utf8_lossy(&response.2).contains("PRIVATE SOURCE SENTINEL"));
        assert_eq!(f.file_resolves.load(Ordering::SeqCst), boundary);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn shared_body_backoff_forged_bytes_capacity_and_mid_io_revocation_never_publish_partial_source(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, true).await;
    let (_, snapshot) = publish(&f, 2).await;
    let (document, image, base) = files(&f, &snapshot).await;
    for (file, suffix) in [(&document, "raw"), (&image, "image?side=after")] {
        let path = body_path(&base, file, suffix);
        for mode in [2, 3, 4] {
            f.provider.backoff.store(mode, Ordering::SeqCst);
            let (status, headers, body) = image_response(&f.app, &f.actor, &path, true).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(!headers.contains_key("X-Comparison-Image"));
            assert!(!String::from_utf8_lossy(&body).contains("PRIVATE SOURCE SENTINEL"));
            if mode == 2 {
                assert_eq!(headers["retry-after"], "13");
            }
        }
    }
    f.provider.backoff.store(0, Ordering::SeqCst);
    let permit = f.slots.acquire_many(2).await.unwrap();
    let busy = get(
        &f.app,
        &f.actor,
        &body_path(&base, &document, "readable"),
        true,
    )
    .await;
    assert_eq!(busy.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(busy.1["error"]["code"], "COMPARISON_BUSY");
    assert_eq!(busy.3.as_deref(), Some("2"));
    drop(permit);
    *f.provider.hook.lock().unwrap() = Some((pool.clone(), f.actor.id));
    let denied = image_response(
        &f.app,
        &f.actor,
        &body_path(&base, &image, "image?side=after"),
        true,
    )
    .await;
    assert_eq!(denied.0, StatusCode::NOT_FOUND);
    assert!(!denied.1.contains_key("X-Comparison-Image"));
    assert!(!denied.2.starts_with(b"\x89PNG"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn shared_referenced_image_http_proves_unchanged_media_from_sealed_receipts_after_closure(
    pool: PgPool,
) {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};
    let (f, _, _) = configured(&pool, true).await;
    f.provider.reused_media.store(true, Ordering::SeqCst);
    let (_, snapshot) = publish(&f, 2).await;
    let (_, image, _) = files(&f, &snapshot).await;
    assert!(
        image.is_null(),
        "Reused asset must not become a fabricated file change"
    );
    let path = format!("{}/{snapshot}/media", history_path(&f));
    let receipts = get(&f.app, &f.actor, &path, true).await;
    assert_eq!(receipts.1["total"], 1);
    let receipt = &receipts.1["receipts"][0];
    let filename = receipt["path"]
        .as_str()
        .unwrap()
        .strip_prefix("assets/")
        .unwrap();
    sqlx::query("UPDATE proposals SET state='CLOSED',head_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .bind("4".repeat(40))
        .execute(&pool)
        .await
        .unwrap();
    let start = f.provider.reads.load(Ordering::SeqCst);
    let (status, headers, bytes) =
        image_response(&f.app, &f.actor, &format!("{path}/{filename}/image"), true).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(headers["content-type"], "image/png");
    assert!(!headers.contains_key("X-Comparison-Image"));
    let evidence: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(headers["X-Review-Media"].as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(evidence["asset_path"], receipt["path"]);
    assert_eq!(
        evidence["media_validation_version"],
        specspace_document_engine::media::VALIDATION_VERSION
    );
    let image = &evidence["image"];
    assert_eq!(image["actor_id"], f.actor.id.to_string());
    assert_eq!(image["snapshot_id"], snapshot);
    assert_eq!(image["head_sha"], HEAD);
    assert_eq!(image["merge_base_sha"], OLD);
    assert_eq!(image["side"], "after");
    assert_eq!(image["source_blob_oid"], receipt["blob_oid"]);
    assert_eq!(image["source_sha256"], receipt["source_sha256"]);
    assert_eq!(image["display_sha256"], receipt["display_sha256"]);
    assert_eq!(
        image["display_sha256"],
        format!("{:x}", Sha256::digest(&bytes))
    );
    assert_eq!(
        image["file_id"],
        format!(
            "{:x}",
            Sha256::digest(format!(
                "specspace-review-media-v1\0{}",
                receipt["path"].as_str().unwrap()
            ))
        )
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), start + 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn shared_media_http_requires_captured_membership_sealed_path_and_exact_snapshot_without_ref_queries(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, true).await;
    let (_, snapshot) = publish(&f, 2).await;
    let filename = f
        .provider
        .images
        .keys()
        .next()
        .unwrap()
        .strip_prefix("assets/")
        .unwrap();
    let path = format!("{}/{snapshot}/media/{filename}/image", history_path(&f));
    let start = f.provider.reads.load(Ordering::SeqCst);
    assert_eq!(
        image_response(&f.app, &f.actor, &path, false).await.0,
        StatusCode::CONFLICT
    );
    let foreign = fixture(&pool, true).await;
    assert_eq!(
        image_response(&f.app, &foreign.owner, &path, true).await.0,
        StatusCode::NOT_FOUND
    );
    for query in [
        "side=before",
        "snapshot=other",
        "commit=main",
        "ref=main",
        "path=docs/a.md",
        "side=after&side=after",
    ] {
        let response = image_response(&f.app, &f.actor, &format!("{path}?{query}"), true).await;
        assert_eq!(response.0, StatusCode::BAD_REQUEST);
        assert!(!response.1.contains_key("X-Review-Media"));
    }
    for bad in [
        path.replace(&snapshot, &Uuid::new_v4().to_string()),
        path.replace(filename, "bad.svg"),
        path.replace(filename, &format!("{}.png", "0".repeat(64))),
    ] {
        let response = image_response(&f.app, &f.actor, &bad, true).await;
        assert_eq!(response.0, StatusCode::NOT_FOUND);
        assert!(!response.1.contains_key("X-Review-Media"));
    }
    sqlx::query("UPDATE repositories SET installation_id='99' WHERE organization_id=$1")
        .bind(f.organization)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        image_response(&f.app, &f.actor, &path, true).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), start);
}

#[sqlx::test(migrations = "../../migrations")]
async fn shared_media_http_preserves_backoff_and_discards_forged_missing_or_revoked_pixels(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, true).await;
    let (_, snapshot) = publish(&f, 2).await;
    let filename = f
        .provider
        .images
        .keys()
        .next()
        .unwrap()
        .strip_prefix("assets/")
        .unwrap();
    let path = format!("{}/{snapshot}/media/{filename}/image", history_path(&f));
    for mode in [2, 3, 4] {
        f.provider.backoff.store(mode, Ordering::SeqCst);
        let (status, headers, body) = image_response(&f.app, &f.actor, &path, true).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(!headers.contains_key("X-Review-Media"));
        assert!(!body.starts_with(b"\x89PNG"));
        if mode == 2 {
            assert_eq!(headers["retry-after"], "13");
        }
    }
    f.provider.backoff.store(0, Ordering::SeqCst);
    *f.provider.hook.lock().unwrap() = Some((pool.clone(), f.actor.id));
    let (status, headers, body) = image_response(&f.app, &f.actor, &path, true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!headers.contains_key("X-Review-Media"));
    assert!(!body.starts_with(b"\x89PNG"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn before_media_http_binds_document_reference_base_source_and_png_after_closure(
    pool: PgPool,
) {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};
    let (f, _, _) = configured(&pool, true).await;
    f.provider.reused_media.store(true, Ordering::SeqCst);
    f.provider.before_media.store(true, Ordering::SeqCst);
    let (_, snapshot) = publish(&f, 2).await;
    let (document, image, base) = files(&f, &snapshot).await;
    assert!(image.is_null());
    let filename = f
        .provider
        .images
        .keys()
        .next()
        .unwrap()
        .strip_prefix("assets/")
        .unwrap();
    sqlx::query("UPDATE proposals SET state='CLOSED',head_sha=$2,target_sha=$2,version=version+1 WHERE id=$1").bind(f.proposal).bind("4".repeat(40)).execute(&pool).await.unwrap();
    let path = body_path(&base, &document, &format!("before-media/{filename}/image"));
    let (status, headers, bytes) = image_response(&f.app, &f.actor, &path, true).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(!headers.contains_key("X-Review-Media"));
    assert!(!headers.contains_key("X-Comparison-Image"));
    let proof: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(headers["X-Review-Before-Media"].as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(proof["asset_path"], format!("assets/{filename}"));
    assert_eq!(
        proof["media_validation_version"],
        specspace_document_engine::media::VALIDATION_VERSION
    );
    assert_eq!(proof["document_path"], document["before"]["path"]);
    assert_eq!(proof["document_blob_oid"], document["before"]["blob_oid"]);
    assert_eq!(proof["document_sha256"], document["before"]["sha256"]);
    let image = &proof["image"];
    assert_eq!(image["file_id"], document["id"]);
    assert_eq!(image["actor_id"], f.actor.id.to_string());
    assert_eq!(image["snapshot_id"], snapshot);
    assert_eq!(image["side"], "before");
    assert_eq!(image["head_sha"], HEAD);
    assert_eq!(image["merge_base_sha"], OLD);
    assert_eq!(image["source_sha256"], &filename[..64]);
    assert_eq!(
        image["display_sha256"],
        format!("{:x}", Sha256::digest(&bytes))
    );
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(headers["content-type"], "image/png");
}

#[sqlx::test(migrations = "../../migrations")]
async fn before_media_http_rejects_missing_before_reference_queries_and_foreign_scope_without_media_io(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, true).await;
    f.provider.reused_media.store(true, Ordering::SeqCst);
    let (_, snapshot) = publish(&f, 2).await;
    let (document, _, base) = files(&f, &snapshot).await;
    let filename = f
        .provider
        .images
        .keys()
        .next()
        .unwrap()
        .strip_prefix("assets/")
        .unwrap();
    let path = body_path(&base, &document, &format!("before-media/{filename}/image"));
    assert_eq!(
        image_response(&f.app, &f.actor, &path, false).await.0,
        StatusCode::CONFLICT
    );
    let foreign = fixture(&pool, true).await;
    assert_eq!(
        image_response(&f.app, &foreign.owner, &path, true).await.0,
        StatusCode::NOT_FOUND
    );
    for query in [
        "side=after",
        "ref=main",
        "snapshot=other",
        "commit=main",
        "path=assets/other.png",
    ] {
        assert_eq!(
            image_response(&f.app, &f.actor, &format!("{path}?{query}"), true)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        get(&f.app, &f.actor, &body_path(&base, &document, "raw"), true)
            .await
            .0,
        StatusCode::OK
    );
    let reads = f.provider.reads.load(Ordering::SeqCst);
    let (status, headers, bytes) = image_response(&f.app, &f.actor, &path, true).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["error"]["code"],
        "BEFORE_MEDIA_REFERENCE_NOT_FOUND"
    );
    assert!(!headers.contains_key("X-Review-Before-Media"));
    assert!(!bytes.starts_with(b"\x89PNG"));
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn before_media_http_final_authorization_and_mid_binary_revocation_never_emit_pixels(
    pool: PgPool,
) {
    let (f, _, _) = configured_options(&pool, true, Some(12)).await;
    f.provider.reused_media.store(true, Ordering::SeqCst);
    f.provider.before_media.store(true, Ordering::SeqCst);
    let (_, snapshot) = publish(&f, 2).await;
    let (document, _, base) = files(&f, &snapshot).await;
    let filename = f
        .provider
        .images
        .keys()
        .next()
        .unwrap()
        .strip_prefix("assets/")
        .unwrap();
    let path = body_path(&base, &document, &format!("before-media/{filename}/image"));
    let (status, headers, bytes) = image_response(&f.app, &f.actor, &path, true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!headers.contains_key("X-Review-Before-Media"));
    assert!(!bytes.starts_with(b"\x89PNG"));
    assert_eq!(f.file_resolves.load(Ordering::SeqCst), 12);
    let (f, _, _) = configured(&pool, true).await;
    f.provider.reused_media.store(true, Ordering::SeqCst);
    f.provider.before_media.store(true, Ordering::SeqCst);
    let (_, snapshot) = publish(&f, 2).await;
    let (document, _, base) = files(&f, &snapshot).await;
    let filename = f
        .provider
        .images
        .keys()
        .next()
        .unwrap()
        .strip_prefix("assets/")
        .unwrap();
    assert_eq!(
        get(&f.app, &f.actor, &body_path(&base, &document, "raw"), true)
            .await
            .0,
        StatusCode::OK
    );
    *f.provider.hook.lock().unwrap() = Some((pool.clone(), f.actor.id));
    let path = body_path(&base, &document, &format!("before-media/{filename}/image"));
    let (status, headers, bytes) = image_response(&f.app, &f.actor, &path, true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!headers.contains_key("X-Review-Before-Media"));
    assert!(!bytes.starts_with(b"\x89PNG"));
}
