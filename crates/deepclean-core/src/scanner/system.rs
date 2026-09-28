use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytesize::ByteSize;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

/// Escape a path for embedding in an AppleScript string literal.
///
/// The Trash actions interpolate a discovered filename into an `osascript`
/// program. macOS permits `"` and `\` in filenames, so an unescaped name can
/// close the string and append arbitrary AppleScript — from a file the user
/// merely downloaded.
fn applescript_literal(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// Escape a path for a PowerShell single-quoted string, where the only
/// metacharacter is the quote itself and it is doubled to escape.
fn powershell_literal(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

pub struct SystemScanner;

#[async_trait]
impl EcosystemScanner for SystemScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::System
    }

    fn is_candidate(&self, _file_name: &str, _path: &Path) -> bool {
        false
    }

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let path_str = path.to_string_lossy();

        // Individual download entry (file or folder inside Downloads). Checked
        // first: a download named "Trash.zip" is a download, not the Trash.
        if is_inside_downloads(path) {
            return analyze_download_entry(path).await;
        }

        // Trash directories
        if path_str.contains("Trash") || path_str.contains("$Recycle.Bin") {
            if path.is_dir() {
                return analyze_trash(path).await;
            }
            return Ok(None);
        }

        // System logs
        if path_str.ends_with("Library/Logs") || path_str.contains("Library/Logs") {
            if path.is_dir() {
                return analyze_logs(path).await;
            }
            return Ok(None);
        }

        Ok(None)
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        let mut locs = vec![];

        // Enumerate individual entries inside Downloads
        if let Some(dl) = dirs::download_dir() {
            if dl.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&dl) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        // Skip hidden files
                        let name = entry.file_name();
                        if name.to_string_lossy().starts_with('.') {
                            continue;
                        }
                        locs.push(path);
                    }
                }
            }
        }

        // System logs
        if let Some(home) = crate::paths::home_dir() {
            let logs = home.join("Library/Logs");
            if logs.is_dir() {
                locs.push(logs);
            }
        }

        // Trash
        if let Some(home) = crate::paths::home_dir() {
            let mac_trash = home.join(".Trash");
            if mac_trash.is_dir() {
                locs.push(mac_trash);
            }

            // freedesktop.org Trash (GNOME, KDE, most Linux desktops).
            let xdg_trash = home.join(".local/share/Trash");
            if xdg_trash.is_dir() {
                locs.push(xdg_trash);
            }

            #[cfg(windows)]
            {
                let recycle = PathBuf::from("C:\\$Recycle.Bin");
                if recycle.is_dir() {
                    locs.push(recycle);
                }
            }
        }

        locs
    }
}

fn is_inside_downloads(path: &Path) -> bool {
    if let Some(dl) = dirs::download_dir() {
        if let Some(parent) = path.parent() {
            return parent == dl;
        }
    }
    false
}

/// Analyze an individual file or folder inside Downloads.
async fn analyze_download_entry(path: &Path) -> Result<Option<CleanableItem>, ScanError> {
    let size_bytes = if path.is_dir() {
        staleness::compute_dir_size(path).await
    } else if path.is_file() {
        tokio::fs::metadata(path)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        return Ok(None);
    };

    if size_bytes == 0 {
        return Ok(None);
    }

    let last_modified: Option<DateTime<Utc>> = tokio::fs::metadata(path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .map(DateTime::<Utc>::from);
    let days_stale = last_modified.map(staleness::days_since);

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into());

    let actions = download_actions(path, &file_name, size_bytes);

    Ok(Some(CleanableItem {
        details: Vec::new(),
        agent: None,
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        ecosystem: Ecosystem::System,
        kind: ArtifactKind::DownloadsDir,
        risk: RiskLevel::Safe,
        size_bytes,
        size_display: ByteSize(size_bytes).to_string(),
        last_modified,
        days_stale,
        project_name: Some(file_name),
        project_root: dirs::download_dir(),
        available_actions: actions,
    }))
}

