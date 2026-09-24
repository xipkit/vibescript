//! Script reading and module search paths for the Go-style commands.

use crate::compat;
use std::{
    borrow::Cow,
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// The largest script the commands compile, as the reference engine's default
/// `MaxSourceBytes`.
pub const MAX_SOURCE_BYTES: u64 = 1 << 20;

/// Reads a script through one descriptor, rejecting non-regular files and
/// sources over [`MAX_SOURCE_BYTES`] before loading them.
///
/// Invalid UTF-8 is replaced with U+FFFD, as the reference lexer decodes it.
pub fn read(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|error| compat::path_error("open", path, &error))?;
    let metadata = file
        .metadata()
        .map_err(|error| compat::path_error("stat", path, &error))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "source exceeds maximum size ({} > {MAX_SOURCE_BYTES} bytes)",
            metadata.len()
        ));
    }
    let mut data = Vec::new();
    file.by_ref()
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|error| compat::path_error("read", path, &error))?;
    if data.len() as u64 > MAX_SOURCE_BYTES {
        return Err(format!(
            "source exceeds maximum size (> {MAX_SOURCE_BYTES} bytes)"
        ));
    }
    Ok(match String::from_utf8_lossy(&data) {
        Cow::Borrowed(_) => String::from_utf8(data).expect("validated above"),
        Cow::Owned(text) => text,
    })
}

/// Builds the module search path: `base` first, then each extra directory,
/// all absolute and deduplicated by spelling, as the reference does.
pub fn module_paths(base: &Path, extras: &[OsString]) -> Result<Vec<PathBuf>, String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut add = |label: &str, path: &Path| -> Result<(), String> {
        let absolute = compat::absolute(path).map_err(|error| {
            format!(
                "resolve {label} {}: {}",
                compat::quote(&compat::bytes(path.as_os_str())),
                compat::reason(&error)
            )
        })?;
        let quoted = compat::quote(&compat::bytes(absolute.as_os_str()));
        let metadata = fs::metadata(&absolute).map_err(|error| {
            format!(
                "access {label} {quoted}: {}",
                compat::path_error("stat", &absolute, &error)
            )
        })?;
        if !metadata.is_dir() {
            return Err(format!("{label} {quoted} is not a directory"));
        }
        if !paths.contains(&absolute) {
            paths.push(absolute);
        }
        Ok(())
    };
    add("script directory", base)?;
    for extra in extras {
        add("module path", Path::new(extra))?;
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The reference's property: the script directory first, then each extra
    /// directory in order, absolute and without repeats.
    #[test]
    fn module_paths_match_the_directory_model() {
        // WASI has neither process ids nor canonical paths below a preopen,
        // so the directory is named by time below the repository's cache.
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".cache/tmp");
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let scratch = Scratch(base.join(format!("module-paths-{stamp}")));
        let choices: Vec<PathBuf> = ["scripts", "modules-a", "modules-b"]
            .iter()
            .map(|name| scratch.0.join(name))
            .collect();
        for dir in &choices {
            fs::create_dir_all(dir).unwrap();
        }
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        for _ in 0..200 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let extras: Vec<OsString> = (0..state % 12)
                .map(|i| choices[((state >> (i * 3)) % 3) as usize].clone().into())
                .collect();
            let mut model: Vec<PathBuf> = Vec::new();
            for dir in std::iter::once(&choices[0].clone().into_os_string()).chain(&extras) {
                let dir = PathBuf::from(dir);
                if !model.contains(&dir) {
                    model.push(dir);
                }
            }
            assert_eq!(module_paths(&choices[0], &extras).unwrap(), model);
        }
        let file = scratch.0.join("file");
        fs::write(&file, "x").unwrap();
        let quoted = compat::quote(file.to_str().unwrap().as_bytes());
        assert_eq!(
            module_paths(&choices[0], &[file.clone().into()]).unwrap_err(),
            format!("module path {quoted} is not a directory")
        );
    }
}
