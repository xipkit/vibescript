use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

pub(super) struct Directory(pub PathBuf);

impl Directory {
    pub fn new() -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(".cache/tmp");
        fs::create_dir_all(&base).unwrap();
        let base = canonical(&base);
        loop {
            let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("module-files-{}-{serial}", process_id()));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create module fixture directory: {error}"),
            }
        }
    }

    pub fn write(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let result = fs::remove_dir_all(&self.0);
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}

/// Canonicalizes an existing directory the way the loader resolves roots,
/// which also works beneath WASI preopens whose ancestors are hidden.
pub(super) fn canonical(path: &Path) -> PathBuf {
    super::files::Root::new(path).unwrap().path().to_owned()
}

/// Distinguishes this process's fixture directories from those of other test
/// runs. WASI has no process IDs, so it uses the current time instead.
pub(crate) fn process_id() -> u128 {
    #[cfg(not(target_os = "wasi"))]
    return std::process::id().into();
    #[cfg(target_os = "wasi")]
    return std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
}
