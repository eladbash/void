//! Asking whether a newer Void exists.
//!
//! The check reads the repository's GitHub Releases, ignores drafts and
//! prereleases, and compares the newest release with the running version.
//! It is the only network request Void makes, and only when the user lets it
//! (Settings → About), or asks for it with "Check for updates".
//!
//! `VOID_UPDATE_FEED` replaces GitHub with a local releases JSON file (or
//! another URL), for development and the UI driver. A sandboxed run
//! (`VOID_HOME`) never touches the network: it checks a local feed or nothing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;
use serde::Deserialize;

/// The releases of eladbash/void, newest first.
pub const GITHUB_RELEASES: &str = "https://api.github.com/repos/eladbash/void/releases?per_page=20";

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    UpToDate { latest: Version },
    Available(Update),
}

/// A newer release, with what this install needs to update to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub version: Version,
    pub notes_url: String,
    /// The download for this platform and package, if the release has one.
    pub asset: Option<ReleaseAsset>,
    pub manifest: Option<ReleaseAsset>,
    pub signature: Option<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub notes_url: String,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    pub size: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CheckError {
    #[error("couldn't reach GitHub: {0}")]
    Offline(String),
    #[error("GitHub is limiting requests right now; try again later")]
    RateLimited,
    #[error("GitHub answered {0}")]
    Http(u16),
    #[error("couldn't read the release list: {0}")]
    Parse(String),
    #[error("no release has been published yet")]
    NoReleases,
}

/// How Void is installed, which decides the download that updates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Package {
    /// `Void.app`, updated from `Void_<v>_<arch>.app.tar.gz`.
    MacApp,
    /// Windows, updated by running `Void_<v>_x64-setup.exe`.
    WindowsInstaller,
    /// A Linux AppImage, replaced in place.
    AppImage,
    /// The Linux `.deb`, installed by the system package manager.
    Deb,
    /// The `void` command line tool.
    Cli,
}

impl Package {
    /// The desktop app's package on this machine.
    pub fn desktop() -> Self {
        match std::env::consts::OS {
            "macos" => Package::MacApp,
            "windows" => Package::WindowsInstaller,
            // The AppImage runtime sets $APPIMAGE to the file it runs from.
            _ if std::env::var_os("APPIMAGE").is_some() => Package::AppImage,
            _ => Package::Deb,
        }
    }

    /// The release asset this package updates from, as `release.yml` names
    /// it, for a platform given as `std::env::consts::{OS, ARCH}`.
    pub fn asset_name(self, version: &Version, os: &str, arch: &str) -> Option<String> {
        let v = version;
        // release.yml's matrix `arch`, per platform.
        let bundle_arch = match (os, arch) {
            ("macos", "aarch64") => "aarch64",
            ("macos", "x86_64") | ("windows", "x86_64") => "x64",
            ("linux", "x86_64") => "amd64",
            _ => return None,
        };
        Some(match self {
            Package::MacApp if os == "macos" => format!("Void_{v}_{bundle_arch}.app.tar.gz"),
            Package::WindowsInstaller if os == "windows" => {
                format!("Void_{v}_{bundle_arch}-setup.exe")
            }
            Package::AppImage if os == "linux" => format!("Void_{v}_{bundle_arch}.AppImage"),
            Package::Deb if os == "linux" => format!("void_{v}_{bundle_arch}.deb"),
            Package::Cli => {
                let target = match (os, arch) {
                    ("macos", "aarch64") => "aarch64-apple-darwin",
                    ("macos", "x86_64") => "x86_64-apple-darwin",
                    ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
                    ("windows", "x86_64") => "x86_64-pc-windows-msvc",
                    _ => return None,
                };
                let exe = if os == "windows" { ".exe" } else { "" };
                format!("void-{v}-{target}{exe}")
            }
            _ => return None,
        })
    }
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

/// The newest published, non-prerelease release in a GitHub releases list.
/// Tags that aren't `vX.Y.Z` versions are skipped.
pub fn latest_stable(json: &str) -> Result<Release, CheckError> {
    let releases: Vec<GhRelease> =
        serde_json::from_str(json).map_err(|e| CheckError::Parse(e.to_string()))?;
    releases
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let version = Version::parse(r.tag_name.trim_start_matches('v')).ok()?;
            // A version string with a pre-release part is a prerelease even
            // when the release isn't marked as one.
            version.pre.is_empty().then_some(Release {
                version,
                notes_url: r.html_url,
                assets: r.assets,
            })
        })
        .max_by(|a, b| a.version.cmp(&b.version))
        .ok_or(CheckError::NoReleases)
}

