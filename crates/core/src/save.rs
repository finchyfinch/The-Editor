//! Writing a file without losing it.
//!
//! The obvious atomic save — write `<file>.tmp`, rename it over the original —
//! has four ways to destroy something, and the first version of this editor
//! had all of them:
//!
//! * **The temporary name belonged to somebody.** A user's own `notes.py.tmp`
//!   was overwritten by the save of `notes.py` and then renamed away. The
//!   temporary file is now created with `create_new` under a name no person
//!   would choose, so it can only ever be one this process made.
//! * **Nothing reached the disk before the rename.** On a power cut the rename
//!   can land before the data does, leaving an empty file where the old one
//!   was. The temporary file is flushed first.
//! * **A symlink was replaced by a plain file.** Renaming onto a link replaces
//!   the link, not what it points at, so the next edit to the real file went
//!   unseen. The link is followed and its target is written instead.
//! * **A hard link was split in two.** Renaming creates a new file, so the other
//!   names for the old one kept the old contents. A file with more than one
//!   name is written in place instead, after a flushed copy has been put aside
//!   so that an interrupted write still leaves the work somewhere.
//!
//! On Unix the original's permissions are carried across as well, or saving a
//! script would quietly take away its executable bit.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

/// Write `bytes` to `path`, following a symlink and keeping any hard links.
///
/// # Errors
/// If the directory cannot be created or any write, flush or rename fails.
/// When an in-place write fails part-way the error names the copy that was put
/// aside first, because at that moment it is the only complete one.
pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let target = follow_symlink(path);

    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;

    let existing = fs::metadata(&target).ok();
    let (tmp, mut file) = create_temporary(&target)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("writing {}", tmp.display()));
    drop(file);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    if existing
        .as_ref()
        .is_some_and(|m| link_count(&target, m) > 1)
    {
        return write_in_place(&target, bytes, &tmp);
    }

    #[cfg(unix)]
    if let Some(meta) = &existing {
        // Best effort: a filesystem that refuses the mode still gets the save.
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }

    if let Err(e) = fs::rename(&tmp, &target) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("replacing {}", target.display()));
    }
    sync_directory(parent);
    Ok(())
}

/// The file a write to `path` should actually change.
///
/// A dangling link resolves to where it points, so saving creates the file the
/// link was waiting for rather than replacing the link.
fn follow_symlink(path: &Path) -> PathBuf {
    let is_link = fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    if !is_link {
        return path.to_path_buf();
    }
    if let Ok(real) = fs::canonicalize(path) {
        return real;
    }
    match fs::read_link(path) {
        Ok(target) if target.is_absolute() => target,
        Ok(target) => path.parent().map_or(target.clone(), |dir| dir.join(target)),
        Err(_) => path.to_path_buf(),
    }
}

/// Make a new, empty temporary file beside `target`.
///
/// `create_new` is what guarantees the file is ours: if the name is taken, the
/// next one is tried rather than the existing file being opened.
fn create_temporary(target: &Path) -> Result<(PathBuf, File)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .map_or_else(|| "untitled".into(), |n| n.to_string_lossy().into_owned());

    let mut last_error = None;
    for _ in 0..100 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Ends in `.tmp`, which the file watcher already ignores.
        let tmp = dir.join(format!(".{name}.{}-{n}.the-editor.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_error = Some(e),
            Err(e) => return Err(e).with_context(|| format!("creating {}", tmp.display())),
        }
    }
    Err(last_error.map_or_else(
        || anyhow::anyhow!("no free temporary name"),
        anyhow::Error::from,
    ))
    .with_context(|| format!("finding a temporary name beside {}", target.display()))
}

/// Overwrite a hard-linked file where it stands, so every name for it sees the
/// new contents. `backup` already holds them, flushed, in case this fails.
fn write_in_place(target: &Path, bytes: &[u8], backup: &Path) -> Result<()> {
    let result = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(target)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        });
    match result {
        Ok(()) => {
            let _ = fs::remove_file(backup);
            Ok(())
        }
        Err(e) => Err(e).with_context(|| {
            format!(
                "writing {} in place; the complete new contents are in {}",
                target.display(),
                backup.display()
            )
        }),
    }
}