/// Whether `path` is a freedesktop.org Trash (`~/.local/share/Trash`), whose
/// user-visible items live in `files/` with metadata in `info/`.
fn is_freedesktop_trash(path: &Path) -> bool {
    path.ends_with(".local/share/Trash")
}

/// Which command family a platform uses for Downloads actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    MacOs,
    Windows,
    /// Linux and other Unix desktops (freedesktop.org).
    Unix,
}

impl Platform {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// "Move to Trash" then "Delete permanently" for a Downloads entry.
///
/// These stay shell commands rather than `MoveToTrash`/`RemoveFile`: the
/// executor's path check refuses everything under `~/Downloads`, and only the
/// command-argument check allows the folder's *entries*. Paths are passed as
/// separate arguments (never through a shell) except for the AppleScript and
/// PowerShell programs, which escape them.
fn download_actions(path: &Path, file_name: &str, size_bytes: u64) -> Vec<CleanAction> {
    download_actions_for(Platform::current(), path, file_name, size_bytes)
}

fn download_actions_for(
    platform: Platform,
    path: &Path,
    file_name: &str,
    size_bytes: u64,
) -> Vec<CleanAction> {
    let is_dir = path.is_dir();
    let path_arg = path.to_string_lossy().to_string();
    let command = |program: &str, args: Vec<String>| ActionMethod::Command {
        program: program.into(),
        args,
        working_dir: None,
    };

    let (trash_label, trash) = match platform {
        Platform::MacOs => (
            "Move to Trash",
            command(
                "osascript",
                vec![
                    "-e".into(),
                    format!(
                        "tell application \"Finder\" to delete POSIX file \"{}\"",
                        applescript_literal(path)
                    ),
                ],
            ),
        ),
        Platform::Windows => (
            "Move to Recycle Bin",
            command(
                "powershell",
                vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    format!(
                        "$shell = New-Object -ComObject Shell.Application; $shell.Namespace(10).MoveHere('{}')",
                        powershell_literal(path)
                    ),
                ],
            ),
        ),
        // `gio trash` implements the freedesktop Trash spec (restorable from
        // the file manager). `--` so a name starting with `-` is not a flag.
        Platform::Unix => (
            "Move to Trash",
            command("gio", vec!["trash".into(), "--".into(), path_arg.clone()]),
        ),
    };

    let delete = match platform {
        Platform::Windows => {
            let recurse = if is_dir { " -Recurse" } else { "" };
            command(
                "powershell",
                vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    format!("Remove-Item '{}'{recurse} -Force", powershell_literal(path)),
                ],
            )
        }
        Platform::MacOs | Platform::Unix => {
            let mut args = Vec::new();
            if is_dir {
                args.push("-rf".to_string());
            }
            args.push("--".into());
            args.push(path_arg);
            command("rm", args)
        }
    };

    vec![
        CleanAction {
            id: Uuid::new_v4(),
            label: trash_label.into(),
            description: format!("Move \"{file_name}\" to the Trash (recoverable)"),
            method: trash,
            risk: RiskLevel::Safe,
            estimated_savings_bytes: size_bytes,
        },
        CleanAction {
            id: Uuid::new_v4(),
            label: "Delete permanently".into(),
            description: format!("Permanently delete \"{file_name}\""),
            method: delete,
            risk: RiskLevel::Danger,
            estimated_savings_bytes: size_bytes,
        },
    ]
}