/// Compare `release` with the running `current` version.
pub fn compare(release: Release, current: &str, package: Package, os: &str, arch: &str) -> Check {
    let current = Version::parse(current.trim_start_matches('v')).unwrap_or(Version::new(0, 0, 0));
    if release.version <= current {
        return Check::UpToDate {
            latest: release.version,
        };
    }
    let find = |name: &str| release.assets.iter().find(|a| a.name == name).cloned();
    let asset = package
        .asset_name(&release.version, os, arch)
        .and_then(|name| find(&name));
    Check::Available(Update {
        manifest: find(super::manifest::MANIFEST_ASSET),
        signature: find(super::manifest::SIGNATURE_ASSET),
        asset,
        version: release.version,
        notes_url: release.notes_url,
    })
}

/// Where release information comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feed {
    Url(String),
    File(PathBuf),
}

impl Feed {
    /// The feed for this run: `VOID_UPDATE_FEED` if set, otherwise GitHub.
    /// `None` when this run must not check: a sandbox without a local feed.
    pub fn from_env() -> Option<Feed> {
        let custom = std::env::var("VOID_UPDATE_FEED")
            .ok()
            .filter(|v| !v.is_empty());
        let feed = match custom {
            Some(v) if v.starts_with("http://") || v.starts_with("https://") => Feed::Url(v),
            Some(v) => Feed::File(PathBuf::from(v.trim_start_matches("file://"))),
            None => Feed::Url(GITHUB_RELEASES.to_string()),
        };
        let sandboxed = crate::paths::home_override().is_some();
        match feed {
            Feed::Url(_) if sandboxed => None,
            feed => Some(feed),
        }
    }

    /// Fetch the release list and compare it with `current`. Blocking.
    pub fn check(&self, current: &str, package: Package) -> Result<Check, CheckError> {
        let json = match self {
            Feed::File(path) => std::fs::read_to_string(path)
                .map_err(|e| CheckError::Offline(format!("{}: {e}", path.display())))?,
            Feed::Url(url) => get_text(url, current)?,
        };
        let release = latest_stable(&json)?;
        Ok(compare(
            release,
            current,
            package,
            std::env::consts::OS,
            std::env::consts::ARCH,
        ))
    }
}

fn agent(current: &str) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .user_agent(&format!("Void/{current}"))
        .build()
}

fn get_text(url: &str, current: &str) -> Result<String, CheckError> {
    let resp = agent(current)
        .get(url)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(http_error)?;
    resp.into_string()
        .map_err(|e| CheckError::Offline(e.to_string()))
}

fn http_error(err: ureq::Error) -> CheckError {
    match err {
        // GitHub answers 403 with a zero remaining quota, or 429.
        ureq::Error::Status(403 | 429, resp)
            if resp.status() == 429 || resp.header("x-ratelimit-remaining") == Some("0") =>
        {
            CheckError::RateLimited
        }
        ureq::Error::Status(code, _) => CheckError::Http(code),
        ureq::Error::Transport(t) => CheckError::Offline(t.to_string()),
    }
}

