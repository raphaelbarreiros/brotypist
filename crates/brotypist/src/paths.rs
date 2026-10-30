//! Existing platformdirs profile identities. Resolving a path never creates it.
use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
#[cfg(any(windows, test))]
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub fn config_dir() -> Result<PathBuf> {
    if let Some(path) = home_override()? {
        return Ok(path);
    }
    platform_config_dir()
}

pub fn cache_dir() -> Result<PathBuf> {
    platform_cache_dir()
}
pub fn model_dir() -> Result<PathBuf> {
    Ok(cache_dir()?.join("models"))
}
pub fn update_dir() -> Result<PathBuf> {
    Ok(match home_override()? {
        Some(path) => path,
        None => cache_dir()?,
    }
    .join("updates"))
}

fn home_override() -> Result<Option<PathBuf>> {
    let value = std::env::var_os("INKLOW_HOME");
    override_path(value.as_deref())
}

fn override_path(value: Option<&OsStr>) -> Result<Option<PathBuf>> {
    value.filter(|s| !s.is_empty()).map(expand_user).transpose()
}

pub(crate) fn expand_user(value: &OsStr) -> Result<PathBuf> {
    let path = Path::new(value);
    if path == Path::new("~") {
        return home_dir();
    }
    if let Ok(rest) = path.strip_prefix("~") {
        return Ok(home_dir()?.join(rest));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = value.as_bytes();
        if bytes.first() == Some(&b'~') {
            let end = bytes.iter().position(|&b| b == b'/').unwrap_or(bytes.len());
            let username = std::ffi::CString::new(&bytes[1..end])?;
            // getpwnam_r owns its scratch buffer and avoids process-global passwd state.
            let mut storage = vec![0u8; 16384];
            loop {
                let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
                let mut found = std::ptr::null_mut();
                let status = unsafe {
                    libc::getpwnam_r(
                        username.as_ptr(),
                        pwd.as_mut_ptr(),
                        storage.as_mut_ptr().cast(),
                        storage.len(),
                        &mut found,
                    )
                };
                if status == libc::ERANGE && storage.len() < 1024 * 1024 {
                    storage.resize(storage.len() * 2, 0);
                    continue;
                }
                if status != 0 {
                    return Err(std::io::Error::from_raw_os_error(status).into());
                }
                if found.is_null() {
                    bail!("Cannot resolve home directory for {}", path.display());
                }
                let pwd = unsafe { pwd.assume_init() };
                let home = unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) }.to_bytes();
                let mut result = PathBuf::from(OsStr::from_bytes(home));
                if end < bytes.len() {
                    result.push(OsStr::from_bytes(&bytes[end + 1..]));
                }
                return Ok(result);
            }
        }
    }
    Ok(path.to_path_buf())
}

pub(crate) fn home_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    if let Some(home) = std::env::var_os("USERPROFILE").filter(|p| !p.is_empty()) {
        return Ok(home.into());
    }
    if let Some(home) = std::env::var_os("HOME").filter(|p| !p.is_empty()) {
        return Ok(home.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut storage = vec![0u8; 16384];
        loop {
            let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
            let mut found = std::ptr::null_mut();
            let status = unsafe {
                libc::getpwuid_r(
                    libc::getuid(),
                    pwd.as_mut_ptr(),
                    storage.as_mut_ptr().cast(),
                    storage.len(),
                    &mut found,
                )
            };
            if status == libc::ERANGE && storage.len() < 1024 * 1024 {
                storage.resize(storage.len() * 2, 0);
                continue;
            }
            if status != 0 {
                return Err(std::io::Error::from_raw_os_error(status).into());
            }
            if found.is_null() {
                bail!("Cannot resolve the current user's home directory");
            }
            let pwd = unsafe { pwd.assume_init() };
            return Ok(PathBuf::from(OsStr::from_bytes(
                unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) }.to_bytes(),
            )));
        }
    }
    #[cfg(not(unix))]
    bail!("Cannot resolve the current user's home directory")
}

