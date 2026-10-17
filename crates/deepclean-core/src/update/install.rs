//! Downloading, verifying and installing an update.
//!
//! Nothing is installed unless the release manifest's signature checks out
//! against the release key and the download matches the manifest's size and
//! SHA-256; a download that fails either check is deleted. Installing
//! replaces the app beside itself, by rename, and puts the old one back if
//! the swap fails, so a failed update leaves the current version runnable.
//!
//! A sandboxed run (`VOID_HOME`) may point the key and the install target
//! somewhere else with `VOID_UPDATE_KEY` and `VOID_UPDATE_TARGET`, so the
//! whole flow can be exercised without touching the real app.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::manifest::{Manifest, VerifyError, RELEASE_KEY};
use super::release::{download, CheckError, Package, Update};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("this release has no download for your system; get it from the release page")]
    NoAsset,
    #[error("this release has no signed manifest, so it can't be verified")]
    Unsigned,
    #[error("{0}")]
    Verify(#[from] VerifyError),
    #[error("download failed: {0}")]
    Download(CheckError),
    #[error("download cancelled")]
    Cancelled,
    #[error("{0}")]
    Install(String),
}

/// What an install did, and what the app should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// The new version is in place; start `relaunch` once this process exits.
    Replaced { relaunch: Relaunch },
    /// The installer takes over; quit now and it starts the new version.
    InstallerStarted,
    /// The package manager has to install it: show the user this command.
    Manual { package: PathBuf, command: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relaunch {
    /// `open` this app bundle (macOS).
    App(PathBuf),
    /// Run this executable (an AppImage).
    Exe(PathBuf),
}

/// The public key updates must be signed with.
pub fn trusted_key() -> Result<String, InstallError> {
    match sandbox_override("VOID_UPDATE_KEY") {
        Some(path) => std::fs::read_to_string(&path)
            .map_err(|e| InstallError::Install(format!("{}: {e}", path.display()))),
        None => Ok(RELEASE_KEY.to_string()),
    }
}

/// What the update replaces: the running `.app` bundle on macOS, the
/// AppImage file, or the installed executable on Windows.
pub fn install_target(package: Package) -> Result<PathBuf, InstallError> {
    if let Some(path) = sandbox_override("VOID_UPDATE_TARGET") {
        return Ok(path);
    }
    let exe = std::env::current_exe().map_err(|e| InstallError::Install(e.to_string()))?;
    match package {
        Package::MacApp => exe
            .ancestors()
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                InstallError::Install(
                    "Void isn't running from an app bundle, so it can't replace itself".into(),
                )
            }),
        Package::AppImage => std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .ok_or_else(|| InstallError::Install("not running from an AppImage".into())),
        Package::WindowsInstaller | Package::Deb | Package::Cli => Ok(exe),
    }
}

