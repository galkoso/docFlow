use std::fmt;
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError(pub &'static str);

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ValidationError {}

/// Proposal refs are immutable and always live under change/.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BranchName(String);

impl BranchName {
    pub fn parse(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        let invalid = !value.starts_with("change/")
            || value.len() > 100
            || !value.is_ascii()
            || value
                .bytes()
                .any(|b| b <= b' ' || b == 127 || b"~^:?*[\\".contains(&b))
            || ["..", "@{", "//"].iter().any(|part| value.contains(part))
            || value.ends_with('.')
            || value
                .split('/')
                .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"));
        if invalid {
            return Err(ValidationError("INVALID_BRANCH"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Used for the database uniqueness key, including closed proposals.
    pub fn uniqueness_key(&self) -> String {
        self.0.to_ascii_lowercase()
    }
}

/// A canonical repository path, never an absolute filesystem path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RepositoryPath(String);

impl RepositoryPath {
    pub fn parse(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        let parts: Vec<_> = value.split('/').collect();
        if value.is_empty()
            || value.nfc().ne(value.chars())
            || value.chars().count() > 240
            || parts.len() > 10
            || value
                .chars()
                .any(|c| c.is_control() || c == '\\' || c == ':')
            || parts
                .iter()
                .any(|part| part.is_empty() || *part == "." || *part == "..")
            || parts.iter().any(|part| part.eq_ignore_ascii_case(".git"))
        {
            return Err(ValidationError("PATH_FORBIDDEN"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_and_reserved_refs() {
        for value in [
            "main",
            "HEAD",
            "refs/heads/main",
            "change/",
            "change/a b",
            "change/a..b",
            "change/a@{b",
            "change/a//b",
            "change/a\\b",
            "change/.hidden",
            "change/a.lock",
            "change/a.lock/b",
            "change/a/",
            "change/a.",
            "change/שלום",
            "change/a~1",
            "change/a^b",
            "change/a:b",
            "change/a?b",
            "change/a*b",
            "change/a[b",
            "change/a\n",
        ] {
            assert!(BranchName::parse(value).is_err(), "accepted {value:?}");
        }
        assert!(BranchName::parse(format!("change/{}", "a".repeat(94))).is_err());
    }

    #[test]
    fn accepts_nested_refs_and_case_insensitive_reservations() {
        let branch = BranchName::parse("change/25-Bulk_delete/subtask").unwrap();
        assert_eq!(branch.as_str(), "change/25-Bulk_delete/subtask");
        assert_eq!(branch.uniqueness_key(), "change/25-bulk_delete/subtask");
    }

    #[test]
    fn rejects_path_traversal_and_git_metadata() {
        for path in [
            "",
            "/docs/a.md",
            "../a",
            "docs/../a",
            "docs/./a",
            "docs//a",
            "C:/a",
            "docs\\a",
            ".git/config",
            "docs/.GIT/config",
            "docs/a\0",
        ] {
            assert!(RepositoryPath::parse(path).is_err(), "accepted {path:?}");
        }
        assert!(RepositoryPath::parse("a/".repeat(10) + "b").is_err());
        assert!(RepositoryPath::parse("a".repeat(241)).is_err());
        assert!(RepositoryPath::parse("docs/cafe\u{301}.md").is_err());
        assert_eq!(
            RepositoryPath::parse("docs/הרשאות.md").unwrap().as_str(),
            "docs/הרשאות.md"
        );
    }
}