#[cfg(target_os = "macos")]
pub(crate) fn platform_config_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/Application Support/inklow"))
}
#[cfg(target_os = "macos")]
fn platform_cache_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/Caches/inklow"))
}
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn platform_config_dir() -> Result<PathBuf> {
    Ok(xdg_base("XDG_CONFIG_HOME", ".config")?.join("inklow"))
}
#[cfg(all(unix, not(target_os = "macos")))]
fn platform_cache_dir() -> Result<PathBuf> {
    Ok(xdg_base("XDG_CACHE_HOME", ".cache")?.join("inklow"))
}
#[cfg(all(unix, not(target_os = "macos")))]
fn xdg_base(name: &str, fallback: &str) -> Result<PathBuf> {
    Ok(match std::env::var_os(name).filter(|p| !p.is_empty()) {
        Some(path) => path.into(),
        None => home_dir()?.join(fallback),
    })
}
#[cfg(windows)]
pub(crate) fn platform_config_dir() -> Result<PathBuf> {
    Ok(local_app_data()?.join("inklow"))
}
#[cfg(windows)]
fn platform_cache_dir() -> Result<PathBuf> {
    Ok(local_app_data()?.join("inklow/Cache"))
}

#[cfg(windows)]
fn local_app_data() -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    #[repr(C)]
    struct Guid {
        a: u32,
        b: u16,
        c: u16,
        d: [u8; 8],
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(
            id: *const Guid,
            flags: u32,
            token: *mut std::ffi::c_void,
            path: *mut *mut u16,
        ) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(pointer: *mut std::ffi::c_void);
    }
    const LOCAL_APP_DATA: Guid = Guid {
        a: 0xf1b32785,
        b: 0x6fba,
        c: 0x4fcf,
        d: [0x9d, 0x55, 0x7b, 0x8e, 0x7f, 0x15, 0x70, 0x91],
    };
    let mut pointer = std::ptr::null_mut();
    let status =
        unsafe { SHGetKnownFolderPath(&LOCAL_APP_DATA, 0, std::ptr::null_mut(), &mut pointer) };
    if status < 0 || pointer.is_null() {
        bail!("Cannot resolve LocalAppData (HRESULT {status:#x})");
    }
    let mut length = 0;
    while unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    let value = OsString::from_wide(unsafe { std::slice::from_raw_parts(pointer, length) });
    unsafe { CoTaskMemFree(pointer.cast()) };
    Ok(value.into())
}

/// Create only missing directories privately; never chmod an existing profile.
pub(crate) fn private_directory(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .with_context(|| format!("Cannot create private directory {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    // Tests inject paths directly rather than mutate process-global environment.
    #[test]
    fn unicode_and_relative_paths_are_preserved() {
        let value = OsString::from("profiles/João/設定");
        assert_eq!(
            expand_user(&value).unwrap(),
            PathBuf::from("profiles/João/設定")
        );
        assert_eq!(
            expand_user(OsStr::new("~/設定")).unwrap(),
            home_dir().unwrap().join("設定")
        );
    }
    #[test]
    fn override_is_expanded_without_rebranding_or_redirecting_cache() {
        assert_eq!(override_path(None).unwrap(), None);
        assert_eq!(override_path(Some(OsStr::new(""))).unwrap(), None);
        assert_eq!(
            override_path(Some(OsStr::new("~/João/設定"))).unwrap(),
            Some(home_dir().unwrap().join("João/設定"))
        );
        let profile = override_path(Some(OsStr::new("isolated/profile")))
            .unwrap()
            .unwrap();
        assert_eq!(
            profile.join("settings.json"),
            PathBuf::from("isolated/profile/settings.json")
        );
        assert_eq!(
            profile.join("updates"),
            PathBuf::from("isolated/profile/updates")
        );
        assert_ne!(platform_cache_dir().unwrap(), profile);
    }
    #[test]
    fn native_paths_keep_existing_brand_and_cache_is_separate() {
        let config = platform_config_dir().unwrap();
        let cache = platform_cache_dir().unwrap();
        assert!(config.ends_with("inklow"));
        #[cfg(not(windows))]
        assert!(cache.ends_with("inklow"));
        #[cfg(windows)]
        assert!(cache.ends_with("inklow/Cache"));
        assert_ne!(config, cache);
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                config,
                home_dir()
                    .unwrap()
                    .join("Library/Application Support/inklow")
            );
            assert_eq!(cache, home_dir().unwrap().join("Library/Caches/inklow"));
        }
    }
    #[test]
    fn creation_is_private_without_creating_profile_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("私的/profile");
        private_directory(&path).unwrap();
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
}
