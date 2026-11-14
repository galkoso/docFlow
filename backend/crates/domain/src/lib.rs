//! Pure domain rules shared by API and worker. No provider or HTTP dependencies.
mod identifiers;

pub use identifiers::{BranchName, RepositoryPath, ValidationError};