async fn analyze_trash(path: &Path) -> Result<Option<CleanableItem>, ScanError> {
    let freedesktop = is_freedesktop_trash(path);
    let item_count = if freedesktop {
        count_entries(&path.join("files"))
    } else {
        count_entries(path)
    };
    if item_count == 0 {
        return Ok(None);
    }

    // On macOS the walker can be refused access to .Trash, which reads as a
    // size of zero; `du` gets there when it does.
    let size_bytes = match staleness::compute_dir_size(path).await {
        0 => du_fallback_size(path).await,
        measured => measured,
    };

    let actions = if freedesktop {
        freedesktop_trash_actions(path, size_bytes)
    } else if cfg!(target_os = "macos") {
        vec![CleanAction {
            id: Uuid::new_v4(),
            label: "Empty Trash".into(),
            description: "Permanently delete all items in the Trash".into(),
            method: ActionMethod::Command {
                program: "osascript".into(),
                args: vec![
                    "-e".into(),
                    "tell application \"Finder\" to empty the trash".into(),
                ],
                working_dir: None,
            },
            risk: RiskLevel::Caution,
            estimated_savings_bytes: size_bytes,
        }]
    } else if cfg!(target_os = "windows") {
        vec![CleanAction {
            id: Uuid::new_v4(),
            label: "Empty Recycle Bin".into(),
            description: "Permanently delete all items in the Recycle Bin".into(),
            method: ActionMethod::Command {
                program: "powershell".into(),
                args: vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    "Clear-RecycleBin -Force".into(),
                ],
                working_dir: None,
            },
            risk: RiskLevel::Caution,
            estimated_savings_bytes: size_bytes,
        }]
    } else {
        // A `.Trash` on some other Unix: no known way to empty it properly.
        return Ok(None);
    };

    Ok(Some(CleanableItem {
        details: Vec::new(),
        agent: None,
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        ecosystem: Ecosystem::System,
        kind: ArtifactKind::TrashBin,
        risk: RiskLevel::Caution,
        size_bytes,
        size_display: ByteSize(size_bytes).to_string(),
        last_modified: staleness::most_recent_modification(path),
        days_stale: None,
        project_name: Some(format!("Trash ({item_count} items)")),
        project_root: None,
        available_actions: actions,
    }))
}

/// Emptying a freedesktop Trash: `gio trash --empty` where GLib is
/// installed (it also handles trash on other mounts), else delete `files/`
/// and `info/` — the spec requires implementations to recreate them. Both
/// are irreversible, hence Caution like emptying the macOS Trash.
fn freedesktop_trash_actions(path: &Path, size_bytes: u64) -> Vec<CleanAction> {
    let parts: Vec<PathBuf> = ["files", "info"]
        .iter()
        .map(|d| path.join(d))
        .filter(|p| p.is_dir())
        .collect();
    let mut actions = vec![CleanAction {
        id: Uuid::new_v4(),
        label: "Empty Trash".into(),
        description: "Run `gio trash --empty` to permanently delete all items in the Trash".into(),
        method: ActionMethod::Command {
            program: "gio".into(),
            args: vec!["trash".into(), "--empty".into()],
            working_dir: None,
        },
        risk: RiskLevel::Caution,
        estimated_savings_bytes: size_bytes,
    }];
    if !parts.is_empty() {
        actions.push(CleanAction {
            id: Uuid::new_v4(),
            label: "Delete Trash contents".into(),
            description: format!(
                "Permanently delete {}/files and {}/info",
                path.display(),
                path.display()
            ),
            method: ActionMethod::RemoveDirs { paths: parts },
            risk: RiskLevel::Caution,
            estimated_savings_bytes: size_bytes,
        });
    }
    actions
}

/// Fallback size computation using `du -sk` (macOS/Linux) or powershell (Windows).
async fn du_fallback_size(path: &Path) -> u64 {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let output = if cfg!(target_os = "windows") {
            std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "(Get-ChildItem -Recurse -Force '{}' -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum",
                        powershell_literal(path.as_ref())
                    ),
                ])
                .output()
        } else {
            std::process::Command::new("du")
                .args(["-sk", &path.to_string_lossy()])
                .output()
        };

        match output {
            Ok(out) if out.status.success() => {
                let s = String::from_utf8_lossy(&out.stdout);
                // `du -sk` outputs "<size_in_kb>\t<path>"
                // powershell outputs just the number
                s.split_whitespace()
                    .next()
                    .and_then(|n| n.trim().parse::<u64>().ok())
                    .map(|kb| if cfg!(target_os = "windows") { kb } else { kb * 1024 })
                    .unwrap_or(0)
            }
            _ => 0,
        }
    })
    .await
    .unwrap_or(0)
}

