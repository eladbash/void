//! Updating Void: the signed release manifest, the release check and
//! installing a verified update.

pub mod manifest;
pub mod release;

pub use manifest::{Manifest, ManifestAsset, VerifyError, RELEASE_KEY};
pub use release::{Check, CheckError, Feed, Package, Release, ReleaseAsset, Update};
pub use semver::Version;
