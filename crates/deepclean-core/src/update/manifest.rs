//! The signed update manifest each release carries.
//!
//! `scripts/release-manifest.sh` writes `manifest.json` (the version and every
//! asset's name, size and SHA-256) and signs it with the release key; the
//! signature goes in `manifest.json.sig` as hex. Nothing in a manifest is
//! trusted until its signature checks out against [`RELEASE_KEY`].

use std::io::Read;
use std::path::Path;

use ed25519_dalek::pkcs8::DecodePublicKey;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The release signing key's public half (SPKI PEM). Its private half lives
/// only in the repository's `VOID_RELEASE_SIGNING_KEY` secret.
pub const RELEASE_KEY: &str = include_str!("release-key.pem");

pub const MANIFEST_ASSET: &str = "manifest.json";
pub const SIGNATURE_ASSET: &str = "manifest.json.sig";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    pub assets: Vec<ManifestAsset>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ManifestAsset {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("the update's signature is malformed")]
    BadSignature,
    #[error("the update's signature does not match Void's release key")]
    WrongSignature,
    #[error("the update manifest is unreadable: {0}")]
    BadManifest(String),
    #[error("the manifest is for version {found}, not {expected}")]
    WrongVersion { expected: String, found: String },
    #[error("{0} is not listed in the signed manifest")]
    NotListed(String),
    #[error("{name} does not match its signed checksum")]
    WrongChecksum { name: String },
    #[error("{name} is {found} bytes, but the signed manifest says {expected}")]
    WrongSize {
        name: String,
        expected: u64,
        found: u64,
    },
    #[error("could not read the download: {0}")]
    Io(String),
}

impl Manifest {
    /// Parse `manifest.json` after checking its signature (hex, as in
    /// `manifest.json.sig`) against `public_key_pem`, and that it describes
    /// `expected_version` — so an old, validly signed manifest can't be
    /// replayed for a newer release.
    pub fn verify(
        manifest: &[u8],
        signature_hex: &str,
        public_key_pem: &str,
        expected_version: &str,
    ) -> Result<Self, VerifyError> {
        let key = VerifyingKey::from_public_key_pem(public_key_pem)
            .map_err(|e| VerifyError::BadManifest(format!("release key: {e}")))?;
        let sig = decode_hex(signature_hex.trim()).ok_or(VerifyError::BadSignature)?;
        let sig = Signature::from_slice(&sig).map_err(|_| VerifyError::BadSignature)?;
        key.verify_strict(manifest, &sig)
            .map_err(|_| VerifyError::WrongSignature)?;
        let parsed: Manifest = serde_json::from_slice(manifest)
            .map_err(|e| VerifyError::BadManifest(e.to_string()))?;
        if parsed.version.trim_start_matches('v') != expected_version.trim_start_matches('v') {
            return Err(VerifyError::WrongVersion {
                expected: expected_version.to_string(),
                found: parsed.version,
            });
        }
        Ok(parsed)
    }

    pub fn asset(&self, name: &str) -> Option<&ManifestAsset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// Check a downloaded file against its signed size and SHA-256.
    pub fn check_file(&self, name: &str, path: &Path) -> Result<(), VerifyError> {
        let asset = self
            .asset(name)
            .ok_or_else(|| VerifyError::NotListed(name.to_string()))?;
        let io = |e: std::io::Error| VerifyError::Io(e.to_string());
        let found = std::fs::metadata(path).map_err(io)?.len();
        if found != asset.size {
            return Err(VerifyError::WrongSize {
                name: name.to_string(),
                expected: asset.size,
                found,
            });
        }
        let mut file = std::fs::File::open(path).map_err(io)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf).map_err(io)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let digest = hex(&hasher.finalize());
        if !digest.eq_ignore_ascii_case(&asset.sha256) {
            return Err(VerifyError::WrongChecksum {
                name: name.to_string(),
            });
        }
        Ok(())
    }
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Written and signed by scripts/release-manifest.sh with the real release
    // key, so these tests pin the OpenSSL output format the app must accept.
    const FIXTURE: &[u8] = include_bytes!("testdata/manifest.json");
    const FIXTURE_SIG: &str = include_str!("testdata/manifest.json.sig");

    #[test]
    fn accepts_a_manifest_signed_by_the_release_key() {
        let m = Manifest::verify(FIXTURE, FIXTURE_SIG, RELEASE_KEY, "1.3.0").unwrap();
        assert_eq!(m.version, "1.3.0");
        assert_eq!(m.assets.len(), 2);
        assert!(m.asset("Void_1.3.0_aarch64.dmg").is_some());
    }

    #[test]
    fn accepts_a_v_prefixed_version() {
        assert!(Manifest::verify(FIXTURE, FIXTURE_SIG, RELEASE_KEY, "v1.3.0").is_ok());
    }

    #[test]
    fn rejects_a_tampered_manifest() {
        let tampered = String::from_utf8(FIXTURE.to_vec())
            .unwrap()
            .replace("00cbbd", "11cbbd");
        assert_eq!(
            Manifest::verify(tampered.as_bytes(), FIXTURE_SIG, RELEASE_KEY, "1.3.0"),
            Err(VerifyError::WrongSignature)
        );
    }

    #[test]
    fn rejects_another_key() {
        let other = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEAkjYZSdC1FSxjBzLFYhyykha4lI9aXpI9f7HH3LhZQws=\n-----END PUBLIC KEY-----\n";
        assert_eq!(
            Manifest::verify(FIXTURE, FIXTURE_SIG, other, "1.3.0"),
            Err(VerifyError::WrongSignature)
        );
    }

    #[test]
    fn rejects_malformed_signatures() {
        for sig in ["", "zz", "abcd", &"0".repeat(127)] {
            assert_eq!(
                Manifest::verify(FIXTURE, sig, RELEASE_KEY, "1.3.0"),
                Err(VerifyError::BadSignature),
                "{sig:?}"
            );
        }
    }

    #[test]
    fn rejects_a_replayed_manifest_for_another_version() {
        assert_eq!(
            Manifest::verify(FIXTURE, FIXTURE_SIG, RELEASE_KEY, "1.4.0"),
            Err(VerifyError::WrongVersion {
                expected: "1.4.0".into(),
                found: "1.3.0".into()
            })
        );
    }

    #[test]
    fn checks_downloads_against_their_signed_hash_and_size() {
        let m = Manifest::verify(FIXTURE, FIXTURE_SIG, RELEASE_KEY, "1.3.0").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good");
        std::fs::write(&good, "dmg").unwrap();
        assert_eq!(m.check_file("Void_1.3.0_aarch64.dmg", &good), Ok(()));

        let bad = dir.path().join("bad");
        std::fs::write(&bad, "DMG").unwrap();
        assert_eq!(
            m.check_file("Void_1.3.0_aarch64.dmg", &bad),
            Err(VerifyError::WrongChecksum {
                name: "Void_1.3.0_aarch64.dmg".into()
            })
        );

        let short = dir.path().join("short");
        std::fs::write(&short, "dm").unwrap();
        assert!(matches!(
            m.check_file("Void_1.3.0_aarch64.dmg", &short),
            Err(VerifyError::WrongSize { .. })
        ));
        assert_eq!(
            m.check_file("Void_9.9.9_x64.dmg", &good),
            Err(VerifyError::NotListed("Void_9.9.9_x64.dmg".into()))
        );
    }
}