/// Download `url` (or copy a local path / `file://` URL) to `dest`, calling
/// `progress(done, total)` as bytes arrive. Blocking. `total` is 0 when the
/// server doesn't say.
pub fn download(
    url: &str,
    dest: &Path,
    current: &str,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), CheckError> {
    use std::io::{Read, Write};
    let io = |e: std::io::Error| CheckError::Offline(e.to_string());
    let (mut reader, total): (Box<dyn Read>, u64) =
        if url.starts_with("http://") || url.starts_with("https://") {
            let resp = agent(current).get(url).call().map_err(http_error)?;
            let total = resp
                .header("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            (Box::new(resp.into_reader()), total)
        } else {
            let path = Path::new(url.trim_start_matches("file://"));
            let total = std::fs::metadata(path).map_err(io)?.len();
            (Box::new(std::fs::File::open(path).map_err(io)?), total)
        };
    let mut out = std::fs::File::create(dest).map_err(io)?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        if cancelled() {
            drop(out);
            let _ = std::fs::remove_file(dest);
            return Err(CheckError::Offline("cancelled".into()));
        }
        let n = reader.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(io)?;
        done += n as u64;
        progress(done, total);
    }
    out.sync_all().map_err(io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("testdata/releases.json");

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn picks_the_newest_stable_release() {
        // The fixture has a newer 1.4.0-beta.1 prerelease, a 1.5.0 draft and a
        // non-version tag; none of them count.
        let r = latest_stable(FIXTURE).unwrap();
        assert_eq!(r.version, v("1.3.0"));
        assert_eq!(
            r.notes_url,
            "https://github.com/eladbash/void/releases/tag/v1.3.0"
        );
    }

    #[test]
    fn a_pre_release_version_counts_as_a_prerelease_even_unmarked() {
        let json = r#"[
            {"tag_name":"v2.0.0-rc.1","html_url":"x","prerelease":false,"assets":[]},
            {"tag_name":"v1.0.0","html_url":"y","prerelease":false,"assets":[]}
        ]"#;
        assert_eq!(latest_stable(json).unwrap().version, v("1.0.0"));
    }

    #[test]
    fn no_stable_release_is_an_error() {
        let json = r#"[{"tag_name":"v2.0.0","html_url":"x","prerelease":true}]"#;
        assert_eq!(latest_stable(json), Err(CheckError::NoReleases));
        assert!(matches!(latest_stable("{"), Err(CheckError::Parse(_))));
    }

    #[test]
    fn compares_versions_numerically() {
        let r = latest_stable(FIXTURE).unwrap();
        for (current, newer) in [
            ("1.2.1", true),
            ("1.2.10", true),
            ("v1.2.1", true),
            ("1.3.0", false),
            ("1.10.0", false),
            ("2.0.0", false),
            ("not a version", true),
        ] {
            let got = compare(r.clone(), current, Package::MacApp, "macos", "aarch64");
            assert_eq!(matches!(got, Check::Available(_)), newer, "{current}");
        }
    }

    #[test]
    fn finds_the_download_for_each_platform_and_package() {
        let r = latest_stable(FIXTURE).unwrap();
        for (package, os, arch, name) in [
            (
                Package::MacApp,
                "macos",
                "aarch64",
                "Void_1.3.0_aarch64.app.tar.gz",
            ),
            (
                Package::MacApp,
                "macos",
                "x86_64",
                "Void_1.3.0_x64.app.tar.gz",
            ),
            (
                Package::WindowsInstaller,
                "windows",
                "x86_64",
                "Void_1.3.0_x64-setup.exe",
            ),
            (
                Package::AppImage,
                "linux",
                "x86_64",
                "Void_1.3.0_amd64.AppImage",
            ),
            (Package::Deb, "linux", "x86_64", "void_1.3.0_amd64.deb"),
            (
                Package::Cli,
                "macos",
                "aarch64",
                "void-1.3.0-aarch64-apple-darwin",
            ),
            (
                Package::Cli,
                "windows",
                "x86_64",
                "void-1.3.0-x86_64-pc-windows-msvc.exe",
            ),
        ] {
            let Check::Available(u) = compare(r.clone(), "1.2.1", package, os, arch) else {
                panic!("expected an update");
            };
            assert_eq!(
                u.asset.map(|a| a.name).as_deref(),
                Some(name),
                "{package:?} {os}"
            );
            assert_eq!(u.manifest.unwrap().name, "manifest.json");
            assert_eq!(u.signature.unwrap().name, "manifest.json.sig");
        }
    }

    #[test]
    fn an_unsupported_platform_gets_the_notice_without_a_download() {
        let r = latest_stable(FIXTURE).unwrap();
        let Check::Available(u) = compare(r, "1.2.1", Package::Deb, "linux", "aarch64") else {
            panic!("expected an update");
        };
        assert_eq!(u.asset, None);
        assert_eq!(u.version, v("1.3.0"));
    }

    #[test]
    fn a_local_feed_is_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("releases.json");
        std::fs::write(&path, FIXTURE).unwrap();
        let got = Feed::File(path).check("1.3.0", Package::MacApp).unwrap();
        assert_eq!(got, Check::UpToDate { latest: v("1.3.0") });
        let missing = Feed::File(dir.path().join("nope")).check("1.0.0", Package::MacApp);
        assert!(matches!(missing, Err(CheckError::Offline(_))));
    }

    #[test]
    fn downloads_copy_local_files_with_progress() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::write(&src, vec![7u8; 600_000]).unwrap();
        let dest = dir.path().join("dest");
        let mut seen = vec![];
        download(
            &format!("file://{}", src.display()),
            &dest,
            "1.0.0",
            &|| false,
            &mut |d, t| seen.push((d, t)),
        )
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap().len(), 600_000);
        assert_eq!(seen.last(), Some(&(600_000, 600_000)));

        let cancelled = download(
            src.to_str().unwrap(),
            &dest,
            "1.0.0",
            &|| true,
            &mut |_, _| {},
        );
        assert!(cancelled.is_err());
        assert!(!dest.exists());
    }
}
