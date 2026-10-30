//! Per-profile nonblocking process lock; the inode remains in place across launches.
use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::Path;

pub struct InstanceLock { file: File }
impl InstanceLock {
    pub fn acquire() -> Result<Option<Self>> { Self::acquire_in(&crate::paths::config_dir()?) }
    fn acquire_in(directory: &Path) -> Result<Option<Self>> {
        crate::paths::private_directory(directory)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC); }
        let file = options.open(directory.join("instance.lock")).context("Cannot open the profile instance lock")?;
        if !file.metadata()?.is_file() { anyhow::bail!("The profile instance lock is not a regular file"); }
        // fs2 uses flock on Unix and process-shared LockFileEx on Windows, not a process-local mutex.
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self { file })),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => Ok(None),
            Err(error) => Err(error).context("Cannot acquire the profile instance lock"),
        }
    }
}
impl Drop for InstanceLock {
    fn drop(&mut self) { let _ = FileExt::unlock(&self.file); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_handles_contend_until_owner_drops() {
        let root = tempfile::tempdir().unwrap();
        let first = InstanceLock::acquire_in(root.path()).unwrap().unwrap();
        assert!(InstanceLock::acquire_in(root.path()).unwrap().is_none());
        let other = root.path().join("another-profile");
        assert!(InstanceLock::acquire_in(&other).unwrap().is_some());
        drop(first);
        assert!(root.path().join("instance.lock").is_file());
        assert!(InstanceLock::acquire_in(root.path()).unwrap().is_some());
    }
    #[test]
    fn cross_process_lock_child() {
        let Some(root) = std::env::var_os("BROTYPist_LOCK_TEST_CHILD") else { return; };
        assert!(InstanceLock::acquire_in(Path::new(&root)).unwrap().is_none());
    }
    #[test]
    fn another_process_cannot_share_ownership() {
        let root = tempfile::tempdir().unwrap();
        let _first = InstanceLock::acquire_in(root.path()).unwrap().unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "instance::tests::cross_process_lock_child", "--nocapture"])
            .env("BROTYPist_LOCK_TEST_CHILD", root.path()).status().unwrap();
        assert!(result.success());
    }
}
