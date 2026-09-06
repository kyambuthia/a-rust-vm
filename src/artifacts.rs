//! Immutable, workspace-owned artifacts and artifact versions.
//!
//! This module owns the in-memory domain contract only. Content bytes remain
//! outside the store behind a validated content reference so a future durable
//! control plane can choose an object store without changing version semantics.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// Whether an artifact originated with a workspace user or a job runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Source,
    Generated,
}

/// A stable, user-visible file or output inside one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub kind: ArtifactKind,
    pub created_by: String,
    pub current_version_id: Option<String>,
    pub version_ids: Vec<String>,
}

/// One immutable revision of an [`Artifact`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVersion {
    pub id: String,
    pub artifact_id: String,
    pub parent_version_id: Option<String>,
    pub input_version_ids: Vec<String>,
    pub content_reference: String,
    pub media_type: String,
    pub byte_len: u64,
    pub created_by: String,
    pub sequence: u64,
}

/// Input used to create a logical artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewArtifact {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub kind: ArtifactKind,
    pub created_by: String,
}

impl NewArtifact {
    pub fn new(
        id: impl Into<String>,
        workspace_id: impl Into<String>,
        name: impl Into<String>,
        kind: ArtifactKind,
        created_by: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            workspace_id: workspace_id.into(),
            name: name.into(),
            kind,
            created_by: created_by.into(),
        }
    }
}

/// Input used to append an immutable version to an existing artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewArtifactVersion {
    pub id: String,
    pub artifact_id: String,
    pub parent_version_id: Option<String>,
    pub input_version_ids: Vec<String>,
    pub content_reference: String,
    pub media_type: String,
    pub byte_len: u64,
    pub created_by: String,
}

impl NewArtifactVersion {
    pub fn new(
        id: impl Into<String>,
        artifact_id: impl Into<String>,
        content_reference: impl Into<String>,
        media_type: impl Into<String>,
        byte_len: u64,
        created_by: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            artifact_id: artifact_id.into(),
            parent_version_id: None,
            input_version_ids: Vec::new(),
            content_reference: content_reference.into(),
            media_type: media_type.into(),
            byte_len,
            created_by: created_by.into(),
        }
    }

    pub fn with_parent(mut self, parent_version_id: impl Into<String>) -> Self {
        self.parent_version_id = Some(parent_version_id.into());
        self
    }

    pub fn with_inputs<I, S>(mut self, input_version_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.input_version_ids = input_version_ids.into_iter().map(Into::into).collect();
        self
    }
}

/// In-memory artifact metadata store.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ArtifactStore {
    artifacts: BTreeMap<String, Artifact>,
    versions: BTreeMap<String, ArtifactVersion>,
    next_sequence: u64,
}

impl ArtifactStore {
    pub fn create_artifact(&mut self, request: NewArtifact) -> Result<Artifact, ArtifactError> {
        validate_id(&request.id, "artifact id")?;
        validate_id(&request.workspace_id, "workspace id")?;
        validate_text(&request.name, "artifact name")?;
        validate_text(&request.created_by, "artifact creator")?;
        if self.artifacts.contains_key(&request.id) {
            return Err(ArtifactError::ArtifactAlreadyExists(request.id));
        }

        let artifact = Artifact {
            id: request.id.clone(),
            workspace_id: request.workspace_id,
            name: request.name,
            kind: request.kind,
            created_by: request.created_by,
            current_version_id: None,
            version_ids: Vec::new(),
        };
        self.artifacts.insert(request.id, artifact.clone());
        Ok(artifact)
    }