fn sandbox_override(var: &str) -> Option<PathBuf> {
    crate::paths::home_override()?;
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Download `update`'s asset into `dir` and verify it. Returns the verified
/// file; anything that fails verification is deleted first.
pub fn download_verified(
    update: &Update,
    dir: &Path,
    current: &str,
    public_key_pem: &str,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<PathBuf, InstallError> {
    let asset = update.asset.as_ref().ok_or(InstallError::NoAsset)?;
    let (manifest_asset, sig_asset) = match (&update.manifest, &update.signature) {
        (Some(m), Some(s)) => (m, s),
        _ => return Err(InstallError::Unsigned),
    };
    std::fs::create_dir_all(dir).map_err(|e| InstallError::Install(e.to_string()))?;
    let fetch = |url: &str, name: &str| -> Result<Vec<u8>, InstallError> {
        let path = dir.join(name);
        download(url, &path, current, cancelled, &mut |_, _| {}).map_err(map_download)?;
        let bytes = std::fs::read(&path).map_err(|e| InstallError::Install(e.to_string()))?;
        let _ = std::fs::remove_file(&path);
        Ok(bytes)
    };
    // Verify the manifest before spending time on the big download.
    let manifest = fetch(&manifest_asset.url, "manifest.json")?;
    let signature = fetch(&sig_asset.url, "manifest.json.sig")?;
    let signature = String::from_utf8_lossy(&signature);
    let manifest = Manifest::verify(
        &manifest,
        &signature,
        public_key_pem,
        &update.version.to_string(),
    )?;
    if manifest.asset(&asset.name).is_none() {
        return Err(VerifyError::NotListed(asset.name.clone()).into());
    }

    let path = dir.join(&asset.name);
    if let Err(e) = download(&asset.url, &path, current, cancelled, progress) {
        let _ = std::fs::remove_file(&path);
        return Err(if cancelled() {
            InstallError::Cancelled
        } else {
            map_download(e)
        });
    }
    if let Err(e) = manifest.check_file(&asset.name, &path) {
        let _ = std::fs::remove_file(&path);
        return Err(e.into());
    }
    Ok(path)
}

fn map_download(e: CheckError) -> InstallError {
    InstallError::Download(e)
}

/// Install a verified download over `target`.
pub fn install(
    package: Package,
    download: &Path,
    target: &Path,
    version: &str,
) -> Result<Installed, InstallError> {
    match package {
        Package::MacApp => {
            replace_app(download, target, version)?;
            Ok(Installed::Replaced {
                relaunch: Relaunch::App(target.to_path_buf()),
            })
        }
        Package::AppImage | Package::Cli => {
            replace_file(download, target)?;
            Ok(Installed::Replaced {
                relaunch: Relaunch::Exe(target.to_path_buf()),
            })
        }
        Package::WindowsInstaller => {
            run_installer(download, target)?;
            Ok(Installed::InstallerStarted)
        }
        Package::Deb => Ok(Installed::Manual {
            command: format!(
                "sudo apt install {}",
                shell_quote(&download.to_string_lossy())
            ),
            package: download.to_path_buf(),
        }),
    }
}

/// Swap a `.app.tar.gz`'s bundle in for `target`.
///
/// The new bundle is unpacked next to the old one (same volume, so the swap
/// is two renames), its version is checked against the release, and the old
/// bundle is kept until the new one is in place.
pub fn replace_app(archive: &Path, target: &Path, version: &str) -> Result<(), InstallError> {
    let err = |what: &str, e: &dyn std::fmt::Display| InstallError::Install(format!("{what}: {e}"));
    let parent = target
        .parent()
        .ok_or_else(|| InstallError::Install("the app has no parent folder".into()))?;
    let name = target
        .file_name()
        .ok_or_else(|| InstallError::Install("the app has no name".into()))?
        .to_string_lossy()
        .into_owned();
    let stage = parent.join(format!(".{name}.update-{version}"));
    let backup = parent.join(format!(".{name}.previous"));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| {
        err(
            &format!(
                "can't write to {} (is Void in a folder you own?)",
                parent.display()
            ),
            &e,
        )
    })?;
    let cleanup = || {
        let _ = std::fs::remove_dir_all(&stage);
    };

    let out = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(&stage)
        .output()
        .map_err(|e| err("couldn't run tar", &e))?;
    if !out.status.success() {
        cleanup();
        return Err(InstallError::Install(format!(
            "couldn't unpack the update: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let Some(new_app) = std::fs::read_dir(&stage).ok().and_then(|d| {
        d.flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "app"))
    }) else {
        cleanup();
        return Err(InstallError::Install("the update contains no app".into()));
    };
    match bundle_version(&new_app) {
        Some(v) if v == version => {}
        found => {
            cleanup();
            return Err(InstallError::Install(format!(
                "the downloaded app is version {}, not {version}",
                found.unwrap_or_else(|| "unknown".into())
            )));
        }
    }
    clear_quarantine(&new_app);

    let _ = std::fs::remove_dir_all(&backup);
    let had_old = target.exists();
    if had_old {
        if let Err(e) = std::fs::rename(target, &backup) {
            cleanup();
            return Err(err("couldn't move the current app aside", &e));
        }
    }
    if let Err(e) = std::fs::rename(&new_app, target) {
        // Put the running version back where it was.
        if had_old {
            let _ = std::fs::rename(&backup, target);
        }
        cleanup();
        return Err(err("couldn't put the new app in place", &e));
    }
    let _ = std::fs::remove_dir_all(&backup);
    cleanup();
    Ok(())
}

/// The bundle's `CFBundleShortVersionString`, from its XML `Info.plist`.
pub fn bundle_version(app: &Path) -> Option<String> {
    let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).ok()?;
    let after = plist
        .split("<key>CFBundleShortVersionString</key>")
        .nth(1)?;
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_string())
}

/// A bundle Void verified itself must not be stopped by Gatekeeper's
/// "downloaded from the internet" prompt (releases aren't notarized).
fn clear_quarantine(app: &Path) {
    if cfg!(target_os = "macos") {
        let _ = Command::new("xattr")
            .args(["-dr", "com.apple.quarantine"])
            .arg(app)
            .output();
    }
}

/// Replace a single executable file (an AppImage, or the CLI) in place.
pub fn replace_file(download: &Path, target: &Path) -> Result<(), InstallError> {
    let err = |what: &str, e: std::io::Error| InstallError::Install(format!("{what}: {e}"));
    let staged = target.with_file_name(format!(
        ".{}.update",
        target.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::copy(download, &staged).map_err(|e| err("couldn't stage the update", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&staged);
            return Err(err("couldn't make the update executable", e));
        }
    }
    // A rename over the running file is fine on Unix: the running process
    // keeps the old inode.
    std::fs::rename(&staged, target).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        err("couldn't replace the old version", e)
    })
}

