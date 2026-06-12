//! Locked, atomic persistence for mutable local JSON state.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{Error, Result};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct StateLock {
    file: File,
}

impl StateLock {
    fn acquire(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| Error::Io("state path has no parent".to_string()))?;
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Io(format!("create state directory: {e}")))?;
        let lock_path = lock_path(path);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| Error::Io(format!("open state lock {}: {e}", lock_path.display())))?;
        set_owner_only(&lock_path)?;
        lock_exclusive(&file)?;
        Ok(Self { file })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = unlock(&self.file);
    }
}

fn lock_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    path.with_file_name(format!(".{name}.lock"))
}

pub(crate) fn load_json_or_default<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::Io(format!("parse state {}: {e}", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(Error::Io(format!("read state {}: {error}", path.display()))),
    }
}

pub(crate) fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let _lock = StateLock::acquire(path)?;
    save_json_unlocked(path, value)
}

pub(crate) fn update_json<T, R>(path: &Path, update: impl FnOnce(&mut T) -> Result<R>) -> Result<R>
where
    T: DeserializeOwned + Default + Serialize,
{
    let _lock = StateLock::acquire(path)?;
    let mut value = load_json_or_default(path)?;
    let result = update(&mut value)?;
    save_json_unlocked(path, &value)?;
    Ok(result)
}

fn save_json_unlocked<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| Error::Io(format!("serialize state {}: {e}", path.display())))?;
    atomic_write_unlocked(path, &bytes)
}

fn atomic_write_unlocked(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Io("state path has no parent".to_string()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| Error::Io(format!("create state directory: {e}")))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| Error::Io(format!("create state temp {}: {e}", tmp.display())))?;
        set_owner_only(&tmp)?;
        file.write_all(bytes)
            .map_err(|e| Error::Io(format!("write state temp {}: {e}", tmp.display())))?;
        file.sync_all()
            .map_err(|e| Error::Io(format!("sync state temp {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| Error::Io(format!("commit state {}: {e}", path.display())))?;
        set_owner_only(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
        Error::Io(format!(
            "set private state permissions {}: {e}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
        Ok(())
    } else {
        Err(Error::Io(format!(
            "lock state: {}",
            std::io::Error::last_os_error()
        )))
    }
}

#[cfg(not(unix))]
fn lock_exclusive(_file: &File) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn unlock(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn unlock(_file: &File) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    #[test]
    fn concurrent_updates_do_not_lose_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("state.json"));
        let mut joins = Vec::new();
        for index in 0..24 {
            let path = path.clone();
            joins.push(std::thread::spawn(move || {
                update_json::<BTreeSet<usize>, _>(&path, |set| {
                    set.insert(index);
                    Ok(())
                })
                .unwrap();
            }));
        }
        for join in joins {
            join.join().unwrap();
        }
        assert_eq!(
            load_json_or_default::<BTreeSet<usize>>(&path)
                .unwrap()
                .len(),
            24
        );
    }
}
