//! A fake Void release on disk, for exercising updates end to end.
//!
//! [`fake_release`] writes what GitHub would serve: a releases feed, the
//! download for this platform, and a manifest signed with a throwaway key.
//! Point a sandboxed app at it with `VOID_UPDATE_FEED` (the feed) and
//! `VOID_UPDATE_KEY` (the key), and at a fake installed app with
//! `VOID_UPDATE_TARGET`. `void dev release <dir>` builds one.
//!
//! The key is fixed and public: it signs test releases only, and the real
//! app ignores `VOID_UPDATE_KEY` outside a sandbox.

use std::path::{Path, PathBuf};
use std::process::Command;

use ed25519_dalek::pkcs8::EncodePublicKey;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

use crate::update::Package;

/// What [`fake_release`] wrote.
#[derive(Debug, Clone)]
pub struct FakeRelease {
    /// The releases feed (`VOID_UPDATE_FEED`).
    pub feed: PathBuf,
    /// The public key the manifest is signed with (`VOID_UPDATE_KEY`).
    pub key: PathBuf,
    /// An installed `Void.app` at the old version, to update
    /// (`VOID_UPDATE_TARGET`; macOS only).
    pub installed: PathBuf,
    /// The download for this platform.
    pub asset: PathBuf,
}

/// The throwaway test signing key.
pub fn test_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// A minimal `Void.app` of `version`, whose executable is a marker file
/// reading `Void <version>`.
pub fn fake_app(parent: &Path, version: &str) -> PathBuf {
    let app = parent.join("Void.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).expect("create app");
    std::fs::write(
        app.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\n\
             <key>CFBundleIdentifier</key>\n<string>com.void.app</string>\n\
             <key>CFBundleShortVersionString</key>\n<string>{version}</string>\n</dict></plist>\n"
        ),
    )
    .expect("write Info.plist");
    std::fs::write(
        app.join("Contents/MacOS/void-app"),
        format!("Void {version}\n"),
    )
    .expect("write marker");
    app
}

/// Write a release of `version` into `dir`, plus an installed app at
/// `installed_version`. With `tamper`, the download no longer matches its
/// signed checksum.
pub fn fake_release(
    dir: &Path,
    version: &str,
    installed_version: &str,
    tamper: bool,
) -> std::io::Result<FakeRelease> {
    std::fs::create_dir_all(dir)?;
    let dir = dir.canonicalize()?;
    let semver = semver::Version::parse(version)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let assets = dir.join("assets");
    std::fs::create_dir_all(&assets)?;

    let name = Package::desktop()
        .asset_name(&semver, std::env::consts::OS, std::env::consts::ARCH)
        .unwrap_or_else(|| format!("Void_{version}_unsupported"));
    let asset = assets.join(&name);
    if name.ends_with(".app.tar.gz") {
        let build = dir.join("build");
        let _ = std::fs::remove_dir_all(&build);
        fake_app(&build, version);
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(&asset)
            .arg("-C")
            .arg(&build)
            .arg("Void.app")
            .status()?
            .success();
        let _ = std::fs::remove_dir_all(&build);
        if !ok {
            return Err(std::io::Error::other("tar failed"));
        }
    } else {
        std::fs::write(&asset, format!("Void {version}\n"))?;
    }

    // The `void` CLI: a script that prints its version.
    let cli_name = Package::Cli
        .asset_name(&semver, std::env::consts::OS, std::env::consts::ARCH)
        .unwrap_or_else(|| format!("void-{version}-unsupported"));
    std::fs::write(
        assets.join(&cli_name),
        format!("#!/bin/sh\necho void {version}\n"),
    )?;

    let listed = |n: &str| -> std::io::Result<serde_json::Value> {
        let bytes = std::fs::read(assets.join(n))?;
        let sha: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(serde_json::json!({ "name": n, "size": bytes.len(), "sha256": sha }))
    };
    let manifest = serde_json::to_string_pretty(&serde_json::json!({
        "version": version,
        "assets": [listed(&name)?, listed(&cli_name)?],
    }))?;
    let key = test_signing_key();
    let sig: String = key
        .sign(manifest.as_bytes())
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    std::fs::write(assets.join("manifest.json"), &manifest)?;
    std::fs::write(assets.join("manifest.json.sig"), sig)?;
    if tamper {
        // Same size, different bytes: only the checksum can catch it.
        for path in [asset.clone(), assets.join(&cli_name)] {
            let mut bytes = std::fs::read(&path)?;
            if let Some(last) = bytes.last_mut() {
                *last ^= 0xff;
            }
            std::fs::write(&path, bytes)?;
        }
    }

    let public = dir.join("release-key.pem");
    let pem = key
        .verifying_key()
        .to_public_key_pem(Default::default())
        .map_err(std::io::Error::other)?;
    std::fs::write(&public, pem)?;

    let entry = |n: &str| {
        let path = assets.join(n);
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        serde_json::json!({
            "name": n,
            "size": size,
            "browser_download_url": path.display().to_string(),
        })
    };
    let feed = dir.join("releases.json");
    std::fs::write(
        &feed,
        serde_json::to_string_pretty(&serde_json::json!([{
            "tag_name": format!("v{version}"),
            "html_url": format!("https://github.com/eladbash/void/releases/tag/v{version}"),
            "draft": false,
            "prerelease": false,
            "assets": [
                entry(&name),
                entry(&cli_name),
                entry("manifest.json"),
                entry("manifest.json.sig"),
            ],
        }]))?,
    )?;

    let apps = dir.join("Applications");
    let _ = std::fs::remove_dir_all(&apps);
    let installed = fake_app(&apps, installed_version);

    Ok(FakeRelease {
        feed,
        key: public,
        installed,
        asset,
    })
}