/// Start the Windows installer silently; once it finishes, start Void again.
fn run_installer(installer: &Path, exe: &Path) -> Result<(), InstallError> {
    // NSIS installers take /S for a silent install. `start /wait` holds the
    // relaunch until the installer has exited.
    Command::new("cmd")
        .arg("/C")
        .arg(format!(
            "start \"\" /wait \"{}\" /S & start \"\" \"{}\"",
            installer.display(),
            exe.display()
        ))
        .spawn()
        .map(|_| ())
        .map_err(|e| InstallError::Install(format!("couldn't start the installer: {e}")))
}

/// Start the new version once process `pid` (this one) has exited.
pub fn relaunch_after_exit(relaunch: &Relaunch, pid: u32) -> std::io::Result<()> {
    let (program, arg) = match relaunch {
        Relaunch::App(app) => ("open", app.as_os_str()),
        Relaunch::Exe(exe) => ("", exe.as_os_str()),
    };
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(r#"while kill -0 "$0" 2>/dev/null; do sleep 0.2; done; if [ -n "$1" ]; then exec "$1" "$2"; else exec "$2"; fi"#)
        .arg(pid.to_string())
        .arg(program)
        .arg(arg);
    cmd.spawn().map(|_| ())
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::update::release::ReleaseAsset;
    use semver::Version;

    /// A minimal `Void.app` of `version` with a marker file.
    fn fake_app(dir: &Path, version: &str, marker: &str) -> PathBuf {
        let app = dir.join("Void.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            format!(
                "<?xml version=\"1.0\"?><plist><dict>\n<key>CFBundleShortVersionString</key>\n<string>{version}</string>\n</dict></plist>"
            ),
        )
        .unwrap();
        std::fs::write(app.join("Contents/MacOS/void-app"), marker).unwrap();
        app
    }

    fn archive(app_parent: &Path, out: &Path) {
        let ok = Command::new("tar")
            .arg("-czf")
            .arg(out)
            .arg("-C")
            .arg(app_parent)
            .arg("Void.app")
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }

    fn marker(app: &Path) -> String {
        std::fs::read_to_string(app.join("Contents/MacOS/void-app")).unwrap()
    }

    #[test]
    fn replaces_an_app_bundle_with_the_new_version() {
        let tmp = tempfile::tempdir().unwrap();
        let installed = tmp.path().join("Applications");
        std::fs::create_dir_all(&installed).unwrap();
        let target = fake_app(&installed, "1.2.1", "old");
        let build = tmp.path().join("build");
        fake_app(&build, "1.3.0", "new");
        let tgz = tmp.path().join("Void_1.3.0_aarch64.app.tar.gz");
        archive(&build, &tgz);

        replace_app(&tgz, &target, "1.3.0").unwrap();
        assert_eq!(marker(&target), "new");
        assert_eq!(bundle_version(&target).as_deref(), Some("1.3.0"));
        // Nothing left behind next to it.
        let names: Vec<_> = std::fs::read_dir(&installed)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec!["Void.app"]);
    }

    #[test]
    fn refuses_a_bundle_of_the_wrong_version_and_keeps_the_old_one() {
        let tmp = tempfile::tempdir().unwrap();
        let target = fake_app(&tmp.path().join("Applications"), "1.2.1", "old");
        let build = tmp.path().join("build");
        fake_app(&build, "1.2.0", "older");
        let tgz = tmp.path().join("u.tar.gz");
        archive(&build, &tgz);

        let err = replace_app(&tgz, &target, "1.3.0").unwrap_err();
        assert!(err.to_string().contains("1.2.0"), "{err}");
        assert_eq!(marker(&target), "old");
    }

    #[test]
    fn a_corrupt_archive_leaves_the_app_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let target = fake_app(&tmp.path().join("Applications"), "1.2.1", "old");
        let tgz = tmp.path().join("u.tar.gz");
        std::fs::write(&tgz, "not a tarball").unwrap();
        assert!(replace_app(&tgz, &target, "1.3.0").is_err());
        assert_eq!(marker(&target), "old");
        assert_eq!(
            std::fs::read_dir(target.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn replaces_an_appimage_in_place_and_keeps_it_executable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("Void.AppImage");
        std::fs::write(&target, "old").unwrap();
        let dl = tmp.path().join("dl");
        std::fs::write(&dl, "new").unwrap();
        replace_file(&dl, &target).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111);
    }

    #[test]
    fn a_deb_is_left_to_the_package_manager() {
        let got = install(
            Package::Deb,
            Path::new("/tmp/void updates/void_1.3.0_amd64.deb"),
            Path::new("/usr/bin/void-app"),
            "1.3.0",
        )
        .unwrap();
        assert_eq!(
            got,
            Installed::Manual {
                package: PathBuf::from("/tmp/void updates/void_1.3.0_amd64.deb"),
                command: "sudo apt install '/tmp/void updates/void_1.3.0_amd64.deb'".into(),
            }
        );
    }

    /// A signed release in `dir`: the asset, its manifest and signature, all
    /// as local files, signed with a fresh key. Returns (update, public key).
    fn signed_release(dir: &Path, asset_bytes: &[u8], tamper: bool) -> (Update, String) {
        use ed25519_dalek::pkcs8::EncodePublicKey;
        use ed25519_dalek::{Signer, SigningKey};
        use sha2::{Digest, Sha256};
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public = key
            .verifying_key()
            .to_public_key_pem(Default::default())
            .unwrap();
        let name = "Void_1.3.0_aarch64.app.tar.gz";
        std::fs::write(dir.join(name), asset_bytes).unwrap();
        let sha: String = Sha256::digest(asset_bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let manifest = format!(
            r#"{{"version":"1.3.0","assets":[{{"name":"{name}","size":{},"sha256":"{sha}"}}]}}"#,
            asset_bytes.len()
        );
        let sig: String = key
            .sign(manifest.as_bytes())
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        std::fs::write(dir.join("manifest.json"), &manifest).unwrap();
        std::fs::write(dir.join("manifest.json.sig"), sig).unwrap();
        if tamper {
            std::fs::write(dir.join(name), b"tampered!").unwrap();
        }
        let a = |n: &str| {
            Some(ReleaseAsset {
                name: n.into(),
                url: dir.join(n).display().to_string(),
                size: 0,
            })
        };
        (
            Update {
                version: Version::new(1, 3, 0),
                notes_url: String::new(),
                asset: a(name),
                manifest: a("manifest.json"),
                signature: a("manifest.json.sig"),
            },
            public,
        )
    }

    #[test]
    fn downloads_and_verifies_a_signed_release() {
        let tmp = tempfile::tempdir().unwrap();
        let (update, key) = signed_release(tmp.path(), b"the new app", false);
        let dl = tmp.path().join("dl");
        let mut last = (0, 0);
        let path = download_verified(&update, &dl, "1.2.1", &key, &|| false, &mut |d, t| {
            last = (d, t)
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"the new app");
        assert_eq!(last, (11, 11));
    }

    #[test]
    fn refuses_and_deletes_a_download_that_does_not_match() {
        let tmp = tempfile::tempdir().unwrap();
        let (update, key) = signed_release(tmp.path(), b"the new app", true);
        let dl = tmp.path().join("dl");
        let err =
            download_verified(&update, &dl, "1.2.1", &key, &|| false, &mut |_, _| {}).unwrap_err();
        assert!(matches!(err, InstallError::Verify(_)), "{err}");
        assert!(!dl.join("Void_1.3.0_aarch64.app.tar.gz").exists());
    }

    #[test]
    fn refuses_a_release_signed_by_another_key() {
        let tmp = tempfile::tempdir().unwrap();
        let (update, _) = signed_release(tmp.path(), b"x", false);
        let err = download_verified(
            &update,
            &tmp.path().join("dl"),
            "1.2.1",
            RELEASE_KEY,
            &|| false,
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(matches!(
            err,
            InstallError::Verify(VerifyError::WrongSignature)
        ));
    }

    #[test]
    fn an_unsigned_release_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut update, key) = signed_release(tmp.path(), b"x", false);
        update.signature = None;
        assert!(matches!(
            download_verified(
                &update,
                tmp.path(),
                "1.2.1",
                &key,
                &|| false,
                &mut |_, _| {}
            ),
            Err(InstallError::Unsigned)
        ));
    }

    #[test]
    fn cancelling_stops_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let (update, key) = signed_release(tmp.path(), &vec![1u8; 2_000_000], false);
        let dl = tmp.path().join("dl");
        // Let the manifest and signature through, then cancel the asset.
        let calls = std::cell::Cell::new(0);
        let cancelled = || {
            calls.set(calls.get() + 1);
            calls.get() > 4
        };
        let err =
            download_verified(&update, &dl, "1.2.1", &key, &cancelled, &mut |_, _| {}).unwrap_err();
        assert!(matches!(err, InstallError::Cancelled), "{err}");
        assert!(!dl.join("Void_1.3.0_aarch64.app.tar.gz").exists());
    }

    #[test]
    fn relaunches_only_after_the_old_process_exits() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("started");
        let script = tmp.path().join("new-version");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho started > '{}'\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Stands in for the running app.
        let mut old = Command::new("sleep").arg("1").spawn().unwrap();
        relaunch_after_exit(&Relaunch::Exe(script), old.id()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert!(!marker.exists(), "started before the old process exited");
        old.wait().unwrap();
        for _ in 0..50 {
            if marker.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("the new version never started");
    }
}
