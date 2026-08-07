use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use fs2::FileExt;
use thiserror::Error;

const BUNDLE_DIRECTORY: &str = "espeak-ng-1.53.0-data-v1";
const DATA_DIRECTORY: &str = "espeak-ng-data";
const MARKER: &str = ".utterpipe-bundle";
const MARKER_CONTENTS: &str = "utterpipe-espeak-ng\nespeak-ng=359f5f397b85baf875089d3af9cda946bef31dcb\ndata=1.52.0.1\nlayout=1\n";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("the bundled eSpeak NG data could not be prepared")]
    Io(#[from] io::Error),
}

/// Materialize immutable, reconstructible engine data below the host-provided cache.
///
/// A versioned directory and an inter-process lock let provider instances share the
/// same files safely. The complete directory is published with one atomic rename.
///
/// # Errors
///
/// Returns [`BundleError`] when the cache cannot be locked, written, validated,
/// or atomically published.
pub fn ensure_bundled_data(cache_dir: &Path) -> Result<PathBuf, BundleError> {
    fs::create_dir_all(cache_dir)?;
    let lock = File::create(cache_dir.join(".utterpipe-espeak-ng.lock"))?;
    lock.lock_exclusive()?;

    let result = prepare_locked(cache_dir);
    let unlock_result = FileExt::unlock(&lock);
    match (result, unlock_result) {
        (Ok(path), Ok(())) => Ok(path),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

fn prepare_locked(cache_dir: &Path) -> Result<PathBuf, BundleError> {
    let final_root = cache_dir.join(BUNDLE_DIRECTORY);
    if valid_bundle(&final_root) {
        return Ok(final_root);
    }
    if final_root.exists() {
        fs::remove_dir_all(&final_root)?;
    }

    let temporary_root = unique_temporary_directory(cache_dir)?;
    let mut cleanup = TemporaryDirectory(Some(temporary_root.clone()));
    let data_dir = temporary_root.join(DATA_DIRECTORY);
    fs::create_dir_all(&data_dir)?;
    espeak_ng_data_phonemes::install(&data_dir)?;
    espeak_ng_data_dicts::install(&data_dir)?;
    espeak_ng_data_dict_ru::install(&data_dir)?;
    fs::write(temporary_root.join(MARKER), MARKER_CONTENTS)?;
    if !valid_bundle(&temporary_root) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "incomplete bundle").into());
    }
    fs::rename(&temporary_root, &final_root)?;
    cleanup.0 = None;
    Ok(final_root)
}

fn unique_temporary_directory(cache_dir: &Path) -> io::Result<PathBuf> {
    for _ in 0..32 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = cache_dir.join(format!(
            ".utterpipe-espeak-ng-{}-{sequence}.tmp",
            std::process::id()
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a temporary bundle directory",
    ))
}

fn valid_bundle(root: &Path) -> bool {
    fs::read_to_string(root.join(MARKER)).is_ok_and(|marker| marker == MARKER_CONTENTS)
        && root.join(DATA_DIRECTORY).join("phondata").is_file()
        && root.join(DATA_DIRECTORY).join("phontab").is_file()
        && root.join(DATA_DIRECTORY).join("en_dict").is_file()
        && root
            .join(DATA_DIRECTORY)
            .join("lang")
            .join("gmw/en")
            .is_file()
}

struct TemporaryDirectory(Option<PathBuf>);

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_is_idempotent_and_complete() {
        let temporary = tempfile::TempDir::new().unwrap();
        let first = ensure_bundled_data(temporary.path()).unwrap();
        let second = ensure_bundled_data(temporary.path()).unwrap();
        assert_eq!(first, second);
        assert!(valid_bundle(&first));
    }
}
