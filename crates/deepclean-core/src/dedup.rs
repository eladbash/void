//! Duplicate detection and space-saving deduplication for large files
//! (primarily local model weights copied across Ollama, LM Studio, Hugging
//! Face and ComfyUI).
//!
//! The same 5 GB GGUF routinely lives in three stores at once: pulled by
//! Ollama, downloaded again by LM Studio, and once more through the Hugging
//! Face cache by llama.cpp or MLX. Deleting any copy breaks the tool that owns
//! it; *sharing storage* between them breaks nothing. So deduplication never
//! deletes content — each duplicate is replaced by a copy-on-write clone of
//! the kept file (APFS, btrfs, XFS) or, failing that, a hardlink to it.
//!
//! Hardlinks share one inode, so writing to one path would change the other.
//! Model stores never rewrite weights in place (they download to a temporary
//! name and rename), which is why the fallback is acceptable; clones are
//! preferred because they have no such coupling.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::{Command, Stdio};

use tracing::{info, warn};

use crate::model::DedupGroup;

/// Bytes read from each end of a file for the cheap second-stage hash.
const PARTIAL_CHUNK: u64 = 1 << 20;

/// Suffix of the sibling path a replacement is staged at before the atomic
/// rename over the duplicate.
const TMP_SUFFIX: &str = ".void-dedup-tmp";

/// Find groups of byte-identical regular files among `files`.
///
/// Three stages, each only run on survivors of the previous one: equal
/// length, equal blake3 of the first and last MiB, equal full blake3. Files
/// smaller than `min_size` (and empty files) are ignored, symlinks are
/// ignored, and paths that are already hardlinks of one another count as one
/// file — replacing them would save nothing.
///
/// APFS clones cannot be told apart from independent copies without private
/// APIs, so two files that are already clones are still reported; applying
/// the group is harmless but saves nothing further.
pub async fn find_duplicates(files: Vec<PathBuf>, min_size: u64) -> Vec<DedupGroup> {
    tokio::task::spawn_blocking(move || find_duplicates_sync(files, min_size))
        .await
        .unwrap_or_default()
}

/// Blocking form of [`find_duplicates`].
pub fn find_duplicates_sync(mut files: Vec<PathBuf>, min_size: u64) -> Vec<DedupGroup> {
    files.sort();
    files.dedup();

    // Stage 1: regular files of the same length, one path per inode.
    let mut seen_inodes = HashSet::new();
    let mut by_len: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for path in files {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file() || meta.len() == 0 || meta.len() < min_size {
            continue;
        }
        if let Some(id) = file_id(&meta) {
            if !seen_inodes.insert(id) {
                continue;
            }
        }
        by_len.entry(meta.len()).or_default().push(path);
    }

    let mut groups = Vec::new();
    for (len, paths) in by_len {
        if paths.len() < 2 {
            continue;
        }

        // Stage 2: first and last MiB.
        let mut by_partial: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for path in paths {
            if let Ok(h) = partial_hash(&path, len) {
                by_partial.entry(h).or_default().push(path);
            }
        }

        // Stage 3: the whole file.
        for (_, paths) in by_partial {
            if paths.len() < 2 {
                continue;
            }
            let mut by_full: HashMap<String, Vec<PathBuf>> = HashMap::new();
            for path in paths {
                if let Ok(h) = hash_file(&path) {
                    by_full.entry(h).or_default().push(path);
                }
            }
            for (hash, mut paths) in by_full {
                if paths.len() < 2 {
                    continue;
                }
                paths.sort();
                let keep = paths.remove(0);
                groups.push(DedupGroup {
                    keep,
                    duplicates: paths,
                    size_bytes: len,
                    hash,
                });
            }
        }
    }

    // Biggest savings first, then a stable order.
    groups.sort_by(|a, b| {
        let sa = a.size_bytes * a.duplicates.len() as u64;
        let sb = b.size_bytes * b.duplicates.len() as u64;
        sb.cmp(&sa).then_with(|| a.keep.cmp(&b.keep))
    });
    groups
}

/// Replace every duplicate in every group with a copy-on-write clone (APFS) or
/// a hardlink of `keep`, after re-hashing both. Returns bytes saved.
///
/// Nothing is trusted from scan time: `keep` must still hash to the group's
/// digest (otherwise the whole group is skipped), and each duplicate must
/// still have the recorded size and digest (otherwise it is left untouched).
/// The replacement is built at a sibling temp path and renamed over the
/// duplicate, so the duplicate's path is never missing or half-written.
///
/// Partial success returns the bytes saved and logs what was skipped; when
/// nothing could be replaced, the reasons are returned as the error.
pub async fn apply_groups(groups: &[DedupGroup]) -> Result<u64, String> {
    let groups = groups.to_vec();
    tokio::task::spawn_blocking(move || apply_groups_sync(&groups))
        .await
        .map_err(|e| format!("deduplication task failed: {e}"))?
}

