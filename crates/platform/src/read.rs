//! Bounded reads of regular files, never a FIFO/device pretending to be an input file.
use std::{
    fs::{File, OpenOptions},
    io::{self, Read},
    path::Path,
};

fn regular(file: &File, limit: usize) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "input must be a regular file"));
    }
    if metadata.len() > limit as u64 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "input exceeds its byte limit"));
    }
    Ok(())
}

fn open(path: &Path, limit: usize) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    // A file can be replaced with a FIFO between metadata and open. NONBLOCK prevents that
    // replacement from hanging on open; the opened handle is checked again before reading.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    regular(&file, limit)?;
    Ok(file)
}

/// Read at most `limit` bytes, checking cancellation/deadlines before opening and between
/// 64 KiB chunks. An oversized, growing, non-regular or cancelled input produces no result.
/// File symlinks remain supported; the opened target must be a regular file too.
pub fn regular_file(path: &Path, limit: usize, mut check: impl FnMut() -> io::Result<()>) -> io::Result<Vec<u8>> {
    check()?;
    if !std::fs::metadata(path)?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "input must be a regular file"));
    }
    let mut file = open(path, limit)?;
    let mut bytes = Vec::new();
    let mut chunk = [0; 64 * 1024];
    loop {
        check()?;
        let n = file.read(&mut chunk)?;
        if n == 0 {
            return Ok(bytes);
        }
        if n > limit.saturating_sub(bytes.len()) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "input exceeds its byte limit"));
        }
        bytes.try_reserve(n).map_err(io::Error::other)?;
        bytes.extend_from_slice(&chunk[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scratch() -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pdfcraft-bounded-read-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        dir
    }
    #[test]
    fn exact_limit_empty_and_oversized_files() {
        let dir = scratch();
        let path = dir.join("identity.p12");
        for n in [0, 65_536, 65_537] {
            std::fs::write(&path, vec![42; n]).unwrap();
            let got = regular_file(&path, 65_536, || Ok(()));
            if n <= 65_536 {
                assert_eq!(got.unwrap().len(), n);
            } else {
                assert!(got.is_err());
            }
        }
        assert!(regular_file(&dir, 10, || Ok(())).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn cancellation_is_checked_between_chunks() {
        let dir = scratch();
        let path = dir.join("identity.p12");
        std::fs::write(&path, vec![42; 131_072]).unwrap();
        let mut calls = 0;
        let got = regular_file(&path, 131_072, || {
            calls += 1;
            if calls >= 3 { Err(io::ErrorKind::Interrupted.into()) } else { Ok(()) }
        });
        assert_eq!(got.unwrap_err().kind(), io::ErrorKind::Interrupted);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn growth_after_metadata_is_refused_before_excess_is_copied() {
        let dir = scratch();
        let path = dir.join("identity.p12");
        std::fs::write(&path, [42]).unwrap();
        let mut calls = 0;
        let got = regular_file(&path, 1, || {
            calls += 1;
            if calls == 2 {
                std::fs::write(&path, [42, 43])?;
            }
            Ok(())
        });
        assert_eq!(got.unwrap_err().kind(), io::ErrorKind::InvalidData);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn a_fifo_cannot_block_even_the_post_metadata_open() {
        let dir = scratch();
        let path = dir.join("identity.p12");
        assert!(std::process::Command::new("mkfifo").arg(&path).status().unwrap().success());
        let start = std::time::Instant::now();
        assert!(regular_file(&path, 1, || Ok(())).is_err());
        assert!(open(&path, 1).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