    pub fn create_version(
        &mut self,
        request: NewArtifactVersion,
    ) -> Result<ArtifactVersion, ArtifactError> {
        validate_id(&request.id, "artifact version id")?;
        validate_id(&request.artifact_id, "artifact id")?;
        validate_text(&request.content_reference, "content reference")?;
        validate_media_type(&request.media_type)?;
        validate_text(&request.created_by, "artifact version creator")?;

        if self.versions.contains_key(&request.id) {
            return Err(ArtifactError::VersionAlreadyExists(request.id));
        }
        if !self.artifacts.contains_key(&request.artifact_id) {
            return Err(ArtifactError::ArtifactNotFound(request.artifact_id));
        }

        if let Some(parent_id) = &request.parent_version_id {
            validate_id(parent_id, "parent version id")?;
            let parent = self
                .versions
                .get(parent_id)
                .ok_or_else(|| ArtifactError::VersionNotFound(parent_id.clone()))?;
            if parent.artifact_id != request.artifact_id {
                return Err(ArtifactError::ParentBelongsToDifferentArtifact {
                    parent_version_id: parent_id.clone(),
                    artifact_id: request.artifact_id,
                });
            }
        }

        let mut unique_inputs = BTreeSet::new();
        for input_id in &request.input_version_ids {
            validate_id(input_id, "input version id")?;
            if !unique_inputs.insert(input_id) {
                return Err(ArtifactError::DuplicateInputVersion(input_id.clone()));
            }
            if !self.versions.contains_key(input_id) {
                return Err(ArtifactError::VersionNotFound(input_id.clone()));
            }
        }

        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(ArtifactError::SequenceExhausted)?;
        let version = ArtifactVersion {
            id: request.id.clone(),
            artifact_id: request.artifact_id.clone(),
            parent_version_id: request.parent_version_id,
            input_version_ids: request.input_version_ids,
            content_reference: request.content_reference,
            media_type: request.media_type,
            byte_len: request.byte_len,
            created_by: request.created_by,
            sequence: self.next_sequence,
        };

        self.versions.insert(request.id, version.clone());
        let artifact = self
            .artifacts
            .get_mut(&request.artifact_id)
            .expect("artifact existence checked before version creation");
        artifact.current_version_id = Some(version.id.clone());
        artifact.version_ids.push(version.id.clone());
        Ok(version)
    }

    pub fn artifact(&self, id: &str) -> Option<&Artifact> {
        self.artifacts.get(id)
    }

    pub fn version(&self, id: &str) -> Option<&ArtifactVersion> {
        self.versions.get(id)
    }

    pub fn current_version(&self, artifact_id: &str) -> Option<&ArtifactVersion> {
        let version_id = self.artifact(artifact_id)?.current_version_id.as_deref()?;
        self.version(version_id)
    }

    pub fn version_history(&self, artifact_id: &str) -> Vec<&ArtifactVersion> {
        self.artifact(artifact_id)
            .map(|artifact| {
                artifact
                    .version_ids
                    .iter()
                    .filter_map(|id| self.version(id))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Errors raised when artifact metadata would violate the workspace contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactError {
    InvalidInput {
        field: &'static str,
    },
    InvalidMediaType,
    ArtifactAlreadyExists(String),
    ArtifactNotFound(String),
    VersionAlreadyExists(String),
    VersionNotFound(String),
    ParentBelongsToDifferentArtifact {
        parent_version_id: String,
        artifact_id: String,
    },
    DuplicateInputVersion(String),
    SequenceExhausted,
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { field } => write!(formatter, "invalid {field}"),
            Self::InvalidMediaType => formatter.write_str("invalid media type"),
            Self::ArtifactAlreadyExists(id) => write!(formatter, "artifact already exists: {id}"),
            Self::ArtifactNotFound(id) => write!(formatter, "artifact not found: {id}"),
            Self::VersionAlreadyExists(id) => {
                write!(formatter, "artifact version already exists: {id}")
            }
            Self::VersionNotFound(id) => write!(formatter, "artifact version not found: {id}"),
            Self::ParentBelongsToDifferentArtifact {
                parent_version_id,
                artifact_id,
            } => write!(
                formatter,
                "parent version '{parent_version_id}' does not belong to artifact '{artifact_id}'"
            ),
            Self::DuplicateInputVersion(id) => write!(formatter, "duplicate input version: {id}"),
            Self::SequenceExhausted => formatter.write_str("artifact version sequence exhausted"),
        }
    }
}

impl std::error::Error for ArtifactError {}

fn validate_id(value: &str, field: &'static str) -> Result<(), ArtifactError> {
    if value.is_empty()
        || value.chars().any(|character| {
            !character.is_ascii_alphanumeric() && !matches!(character, '-' | '_' | '.' | ':')
        })
    {
        return Err(ArtifactError::InvalidInput { field });
    }
    Ok(())
}

fn validate_text(value: &str, field: &'static str) -> Result<(), ArtifactError> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(ArtifactError::InvalidInput { field });
    }
    Ok(())
}

fn validate_media_type(value: &str) -> Result<(), ArtifactError> {
    let Some((kind, subtype)) = value.split_once('/') else {
        return Err(ArtifactError::InvalidMediaType);
    };
    if kind.is_empty()
        || subtype.is_empty()
        || subtype.contains('/')
        || !kind.chars().all(is_media_token_character)
        || !subtype.chars().all(is_media_token_character)
    {
        return Err(ArtifactError::InvalidMediaType);
    }
    Ok(())
}

fn is_media_token_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            '!' | '#' | '$' | '&' | '^' | '_' | '.' | '+' | '-'
        )
}

