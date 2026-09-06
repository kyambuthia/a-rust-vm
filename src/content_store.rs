//! Content-addressed local storage for durable artifact bytes.
//!
//! Artifact metadata records a `sha256:<hex>` reference instead of embedding
//! bytes in the control-plane snapshot. The store verifies the digest on reads
//! so accidental corruption cannot be mistaken for the referenced artifact.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

const REFERENCE_PREFIX: &str = "sha256:";
const DIGEST_HEX_LEN: usize = 64;
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct ContentStore {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredContent {
    pub reference: String,
    pub byte_len: u64,
}

impl ContentStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store bytes once and return their stable SHA-256 content reference.
    pub fn put(&self, bytes: &[u8]) -> Result<StoredContent, ContentStoreError> {
        let digest = digest_hex(bytes);
        let path = self.path_for_digest(&digest);
        let parent = path
            .parent()
            .expect("content paths always have a sha256 directory");
        fs::create_dir_all(parent).map_err(|error| ContentStoreError::Io {
            operation: "create content directory",
            path: parent.to_path_buf(),
            message: error.to_string(),
        })?;

        if path.exists() {
            if self.read_digest(&digest)? != bytes {
                return Err(ContentStoreError::HashCollision {
                    reference: format!("{REFERENCE_PREFIX}{digest}"),
                });
            }
            return Ok(StoredContent {
                reference: format!("{REFERENCE_PREFIX}{digest}"),
                byte_len: bytes.len() as u64,
            });
        }

        let temporary_id = NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{digest}.{}.{}.tmp",
            std::process::id(),
            temporary_id
        ));
        fs::write(&temporary, bytes).map_err(|error| ContentStoreError::Io {
            operation: "write content",
            path: temporary.clone(),
            message: error.to_string(),
        })?;
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(ContentStoreError::Io {
                operation: "commit content",
                path,
                message: error.to_string(),
            });
        }

        Ok(StoredContent {
            reference: format!("{REFERENCE_PREFIX}{digest}"),
            byte_len: bytes.len() as u64,
        })
    }

    /// Read bytes for a validated content reference and verify their digest.
    pub fn get(&self, reference: &str) -> Result<Vec<u8>, ContentStoreError> {
        let digest = parse_reference(reference)?;
        self.read_digest(digest)
    }

    pub fn contains(&self, reference: &str) -> Result<bool, ContentStoreError> {
        let digest = parse_reference(reference)?;
        Ok(self.path_for_digest(digest).is_file())
    }

    fn read_digest(&self, digest: &str) -> Result<Vec<u8>, ContentStoreError> {
        let path = self.path_for_digest(digest);
        let bytes = fs::read(&path).map_err(|error| ContentStoreError::Io {
            operation: "read content",
            path: path.clone(),
            message: error.to_string(),
        })?;
        if digest_hex(&bytes) != digest {
            return Err(ContentStoreError::CorruptContent {
                reference: format!("{REFERENCE_PREFIX}{digest}"),
            });
        }
        Ok(bytes)
    }

    fn path_for_digest(&self, digest: &str) -> PathBuf {
        self.root.join("sha256").join(digest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentStoreError {
    InvalidReference,
    CorruptContent {
        reference: String,
    },
    HashCollision {
        reference: String,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for ContentStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReference => formatter.write_str("invalid content reference"),
            Self::CorruptContent { reference } => {
                write!(formatter, "content does not match reference: {reference}")
            }
            Self::HashCollision { reference } => {
                write!(formatter, "content hash collision: {reference}")
            }
            Self::Io {
                operation,
                path,
                message,
            } => write!(
                formatter,
                "cannot {operation} '{}': {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ContentStoreError {}

fn parse_reference(reference: &str) -> Result<&str, ContentStoreError> {
    let Some(digest) = reference.strip_prefix(REFERENCE_PREFIX) else {
        return Err(ContentStoreError::InvalidReference);
    };
    if digest.len() != DIGEST_HEX_LEN
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(ContentStoreError::InvalidReference);
    }
    Ok(digest)
}

fn digest_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(DIGEST_HEX_LEN);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{ContentStore, ContentStoreError};

    static NEXT_TEST_ID: AtomicUsize = AtomicUsize::new(0);

    fn test_store() -> ContentStore {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("arvm-content-store-{}-{id}", std::process::id()));
        ContentStore::new(root)
    }

    #[test]
    fn stores_content_once_and_reads_it_by_digest() {
        let store = test_store();
        let first = store.put(b"artifact bytes").unwrap();
        let second = store.put(b"artifact bytes").unwrap();

        assert_eq!(first, second);
        assert_eq!(first.byte_len, 14);
        assert!(store.contains(&first.reference).unwrap());
        assert_eq!(store.get(&first.reference).unwrap(), b"artifact bytes");
        std::fs::remove_dir_all(store.root()).unwrap();
    }

    #[test]
    fn rejects_references_that_cannot_name_a_content_object() {
        let store = test_store();
        assert_eq!(
            store.get("../../workspace").unwrap_err(),
            ContentStoreError::InvalidReference
        );
        assert_eq!(
            store.contains("sha256:ABCDEF").unwrap_err(),
            ContentStoreError::InvalidReference
        );
    }
}
