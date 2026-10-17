//! Updating Void: the signed release manifest, the release check and
//! installing a verified update.

pub mod manifest;

pub use manifest::{Manifest, ManifestAsset, VerifyError, RELEASE_KEY};
