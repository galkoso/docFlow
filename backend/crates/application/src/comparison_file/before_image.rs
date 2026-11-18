//! Referenced before-media is anchored to verified document source, then exact
//! base-tree metadata. It never borrows an after-side receipt or selects refs.
use super::image::{ImageSide, LoadedImage, prepare_image};
use super::*;
use specspace_document_engine::{diff::readable, media, render::RenderKind};
use specspace_git_adapter::{FileEntry, FileMode};

pub struct LoadedBeforeImage {
    image: LoadedImage,
    source: Side,
}
impl LoadedBeforeImage {
    /// The image's authorization context remains the original before-document.
    pub fn image(&self) -> &LoadedImage {
        &self.image
    }
    pub fn source(&self) -> &Side {
        &self.source
    }
    pub fn into_image(self) -> LoadedImage {
        self.image
    }
}
impl ComparisonFileReader {
    pub async fn load_before_image(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
        path: &str,
    ) -> Result<LoadedBeforeImage, FileReadError> {
        if !media::valid_path(path) {
            return Err(FileReadError::InvalidImage("INVALID_ASSET_PATH"));
        }
        tokio::time::timeout(
            self.timeout,
            self.before_image_inner(actor, proposal, snapshot, file, path.to_owned()),
        )
        .await
        .map_err(|_| FileReadError::TimedOut)?
    }
    async fn before_image_inner(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
        path: String,
    ) -> Result<LoadedBeforeImage, FileReadError> {
        let loaded = self.load(actor, proposal, snapshot, file).await?;
        if loaded.change().kind != Kind::Document {
            return Err(FileReadError::UnsupportedKind);
        }
        if loaded.before().is_none() {
            return Err(FileReadError::InvalidImage("BEFORE_MEDIA_SIDE_ABSENT"));
        }
        let permit = self
            .loads
            .clone()
            .try_acquire_owned()
            .map_err(|_| FileReadError::Busy)?;
        let (loaded, path, permit) = tokio::task::spawn_blocking(move || {
            let document = readable::prepare(loaded.before().ok_or(FileReadError::NotFound)?)
                .map_err(|_| FileReadError::InvalidImage("BEFORE_MEDIA_REFERENCE_UNAVAILABLE"))?;
            let path = document
                .units
                .iter()
                .filter(|unit| unit.protected_reason.is_none())
                .flat_map(|unit| &unit.events)
                .find_map(|event| {
                    (event.kind == RenderKind::Image && event.destination.as_deref() == Some(&path))
                        .then(|| event.destination.clone())
                        .flatten()
                })
                .ok_or(FileReadError::InvalidImage(
                    "BEFORE_MEDIA_REFERENCE_NOT_FOUND",
                ))?;
            Ok::<_, FileReadError>((loaded, path, permit))
        })
        .await
        .map_err(|_| FileReadError::Unavailable)??;
        self.recheck(actor, &loaded).await?;
        let provider = self
            .source
            .provider_bound(&loaded.context.scope.connection)
            .await
            .map_err(FileReadError::Access)?;
        self.recheck(actor, &loaded).await?;
        let tree = provider
            .list_files(loaded.merge_base())
            .await
            .map_err(FileReadError::Provider)?;
        self.recheck(actor, &loaded).await?;
        let (loaded, source, permit) = tokio::task::spawn_blocking(move || {
            let source = selected_source(tree, &path)?;
            Ok::<_, FileReadError>((loaded, source, permit))
        })
        .await
        .map_err(|_| FileReadError::Unavailable)??;
        self.recheck(actor, &loaded).await?;
        let bytes = provider
            .read_bytes(
                loaded.merge_base(),
                &RepositoryPath::parse(&source.path).map_err(|_| FileReadError::InvalidEvidence)?,
                media::MAX_ASSET_BYTES,
            )
            .await
            .map_err(FileReadError::Provider)?;
        if bytes.len() > media::MAX_ASSET_BYTES {
            return Err(FileReadError::TooLarge);
        }
        self.recheck(actor, &loaded).await?;
        let (image, _permit) = tokio::task::spawn_blocking(move || {
            let image = prepare_image(loaded.context, ImageSide::Before, source.clone(), bytes)?;
            Ok::<_, FileReadError>((LoadedBeforeImage { image, source }, permit))
        })
        .await
        .map_err(|_| FileReadError::Unavailable)??;
        self.recheck_image(actor, image.image()).await?;
        Ok(image)
    }
}
fn selected_source(tree: Vec<FileEntry>, path: &str) -> Result<Side, FileReadError> {
    if tree.len() > 20_000 {
        return Err(FileReadError::InvalidImage("BEFORE_MEDIA_TREE_LIMIT"));
    }
    let mut matches = tree.into_iter().filter(|entry| entry.path.as_str() == path);
    let source = matches
        .next()
        .ok_or(FileReadError::InvalidImage("BEFORE_MEDIA_SOURCE_MISSING"))?;
    if matches.next().is_some() || !matches!(source.blob_sha.as_str().len(), 40 | 64) {
        return Err(FileReadError::InvalidEvidence);
    }
    if source.mode != FileMode::Regular {
        return Err(FileReadError::InvalidImage("INVALID_ASSET_PATH"));
    }
    if source.size > media::MAX_ASSET_BYTES as u64 {
        return Err(FileReadError::TooLarge);
    }
    Ok(Side {
        path: path.to_owned(),
        blob_oid: source.blob_sha.as_str().to_owned(),
        bytes: source.size,
        executable: false,
        sha256: Some(path[7..71].to_owned()),
        document_id: None,
        catalog_position: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(mode: FileMode, size: u64) -> FileEntry {
        FileEntry {
            path: RepositoryPath::parse(format!("assets/{}.png", "a".repeat(64))).unwrap(),
            blob_sha: CommitSha::parse("b".repeat(40)).unwrap(),
            size,
            mode,
        }
    }
    #[test]
    fn base_source_requires_one_regular_bounded_entry() {
        let path = format!("assets/{}.png", "a".repeat(64));
        assert!(matches!(
            selected_source(vec![], &path),
            Err(FileReadError::InvalidImage("BEFORE_MEDIA_SOURCE_MISSING"))
        ));
        assert!(matches!(
            selected_source(
                vec![entry(FileMode::Regular, 1), entry(FileMode::Regular, 1)],
                &path
            ),
            Err(FileReadError::InvalidEvidence)
        ));
        assert!(matches!(
            selected_source(vec![entry(FileMode::Executable, 1)], &path),
            Err(FileReadError::InvalidImage("INVALID_ASSET_PATH"))
        ));
        assert!(matches!(
            selected_source(
                vec![entry(FileMode::Regular, media::MAX_ASSET_BYTES as u64 + 1)],
                &path
            ),
            Err(FileReadError::TooLarge)
        ));
        let source = selected_source(vec![entry(FileMode::Regular, 1)], &path).unwrap();
        assert_eq!(source.path, path);
        assert_eq!(source.sha256, Some("a".repeat(64)));
    }
    #[test]
    fn oversized_base_trees_fail_before_binary_reads() {
        let tree = (0..20_001).map(|_| entry(FileMode::Regular, 1)).collect();
        assert!(matches!(
            selected_source(tree, &format!("assets/{}.png", "a".repeat(64))),
            Err(FileReadError::InvalidImage("BEFORE_MEDIA_TREE_LIMIT"))
        ));
    }
}