/// Blocking form of [`apply_groups`].
pub fn apply_groups_sync(groups: &[DedupGroup]) -> Result<u64, String> {
    if groups.iter().all(|g| g.duplicates.is_empty()) {
        return Err("nothing to deduplicate".into());
    }

    let mut saved = 0u64;
    let mut problems = Vec::new();

    for group in groups {
        let keep_meta = match verify_keep(group) {
            Ok(meta) => meta,
            Err(reason) => {
                problems.push(format!("{}: {reason}", group.keep.display()));
                continue;
            }
        };
        for dup in &group.duplicates {
            match replace_duplicate(group, &keep_meta, dup) {
                Ok(how) => {
                    info!(dup = %dup.display(), keep = %group.keep.display(), how, "Deduplicated");
                    saved += group.size_bytes;
                }
                Err(reason) => problems.push(format!("{}: {reason}", dup.display())),
            }
        }
    }

    if saved == 0 {
        return Err(problems.join("; "));
    }
    for problem in &problems {
        warn!("Deduplication skipped {problem}");
    }
    Ok(saved)
}

/// The kept file must still exist, be a regular file of the recorded size and
/// still hash to the recorded digest.
fn verify_keep(group: &DedupGroup) -> Result<fs::Metadata, String> {
    let meta = fs::symlink_metadata(&group.keep).map_err(|e| format!("kept file missing: {e}"))?;
    if !meta.file_type().is_file() {
        return Err("kept path is no longer a regular file".into());
    }
    if meta.len() != group.size_bytes {
        return Err("kept file changed size since the scan".into());
    }
    let hash = hash_file(&group.keep).map_err(|e| format!("could not read kept file: {e}"))?;
    if hash != group.hash {
        return Err("kept file changed since the scan; group skipped".into());
    }
    Ok(meta)
}

/// Verify one duplicate and swap it for a clone or hardlink of `keep`.
fn replace_duplicate(
    group: &DedupGroup,
    keep_meta: &fs::Metadata,
    dup: &Path,
) -> Result<&'static str, String> {
    if dup == group.keep {
        return Err("duplicate is the kept file itself".into());
    }
    let meta = fs::symlink_metadata(dup).map_err(|e| format!("missing: {e}"))?;
    if !meta.file_type().is_file() {
        return Err("no longer a regular file".into());
    }
    if meta.len() != group.size_bytes {
        return Err("size changed since the scan; left untouched".into());
    }
    if let (Some(a), Some(b)) = (file_id(&meta), file_id(keep_meta)) {
        if a == b {
            return Err("already shares storage with the kept file".into());
        }
    }
    let hash = hash_file(dup).map_err(|e| format!("could not read: {e}"))?;
    if hash != group.hash {
        return Err("content changed since the scan; left untouched".into());
    }

    let tmp = tmp_sibling(dup)?;
    if fs::symlink_metadata(&tmp).is_ok() {
        // Not ours to delete: it may be something else entirely.
        return Err(format!(
            "temporary path {} already exists; remove it and retry",
            tmp.display()
        ));
    }

    let how = match link_into(&group.keep, &tmp) {
        Ok(how) => how,
        Err(reason) => {
            // A failed clone may have left a partial file at a path we
            // verified was free a moment ago.
            let _ = fs::remove_file(&tmp);
            return Err(reason);
        }
    };

    // Sanity check before the point of no return.
    let staged_ok = fs::symlink_metadata(&tmp)
        .map(|m| m.file_type().is_file() && m.len() == group.size_bytes)
        .unwrap_or(false);
    if !staged_ok {
        let _ = fs::remove_file(&tmp);
        return Err("staged replacement has the wrong size; left untouched".into());
    }

    if how == "clone" {
        // A clone is an independent inode: give it the duplicate's
        // permissions so nothing about the path changes but its storage.
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }

    if let Err(e) = fs::rename(&tmp, dup) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("could not replace: {e}"));
    }
    Ok(how)
}

/// `<dup>.void-dedup-tmp`, in the same directory so the rename is atomic.
fn tmp_sibling(dup: &Path) -> Result<PathBuf, String> {
    let name = dup
        .file_name()
        .ok_or_else(|| "duplicate path has no file name".to_string())?;
    let mut tmp_name = name.to_os_string();
    tmp_name.push(TMP_SUFFIX);
    Ok(dup.with_file_name(tmp_name))
}

