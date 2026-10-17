//! `void version --check` and `void self-update`.

use std::path::{Path, PathBuf};

use deepclean_core::update::install::{self, InstallError};
use deepclean_core::update::{Check, Feed, Package};
use serde_json::json;

const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// How this `void` binary got here, which decides how it updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// `cargo install`: cargo updates it.
    Cargo,
    /// Inside the desktop app's bundle: the app's updater updates both.
    AppBundle(PathBuf),
    /// A release binary on the PATH: it replaces itself.
    Release,
}

pub fn origin(exe: &Path, cargo_home: Option<&Path>) -> Origin {
    if let Some(app) = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
    {
        return Origin::AppBundle(app.to_path_buf());
    }
    if let Some(bin) = cargo_home.map(|h| h.join("bin")) {
        if exe.starts_with(&bin) {
            return Origin::Cargo;
        }
    }
    Origin::Release
}

fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cargo")))
}

fn this_origin() -> Origin {
    let exe = std::env::current_exe().unwrap_or_default();
    let exe = exe.canonicalize().unwrap_or(exe);
    let cargo = cargo_home().map(|h| h.canonicalize().unwrap_or(h));
    origin(&exe, cargo.as_deref())
}

pub fn cargo_command(version: &str) -> String {
    format!("cargo install --force --locked --git https://github.com/eladbash/void --tag v{version} void-cli")
}

fn check() -> Result<Check, String> {
    let feed = Feed::from_env().ok_or(
        "update checks are off in a sandbox (--home); set VOID_UPDATE_FEED to a local releases file",
    )?;
    feed.check(CURRENT, Package::Cli).map_err(|e| e.to_string())
}

/// `void version [--check]`.
pub fn version(check_latest: bool, json_out: bool) -> Result<i32, String> {
    if !check_latest {
        if json_out {
            println!("{}", json!({ "current": CURRENT }));
        } else {
            println!("void {CURRENT}");
        }
        return Ok(0);
    }
    let found = check()?;
    let (latest, notes) = match &found {
        Check::UpToDate { latest } => (latest.to_string(), None),
        Check::Available(u) => (u.version.to_string(), Some(u.notes_url.clone())),
    };
    let available = matches!(found, Check::Available(_));
    let how = match this_origin() {
        Origin::Cargo => cargo_command(&latest),
        Origin::AppBundle(_) => "update the Void app (Settings → About)".to_string(),
        Origin::Release => "void self-update".to_string(),
    };
    if json_out {
        println!(
            "{}",
            json!({
                "current": CURRENT,
                "latest": latest,
                "update_available": available,
                "notes_url": notes,
                "update_with": available.then_some(how),
            })
        );
    } else if available {
        println!("void {CURRENT}: version {latest} is available");
        if let Some(url) = notes {
            println!("What's new: {url}");
        }
        println!("Update with: {how}");
    } else {
        println!("void {CURRENT} is the latest version");
    }
    Ok(0)
}

/// `void self-update`.
pub fn self_update(json_out: bool) -> Result<i32, String> {
    let found = check()?;
    let Check::Available(update) = found else {
        if json_out {
            println!("{}", json!({ "updated": false, "current": CURRENT }));
        } else {
            println!("void {CURRENT} is the latest version");
        }
        return Ok(0);
    };
    let version = update.version.to_string();
    match this_origin() {
        Origin::Cargo => {
            let cmd = cargo_command(&version);
            if json_out {
                println!(
                    "{}",
                    json!({ "updated": false, "latest": version, "run": cmd })
                );
            } else {
                println!("void was installed with cargo; update it with:\n  {cmd}");
            }
            return Ok(0);
        }
        Origin::AppBundle(app) => {
            return Err(format!(
                "this void comes with the Void app at {}; update the app instead (Settings → About)",
                app.display()
            ));
        }
        Origin::Release => {}
    }

    let target = install::install_target(Package::Cli).map_err(|e| e.to_string())?;
    let dir = target
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
        .join(".void-update");
    let key = install::trusted_key().map_err(|e| e.to_string())?;
    let mut last_pct = u64::MAX;
    let result = install::download_verified(
        &update,
        &dir,
        CURRENT,
        &key,
        &|| false,
        &mut |done, total| {
            if json_out || total == 0 {
                return;
            }
            let pct = done * 100 / total;
            if pct != last_pct && pct % 10 == 0 {
                last_pct = pct;
                eprint!("\rDownloading void {version}… {pct}%");
            }
        },
    );
    if !json_out {
        eprintln!();
    }
    let file = result.map_err(|e| {
        let _ = std::fs::remove_dir_all(&dir);
        match e {
            InstallError::NoAsset => format!(
                "release {version} has no void binary for this system; see {}",
                update.notes_url
            ),
            e => e.to_string(),
        }
    })?;
    let replaced = install::replace_file(&file, &target).map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&dir);
    replaced?;
    if json_out {
        println!(
            "{}",
            json!({ "updated": true, "from": CURRENT, "to": version, "path": target })
        );
    } else {
        println!(
            "Updated void {CURRENT} → {version} ({}), after verifying its signature and checksum",
            target.display()
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_cargo_installs_and_bundled_copies_apart() {
        let cargo = Path::new("/Users/me/.cargo");
        assert_eq!(
            origin(Path::new("/Users/me/.cargo/bin/void"), Some(cargo)),
            Origin::Cargo
        );
        assert_eq!(
            origin(
                Path::new("/Applications/Void.app/Contents/MacOS/void"),
                Some(cargo)
            ),
            Origin::AppBundle(PathBuf::from("/Applications/Void.app"))
        );
        assert_eq!(
            origin(Path::new("/usr/local/bin/void"), Some(cargo)),
            Origin::Release
        );
        assert_eq!(
            origin(Path::new("/usr/local/bin/void"), None),
            Origin::Release
        );
    }

    #[test]
    fn the_cargo_command_pins_the_release_tag() {
        assert_eq!(
            cargo_command("1.3.0"),
            "cargo install --force --locked --git https://github.com/eladbash/void --tag v1.3.0 void-cli"
        );
    }
}