/// Analyze ~/Library/Logs — offer to remove old log files.
async fn analyze_logs(path: &Path) -> Result<Option<CleanableItem>, ScanError> {
    let size_bytes = staleness::compute_dir_size(path).await;
    if size_bytes == 0 {
        return Ok(None);
    }

    let last_modified = staleness::most_recent_modification(path);
    let days_stale = last_modified.map(staleness::days_since);

    Ok(Some(CleanableItem {
        details: Vec::new(),
        agent: None,
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        ecosystem: Ecosystem::System,
        kind: ArtifactKind::SystemLogs,
        risk: RiskLevel::Safe,
        size_bytes,
        size_display: ByteSize(size_bytes).to_string(),
        last_modified,
        days_stale,
        project_name: Some("System Logs".into()),
        project_root: None,
        available_actions: vec![CleanAction {
            id: Uuid::new_v4(),
            label: "Remove logs older than 7 days".into(),
            description: "Delete log files older than 7 days from ~/Library/Logs".into(),
            method: ActionMethod::Command {
                program: "find".into(),
                args: vec![
                    path.to_string_lossy().to_string(),
                    "-type".into(),
                    "f".into(),
                    "-mtime".into(),
                    "+7".into(),
                    "-delete".into(),
                ],
                working_dir: None,
            },
            risk: RiskLevel::Safe,
            estimated_savings_bytes: size_bytes,
        }],
    }))
}