/// Create `tmp` sharing `keep`'s storage: a clone if the filesystem supports
/// it, else a hardlink. Different volumes support neither, and the duplicate
/// is skipped rather than copied (a copy saves nothing).
fn link_into(keep: &Path, tmp: &Path) -> Result<&'static str, String> {
    if clone_file(keep, tmp) {
        return Ok("clone");
    }
    let _ = fs::remove_file(tmp);
    fs::hard_link(keep, tmp)
        .map(|_| "hardlink")
        .map_err(|e| format!("could not clone or hardlink (different volumes?): {e}"))
}

/// `cp -c` uses clonefile(2) on APFS.
#[cfg(target_os = "macos")]
fn clone_file(keep: &Path, tmp: &Path) -> bool {
    run_quiet(Command::new("/bin/cp").arg("-c").arg(keep).arg(tmp))
}

/// `cp --reflink=always` clones on btrfs/XFS and fails (rather than copying)
/// everywhere else.
#[cfg(target_os = "linux")]
fn clone_file(keep: &Path, tmp: &Path) -> bool {
    run_quiet(
        Command::new("cp")
            .arg("--reflink=always")
            .arg("--")
            .arg(keep)
            .arg(tmp),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clone_file(_keep: &Path, _tmp: &Path) -> bool {
    false
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run_quiet(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Full blake3 of a file, streamed.
pub fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// blake3 over the first and last [`PARTIAL_CHUNK`] bytes (the whole file
/// when it is small). Headers differ between quantizations and the tail
/// catches truncated downloads, so this discards nearly every false
/// candidate without reading gigabytes.
fn partial_hash(path: &Path, len: u64) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    if len <= PARTIAL_CHUNK * 2 {
        io::copy(&mut file, &mut hasher)?;
    } else {
        let mut buf = vec![0u8; PARTIAL_CHUNK as usize];
        file.read_exact(&mut buf)?;
        hasher.update(&buf);
        file.seek(SeekFrom::End(-(PARTIAL_CHUNK as i64)))?;
        file.read_exact(&mut buf)?;
        hasher.update(&buf);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// (device, inode) on Unix; `None` where it is not cheaply available.
fn file_id(meta: &fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn groups_identical_files() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write(tmp.path(), "a.gguf", &[7u8; 4096]);
        let b = write(tmp.path(), "b.gguf", &[7u8; 4096]);
        let c = write(tmp.path(), "c.gguf", &[7u8; 4096]);
        let groups = find_duplicates_sync(vec![c.clone(), a.clone(), b.clone()], 1);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].keep, a);
        assert_eq!(groups[0].duplicates, vec![b, c]);
        assert_eq!(groups[0].size_bytes, 4096);
        assert_eq!(groups[0].hash, hash_file(&a).unwrap());
    }

    #[test]
    fn same_size_different_content_is_not_a_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write(tmp.path(), "a", &[1u8; 4096]);
        let mut other = vec![1u8; 4096];
        other[2048] = 2; // differs only in the middle: partial hash agrees
        let b = write(tmp.path(), "b", &other);
        assert!(find_duplicates_sync(vec![a, b], 1).is_empty());
    }

    #[test]
    fn large_files_differing_in_the_middle_are_separated_by_the_full_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let len = (PARTIAL_CHUNK * 2 + 4096) as usize;
        let a = write(tmp.path(), "a", &vec![3u8; len]);
        let mut other = vec![3u8; len];
        other[len / 2] = 4;
        let b = write(tmp.path(), "b", &other);
        let c = write(tmp.path(), "c", &vec![3u8; len]);
        let groups = find_duplicates_sync(vec![a.clone(), b, c.clone()], 1);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].keep, a);
        assert_eq!(groups[0].duplicates, vec![c]);
    }

    #[cfg(unix)]
    #[test]
    fn hardlinks_to_the_same_inode_are_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write(tmp.path(), "a", &[5u8; 2048]);
        let b = tmp.path().join("b");
        fs::hard_link(&a, &b).unwrap();
        assert!(find_duplicates_sync(vec![a, b], 1).is_empty());
    }

    #[test]
    fn respects_min_size_and_ignores_symlinks_and_empty_files() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write(tmp.path(), "a", &[9u8; 100]);
        let b = write(tmp.path(), "b", &[9u8; 100]);
        assert!(find_duplicates_sync(vec![a.clone(), b.clone()], 101).is_empty());
        assert_eq!(
            find_duplicates_sync(vec![a.clone(), b.clone()], 100).len(),
            1
        );

        let e1 = write(tmp.path(), "e1", b"");
        let e2 = write(tmp.path(), "e2", b"");
        assert!(find_duplicates_sync(vec![e1, e2], 0).is_empty());

        #[cfg(unix)]
        {
            let link = tmp.path().join("link");
            std::os::unix::fs::symlink(&a, &link).unwrap();
            let groups = find_duplicates_sync(vec![a, b, link.clone()], 1);
            assert_eq!(groups.len(), 1);
            assert!(!groups[0].duplicates.contains(&link));
        }
    }

    #[test]
    fn apply_replaces_duplicate_and_keeps_content() {
        let tmp = tempfile::tempdir().unwrap();
        let content: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        let keep = write(tmp.path(), "a-keep.bin", &content);
        let dup = write(tmp.path(), "b-dup.bin", &content);
        let keep_mtime = fs::metadata(&keep).unwrap().modified().unwrap();

        let groups = find_duplicates_sync(vec![keep.clone(), dup.clone()], 1);
        assert_eq!(groups[0].keep, keep);
        let saved = apply_groups_sync(&groups).unwrap();
        assert_eq!(saved, 8192);

        assert_eq!(fs::read(&keep).unwrap(), content);
        assert_eq!(fs::read(&dup).unwrap(), content);
        assert_eq!(fs::metadata(&keep).unwrap().modified().unwrap(), keep_mtime);
        assert!(
            !tmp_sibling(&dup).unwrap().exists(),
            "temp file left behind"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let k = fs::metadata(&keep).unwrap();
            let d = fs::metadata(&dup).unwrap();
            // Either a hardlink (same inode) or a clone (distinct inode,
            // identical bytes — checked above).
            if k.ino() != d.ino() {
                assert_eq!(hash_file(&keep).unwrap(), hash_file(&dup).unwrap());
            }
        }
    }

    #[test]
    fn tampered_duplicate_is_left_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let keep = write(tmp.path(), "a-keep", &[1u8; 4096]);
        let dup = write(tmp.path(), "b-dup", &[1u8; 4096]);
        let groups = find_duplicates_sync(vec![keep, dup.clone()], 1);
        assert_eq!(groups.len(), 1);

        let mut changed = vec![1u8; 4096];
        changed[0] = 42;
        fs::write(&dup, &changed).unwrap();

        let err = apply_groups_sync(&groups).unwrap_err();
        assert!(err.contains("changed"), "{err}");
        assert_eq!(fs::read(&dup).unwrap(), changed);
    }

    #[test]
    fn missing_keep_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let keep = write(tmp.path(), "a-keep", &[1u8; 4096]);
        let dup = write(tmp.path(), "b-dup", &[1u8; 4096]);
        let groups = find_duplicates_sync(vec![keep.clone(), dup.clone()], 1);
        fs::remove_file(&keep).unwrap();
        let err = apply_groups_sync(&groups).unwrap_err();
        assert!(err.contains("missing"), "{err}");
        assert_eq!(fs::read(&dup).unwrap(), vec![1u8; 4096]);
    }

    #[test]
    fn partial_success_reports_only_replaced_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let keep = write(tmp.path(), "a", &[6u8; 1000]);
        let good = write(tmp.path(), "b", &[6u8; 1000]);
        let bad = write(tmp.path(), "c", &[6u8; 1000]);
        let groups = find_duplicates_sync(vec![keep, good.clone(), bad.clone()], 1);
        fs::write(&bad, [0u8; 1000]).unwrap();
        assert_eq!(apply_groups_sync(&groups).unwrap(), 1000);
        assert_eq!(fs::read(&good).unwrap(), vec![6u8; 1000]);
        assert_eq!(fs::read(&bad).unwrap(), vec![0u8; 1000]);
    }

    #[test]
    fn existing_temp_path_is_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let keep = write(tmp.path(), "a-keep", &[2u8; 512]);
        let dup = write(tmp.path(), "b-dup", &[2u8; 512]);
        let squatter = write(tmp.path(), "b-dup.void-dedup-tmp", b"not ours");
        let groups = find_duplicates_sync(vec![keep, dup], 1);
        assert!(apply_groups_sync(&groups).is_err());
        assert_eq!(fs::read(&squatter).unwrap(), b"not ours");
    }

    #[test]
    fn empty_groups_are_an_error() {
        assert!(apply_groups_sync(&[]).is_err());
    }

    #[tokio::test]
    async fn async_wrappers_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let keep = write(tmp.path(), "a-keep", &[8u8; 2048]);
        let dup = write(tmp.path(), "b-dup", &[8u8; 2048]);
        let groups = find_duplicates(vec![keep, dup], 1).await;
        assert_eq!(apply_groups(&groups).await.unwrap(), 2048);
    }
}
