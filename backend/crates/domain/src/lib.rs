//! Pure domain rules shared by API and worker. No provider or HTTP dependencies.
mod identifiers;
pub mod policy;

pub use identifiers::{BranchName, RepositoryPath, ValidationError};