fn count_entries(path: &Path) -> usize {
    std::fs::read_dir(path)
        .map(|entries| entries.filter(|e| e.is_ok()).count())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_candidate_always_false() {
        let scanner = SystemScanner;
        assert!(!scanner.is_candidate("Downloads", Path::new("/Users/test/Downloads")));
        assert!(!scanner.is_candidate(".Trash", Path::new("/Users/test/.Trash")));
    }

    #[tokio::test]
    async fn analyze_individual_download_file() {
        let tmp = tempfile::tempdir().unwrap();
        let downloads = tmp.path().join("Downloads");
        std::fs::create_dir(&downloads).unwrap();
        let file = downloads.join("big_archive.zip");
        std::fs::write(&file, vec![0u8; 4096]).unwrap();

        // analyze_download_entry works on individual files
        let result = analyze_download_entry(&file).await.unwrap();
        assert!(result.is_some());
        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::System);
        assert_eq!(item.kind, ArtifactKind::DownloadsDir);
        assert_eq!(item.size_bytes, 4096);
        assert_eq!(item.project_name.as_deref(), Some("big_archive.zip"));
        assert!(item.available_actions.len() >= 2); // trash + delete
    }

    #[tokio::test]
    async fn analyze_individual_download_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let downloads = tmp.path().join("Downloads");
        std::fs::create_dir(&downloads).unwrap();
        let subdir = downloads.join("extracted_app");
        std::fs::create_dir(&subdir).unwrap();
        std::fs::write(subdir.join("file.bin"), vec![0u8; 2048]).unwrap();

        let result = analyze_download_entry(&subdir).await.unwrap();
        assert!(result.is_some());
        let item = result.unwrap();
        assert_eq!(item.size_bytes, 2048);
        assert_eq!(item.project_name.as_deref(), Some("extracted_app"));
    }

    // A plain `Trash` dir is only emptied on macOS (Finder) and Windows
    // (Recycle Bin); Linux uses the freedesktop layout tested below.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[tokio::test]
    async fn analyze_trash_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = tmp.path().join("Trash");
        std::fs::create_dir(&trash).unwrap();
        std::fs::write(trash.join("deleted_file"), vec![0u8; 512]).unwrap();

        let result = analyze_trash(&trash).await.unwrap();
        assert!(result.is_some());
        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::System);
        assert_eq!(item.kind, ArtifactKind::TrashBin);
        assert_eq!(item.size_bytes, 512);
    }

    #[tokio::test]
    async fn analyze_empty_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("empty.txt");
        std::fs::write(&file, b"").unwrap();

        let result = analyze_download_entry(&file).await.unwrap();
        assert!(result.is_none());
    }

    /// Every `"` in the escaped output must be backslash-escaped, so none of
    /// them can terminate the surrounding AppleScript string literal.
    fn quotes_are_all_escaped(s: &str) -> bool {
        let bytes = s.as_bytes();
        bytes.iter().enumerate().all(|(i, &c)| {
            if c != b'"' {
                return true;
            }
            // Count the backslashes immediately before it; an odd run escapes.
            let backslashes = bytes[..i].iter().rev().take_while(|&&b| b == b'\\').count();
            backslashes % 2 == 1
        })
    }

    #[test]
    fn applescript_literal_neutralises_a_quote_in_a_filename() {
        // macOS allows `"` in filenames. Unescaped, this would close the
        // string and append arbitrary AppleScript — from a file the user only
        // downloaded.
        let evil = PathBuf::from(r#"/Users/dev/Downloads/x" & (do shell script "id") & ""#);
        let escaped = applescript_literal(&evil);

        assert!(
            quotes_are_all_escaped(&escaped),
            "a quote survived unescaped: {escaped}"
        );
        assert!(
            escaped.contains("do shell script"),
            "the text is preserved, only neutralised: {escaped}"
        );
    }

    #[test]
    fn applescript_literal_escapes_backslashes_before_quotes() {
        // A trailing backslash must not end up escaping the closing quote.
        let p = PathBuf::from(r#"/tmp/a\"#);
        let escaped = applescript_literal(&p);
        assert_eq!(escaped, r"/tmp/a\\");
        let script = format!("\"{escaped}\"");
        assert!(quotes_are_all_escaped(&script[1..script.len() - 1]));
    }

    fn programs(actions: &[CleanAction]) -> Vec<(String, Vec<String>)> {
        actions
            .iter()
            .map(|a| match &a.method {
                ActionMethod::Command { program, args, .. } => (program.clone(), args.clone()),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn unix_downloads_use_gio_trash_and_rm_never_powershell() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("-rf.iso");
        std::fs::write(&file, "x").unwrap();
        let actions = download_actions_for(Platform::Unix, &file, "-rf.iso", 1);
        let p = programs(&actions);
        let f = file.to_string_lossy().to_string();
        assert_eq!(
            p[0],
            ("gio".into(), vec!["trash".into(), "--".into(), f.clone()])
        );
        assert_eq!(p[1], ("rm".into(), vec!["--".into(), f]));
        assert_eq!(actions[0].risk, RiskLevel::Safe);
        assert_eq!(actions[1].risk, RiskLevel::Danger);
    }

    #[test]
    fn unix_download_dir_is_removed_recursively() {
        let tmp = tempfile::tempdir().unwrap();
        let actions = download_actions_for(Platform::Unix, tmp.path(), "d", 1);
        let (program, args) = &programs(&actions)[1];
        assert_eq!(program, "rm");
        assert_eq!(args[0], "-rf");
    }

    #[test]
    fn macos_and_windows_keep_their_native_trash() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.zip");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(
            programs(&download_actions_for(Platform::MacOs, &file, "a.zip", 1))[0].0,
            "osascript"
        );
        let win = programs(&download_actions_for(Platform::Windows, &file, "a.zip", 1));
        assert!(win.iter().all(|(p, _)| p == "powershell"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn downloads_on_linux_never_use_powershell() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("Downloads/a.iso");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, vec![0u8; 10]).unwrap();
        let item = analyze_download_entry(&file).await.unwrap().unwrap();
        for (program, _) in programs(&item.available_actions) {
            assert_ne!(program, "powershell");
            assert_ne!(program, "osascript");
        }
    }

    #[tokio::test]
    async fn freedesktop_trash_counts_files_and_empties_with_gio_or_remove_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = tmp.path().join(".local/share/Trash");
        std::fs::create_dir_all(trash.join("files")).unwrap();
        std::fs::create_dir_all(trash.join("info")).unwrap();
        std::fs::write(trash.join("files/old.txt"), vec![0u8; 100]).unwrap();
        std::fs::write(
            trash.join("info/old.txt.trashinfo"),
            "[Trash Info]\nPath=/tmp/old.txt\n",
        )
        .unwrap();

        let item = SystemScanner.analyze(&trash).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::TrashBin);
        assert_eq!(item.project_name.as_deref(), Some("Trash (1 items)"));
        let a = &item.available_actions;
        assert_eq!(a.len(), 2);
        match &a[0].method {
            ActionMethod::Command { program, args, .. } => {
                assert_eq!(program, "gio");
                assert_eq!(args, &["trash", "--empty"]);
            }
            other => panic!("{other:?}"),
        }
        match &a[1].method {
            ActionMethod::RemoveDirs { paths } => {
                assert_eq!(paths, &vec![trash.join("files"), trash.join("info")]);
            }
            other => panic!("{other:?}"),
        }
        assert!(a.iter().all(|a| a.risk == RiskLevel::Caution));
    }

    #[tokio::test]
    async fn empty_freedesktop_trash_is_not_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = tmp.path().join(".local/share/Trash");
        std::fs::create_dir_all(trash.join("files")).unwrap();
        std::fs::create_dir_all(trash.join("info")).unwrap();
        assert!(SystemScanner.analyze(&trash).await.unwrap().is_none());
    }

    #[test]
    fn powershell_literal_doubles_single_quotes() {
        // In a PowerShell single-quoted string a doubled quote is a literal
        // quote, so the payload cannot break out.
        let evil = PathBuf::from("/tmp/x'; rm -rf ~; '");
        let escaped = powershell_literal(&evil);

        assert_eq!(escaped, "/tmp/x''; rm -rf ~; ''");
        for run in escaped.split(|c| c != '\'').filter(|r| !r.is_empty()) {
            assert_eq!(run.len() % 2, 0, "an odd run of quotes escapes: {escaped}");
        }
    }

    /// Emptying a freedesktop Trash by deleting `files/` and `info/` must
    /// pass the executor's safety checks.
    #[tokio::test]
    async fn freedesktop_trash_remove_dirs_pass_the_safety_checker() {
        use crate::action::ActionExecutor;
        use crate::safety::SafetyChecker;
        use crate::trash::TrashBackend;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("home")).unwrap();
        let home = tmp.path().join("home").canonicalize().unwrap();
        let trash = home.join(".local/share/Trash");
        std::fs::create_dir_all(trash.join("files")).unwrap();
        std::fs::create_dir_all(trash.join("info")).unwrap();
        std::fs::write(trash.join("files/old.txt"), vec![0u8; 64]).unwrap();
        std::fs::write(trash.join("info/old.txt.trashinfo"), "[Trash Info]").unwrap();
        std::fs::write(home.join(".local/share/keep.db"), "x").unwrap();

        let item = SystemScanner.analyze(&trash).await.unwrap().unwrap();
        let action = item.available_actions[1].clone();
        let exec = ActionExecutor::with_trash(
            SafetyChecker::with_home(home.clone(), vec![]),
            TrashBackend::Directory(tmp.path().join("t")),
        );
        let mut rx = exec.execute_batch(vec![(item, action)]);
        let mut failed = Vec::new();
        while let Some(ev) = rx.recv().await {
            if let ActionEvent::Failed { error, .. } = ev {
                failed.push(error);
            }
        }
        assert!(failed.is_empty(), "{failed:?}");
        assert!(!trash.join("files").exists());
        assert!(!trash.join("info").exists());
        assert!(home.join(".local/share/keep.db").exists());
    }
}
