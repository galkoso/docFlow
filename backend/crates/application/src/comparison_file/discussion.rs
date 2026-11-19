//! Anchors from verified immutable file evidence. Read authorization here does
//! not grant comment permission: the mutation service must separately check
//! current discussion policy before source I/O and in its final transaction.
use super::*;
use serde::{Deserialize, Serialize};
use specspace_document_engine::discussion_anchor::{AnchorError, MAX_SELECTED_LINES, TextAnchor};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorSide {
    Before,
    After,
}

/// A browser selects identities and bounded lines, never a path, Git ref, source
/// body, quote, digest or byte offset. The document identity comes from the
/// verified manifest and is checked again against actual source front matter.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TextSelection {
    Document {},
    Section { section_id: String },
    Lines { start_line: usize, end_line: usize },
}

#[derive(Debug, PartialEq, Eq)]
pub enum AnchorReadError {
    Source(FileReadError),
    Selection(AnchorError),
}

impl From<FileReadError> for AnchorReadError {
    fn from(error: FileReadError) -> Self {
        Self::Source(error)
    }
}

impl From<AnchorError> for AnchorReadError {
    fn from(error: AnchorError) -> Self {
        Self::Selection(error)
    }
}

/// No Deserialize/Serialize/Debug: this proof retains its complete server-owned
/// scope. Persistence may serialize the text evidence only after checking the
/// actor, expected snapshot and current write permission in its transaction.
pub struct VerifiedFileAnchor {
    context: FileReadContext,
    side: AnchorSide,
    source: Side,
    text: TextAnchor,
}

impl VerifiedFileAnchor {
    pub fn context(&self) -> &FileReadContext {
        &self.context
    }
    pub fn side(&self) -> AnchorSide {
        self.side
    }
    pub fn source(&self) -> &Side {
        &self.source
    }
    pub fn text(&self) -> &TextAnchor {
        &self.text
    }
    pub fn commit(&self) -> &CommitSha {
        match self.side {
            AnchorSide::Before => &self.context.merge_base,
            AnchorSide::After => &self.context.head,
        }
    }
}

impl ComparisonFileReader {
    pub async fn load_discussion_anchor(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
        side: AnchorSide,
        selection: TextSelection,
    ) -> Result<VerifiedFileAnchor, AnchorReadError> {
        selection.validate()?;
        tokio::time::timeout(self.timeout, async {
            let loaded = self.load(actor, proposal, snapshot, file).await?;
            let permit = self
                .loads
                .clone()
                .try_acquire_owned()
                .map_err(|_| FileReadError::Busy)?;
            self.recheck(actor, &loaded).await?;
            let anchor = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                prepare(loaded, side, selection)
            })
            .await
            .map_err(|_| FileReadError::Unavailable)??;
            self.recheck_discussion_anchor(actor, &anchor).await?;
            Ok(anchor)
        })
        .await
        .map_err(|_| FileReadError::TimedOut)?
    }

    /// A recheck is required after any later await before handing the proof to
    /// the final mutation transaction. That transaction must lock/recheck again;
    /// this method alone cannot close a database write race.
    pub async fn recheck_discussion_anchor(
        &self,
        actor: Uuid,
        anchor: &VerifiedFileAnchor,
    ) -> Result<(), FileReadError> {
        self.check(actor, &anchor.context).await
    }
}

impl TextSelection {
    pub fn validate(&self) -> Result<(), AnchorError> {
        match self {
            TextSelection::Section { section_id }
                if !crate::draft::valid_document_id(section_id) =>
            {
                Err(AnchorError::SectionNotFound)
            }
            TextSelection::Lines {
                start_line,
                end_line,
            } if *start_line == 0
                || end_line < start_line
                || end_line - start_line >= MAX_SELECTED_LINES =>
            {
                Err(AnchorError::InvalidLines)
            }
            _ => Ok(()),
        }
    }
}

fn prepare(
    loaded: LoadedFile,
    side: AnchorSide,
    selection: TextSelection,
) -> Result<VerifiedFileAnchor, AnchorReadError> {
    let (source, metadata) = match side {
        AnchorSide::Before => (loaded.before(), loaded.change().before.as_ref()),
        AnchorSide::After => (loaded.after(), loaded.change().after.as_ref()),
    };
    let source = source.ok_or(FileReadError::NotFound)?;
    let metadata = metadata.ok_or(FileReadError::InvalidEvidence)?;
    let document_id = || {
        if loaded.change().kind != Kind::Document {
            return Err(FileReadError::UnsupportedKind);
        }
        metadata
            .document_id
            .as_deref()
            .ok_or(FileReadError::InvalidEvidence)
    };
    let text = match selection {
        TextSelection::Document {} => TextAnchor::document(source, document_id()?)?,
        TextSelection::Section { section_id } => {
            TextAnchor::section(source, document_id()?, &section_id)?
        }
        TextSelection::Lines {
            start_line,
            end_line,
        } => TextAnchor::lines(source, start_line, end_line)?,
    };
    let source = metadata.clone();
    Ok(VerifiedFileAnchor {
        context: loaded.context,
        side,
        source,
        text,
    })
}