/// How many names the file has. One wherever that cannot be found out, which
/// falls back to the ordinary atomic save.
#[cfg(unix)]
fn link_count(_path: &Path, meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.nlink()
}

#[cfg(windows)]
fn link_count(path: &Path, _meta: &fs::Metadata) -> u64 {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    // `std` has this as `MetadataExt::number_of_links`, still unstable.
    let Ok(file) = File::open(path) else {
        return 1;
    };
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: the handle is open for the duration of the call, and `info` is a
    // correctly sized, writable structure of the type the function fills in.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) };
    if ok == 0 {
        return 1;
    }
    // SAFETY: the call succeeded, so it initialised the structure.
    u64::from(unsafe { info.assume_init() }.nNumberOfLinks)
}

#[cfg(not(any(unix, windows)))]
fn link_count(_path: &Path, _meta: &fs::Metadata) -> u64 {
    1
}

/// Flush the directory entry the rename just changed. Unix only; Windows has
/// no way to open a directory for this and commits renames itself.
fn sync_directory(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("the-editor-save-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn a_new_file_is_created_with_the_bytes_given() {
        let dir = scratch("new");
        let path = dir.join("a.py");
        write(&path, b"print(1)\n").expect("save");
        assert_eq!(fs::read(&path).expect("read"), b"print(1)\n");
        fs::remove_dir_all(&dir).ok();
    }

    /// The bug this module was written for: the temporary name was
    /// `<file>.tmp`, so a file the user had given that name was overwritten and
    /// then renamed away.
    #[test]
    fn a_file_already_named_like_the_old_temporary_file_survives() {
        let dir = scratch("tmpname");
        let path = dir.join("notes.py");
        let bystander = dir.join("notes.py.tmp");
        fs::write(&path, b"v1\n").expect("write");
        fs::write(&bystander, b"mine\n").expect("write");

        write(&path, b"v2\n").expect("save");
        assert_eq!(fs::read(&path).expect("read"), b"v2\n");
        assert_eq!(fs::read(&bystander).expect("still there"), b"mine\n");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nothing_is_left_behind_after_a_save() {
        let dir = scratch("clean");
        let path = dir.join("a.txt");
        fs::write(&path, b"old").expect("write");
        write(&path, b"new").expect("save");
        let names: Vec<_> = fs::read_dir(&dir)
            .expect("list")
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("a.txt")]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn every_name_of_a_hard_linked_file_sees_the_save() {
        let dir = scratch("hardlink");
        let a = dir.join("a.py");
        let b = dir.join("b.py");
        fs::write(&a, b"x = 1\n").expect("write");
        fs::hard_link(&a, &b).expect("link");

        write(&a, b"x = 2\n").expect("save");
        assert_eq!(fs::read(&a).expect("read a"), b"x = 2\n");
        assert_eq!(
            fs::read(&b).expect("read b"),
            b"x = 2\n",
            "the other name must not be left with the old contents"
        );
        assert_eq!(
            fs::read_dir(&dir).expect("list").count(),
            2,
            "no backup left"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_directory_is_created() {
        let dir = scratch("mkdir");
        let path = dir.join("deep").join("er").join("a.txt");
        write(&path, b"x").expect("save");
        assert_eq!(fs::read(&path).expect("read"), b"x");
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn saving_through_a_symlink_changes_the_file_and_keeps_the_link() {
        let dir = scratch("symlink");
        let real = dir.join("real.py");
        let link = dir.join("link.py");
        fs::write(&real, b"old\n").expect("write");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        write(&link, b"new\n").expect("save");
        assert!(
            fs::symlink_metadata(&link)
                .expect("meta")
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real).expect("read"), b"new\n");
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn saving_keeps_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("perms");
        let path = dir.join("run.sh");
        fs::write(&path, b"#!/bin/sh\n").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");

        write(&path, b"#!/bin/sh\necho hi\n").expect("save");
        let mode = fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        fs::remove_dir_all(&dir).ok();
    }
}
