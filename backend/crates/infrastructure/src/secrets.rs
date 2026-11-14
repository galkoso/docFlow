use std::{io::Read, path::Path};
use zeroize::Zeroizing;

const MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum SecretError {
    InvalidReference,
    Unavailable,
    TooLarge,
}

/// No Debug/Serialize implementation: secret bytes never belong in diagnostics.
pub struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

pub fn resolve(reference: &str) -> Result<SecretBytes, SecretError> {
    resolve_with(reference, |key| std::env::var(key).ok())
}

fn resolve_with(
    reference: &str,
    environment: impl FnOnce(&str) -> Option<String>,
) -> Result<SecretBytes, SecretError> {
    let mut bytes = Zeroizing::new(Vec::new());
    if let Some(name) = reference.strip_prefix("env:") {
        if name.is_empty()
            || name.len() > 128
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(SecretError::InvalidReference);
        }
        bytes = Zeroizing::new(
            environment(name)
                .ok_or(SecretError::Unavailable)?
                .into_bytes(),
        );
    } else if let Some(path) = reference.strip_prefix("file:") {
        if !Path::new(path).is_absolute() {
            return Err(SecretError::InvalidReference);
        }
        std::fs::File::open(path)
            .map_err(|_| SecretError::Unavailable)?
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| SecretError::Unavailable)?;
    } else {
        return Err(SecretError::InvalidReference);
    }
    if bytes.len() > MAX_BYTES {
        return Err(SecretError::TooLarge);
    }
    if bytes.is_empty() {
        return Err(SecretError::Unavailable);
    }
    Ok(SecretBytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_are_explicit_bounded_and_never_fetched_from_urls() {
        assert_eq!(
            resolve_with("env:SYNTHETIC_KEY", |_| Some("synthetic-value".into()))
                .unwrap()
                .as_bytes(),
            b"synthetic-value"
        );
        for reference in [
            "https://example.com/key",
            "file:relative/key",
            "env:",
            "env:BAD=NAME",
        ] {
            assert!(matches!(
                resolve_with(reference, |_| None),
                Err(SecretError::InvalidReference)
            ));
        }
        assert!(matches!(
            resolve_with("env:SYNTHETIC_KEY", |_| Some("x".repeat(MAX_BYTES + 1))),
            Err(SecretError::TooLarge)
        ));
        let path =
            std::env::temp_dir().join(format!("specspace-secret-test-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, "synthetic-file-value").unwrap();
        let result = resolve(&format!("file:{}", path.display()));
        std::fs::remove_file(path).unwrap();
        assert_eq!(result.unwrap().as_bytes(), b"synthetic-file-value");
    }
}