#[cfg(test)]
mod tests {
    use super::{ArtifactError, ArtifactKind, ArtifactStore, NewArtifact, NewArtifactVersion};

    fn source_artifact(id: &str) -> NewArtifact {
        NewArtifact::new(
            id,
            "workspace-1",
            "budget.xlsx",
            ArtifactKind::Source,
            "user-1",
        )
    }

    #[test]
    fn versions_are_immutable_and_current_version_advances() {
        let mut store = ArtifactStore::default();
        store
            .create_artifact(source_artifact("artifact-1"))
            .unwrap();

        let initial = store
            .create_version(NewArtifactVersion::new(
                "version-1",
                "artifact-1",
                "blob:budget-v1",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                120,
                "user-1",
            ))
            .unwrap();
        let successor = store
            .create_version(
                NewArtifactVersion::new(
                    "version-2",
                    "artifact-1",
                    "blob:budget-v2",
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    144,
                    "agent-1",
                )
                .with_parent("version-1")
                .with_inputs(["version-1"]),
            )
            .unwrap();

        assert_eq!(initial.sequence, 1);
        assert_eq!(successor.sequence, 2);
        assert_eq!(store.version("version-1").unwrap(), &initial);
        assert_eq!(store.current_version("artifact-1").unwrap(), &successor);
        assert_eq!(
            store
                .version_history("artifact-1")
                .into_iter()
                .map(|version| version.id.as_str())
                .collect::<Vec<_>>(),
            vec!["version-1", "version-2"]
        );
    }

    #[test]
    fn rejects_invalid_workspace_and_media_type() {
        let mut store = ArtifactStore::default();
        let error = store
            .create_artifact(NewArtifact::new(
                "artifact-1",
                "",
                "budget.xlsx",
                ArtifactKind::Source,
                "user-1",
            ))
            .unwrap_err();
        assert_eq!(
            error,
            ArtifactError::InvalidInput {
                field: "workspace id"
            }
        );

        store
            .create_artifact(source_artifact("artifact-1"))
            .unwrap();
        let error = store
            .create_version(NewArtifactVersion::new(
                "version-1",
                "artifact-1",
                "blob:budget-v1",
                "not a mime type",
                120,
                "user-1",
            ))
            .unwrap_err();
        assert_eq!(error, ArtifactError::InvalidMediaType);
    }

    #[test]
    fn rejects_missing_or_cross_artifact_parent_versions() {
        let mut store = ArtifactStore::default();
        store
            .create_artifact(source_artifact("artifact-1"))
            .unwrap();
        store
            .create_artifact(NewArtifact::new(
                "artifact-2",
                "workspace-1",
                "summary.pdf",
                ArtifactKind::Generated,
                "agent-1",
            ))
            .unwrap();
        store
            .create_version(NewArtifactVersion::new(
                "version-1",
                "artifact-1",
                "blob:budget-v1",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                120,
                "user-1",
            ))
            .unwrap();

        let missing_parent = store
            .create_version(
                NewArtifactVersion::new(
                    "version-2",
                    "artifact-1",
                    "blob:budget-v2",
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    144,
                    "user-1",
                )
                .with_parent("missing-version"),
            )
            .unwrap_err();
        assert_eq!(
            missing_parent,
            ArtifactError::VersionNotFound("missing-version".to_owned())
        );

        let cross_artifact_parent = store
            .create_version(
                NewArtifactVersion::new(
                    "version-3",
                    "artifact-2",
                    "blob:summary-v1",
                    "application/pdf",
                    32,
                    "agent-1",
                )
                .with_parent("version-1"),
            )
            .unwrap_err();
        assert_eq!(
            cross_artifact_parent,
            ArtifactError::ParentBelongsToDifferentArtifact {
                parent_version_id: "version-1".to_owned(),
                artifact_id: "artifact-2".to_owned(),
            }
        );
    }
}
