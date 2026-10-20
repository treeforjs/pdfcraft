//! Staging files for atomic saves: a new, unpredictably named file beside the target, written and
//! then renamed over it. Writing, closing, renaming and cleaning up stay with the caller.

use std::path::{Path, PathBuf};

/// How many staging names [`create_staging`] tries. A random 64-bit name is only taken if someone
/// put a file there on purpose, so running out means refusing, not trying harder.
pub const STAGING_ATTEMPTS: usize = 16;

/// Where the suffix goes in a staging name. Each caller keeps the form it has always used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingName {
    /// `.{name}.{suffix}.pdfcraft-tmp`, used by the automation tools.
    SuffixThenTag,
    /// `.{name}.pdfcraft-{suffix}.tmp`, used by the desktop app's save.
    TagThenSuffix,
}

/// Create a new, empty staging file in `dir` for the file `name`, one name per suffix. It is
/// opened with `create_new`, which fails if anything already has the name (a file, a hard link,
/// a symbolic link even when dangling, a folder), on Windows as everywhere else; such a name is
/// skipped, never opened, so a file planted at the staging path can't receive or redirect the
/// write. Pass [`staging_suffixes`]: fixed suffixes are for tests, and a predictable name can be
/// taken in advance to refuse every save.
pub fn create_staging(
    dir: &Path,
    name: &str,
    form: StagingName,
    suffixes: impl IntoIterator<Item = u64>,
) -> std::io::Result<(PathBuf, std::fs::File)> {
    // At most 128 bytes of the target's name, cut between characters, so the staging name fits
    // the 255-byte (Linux, macOS) and 255-unit (Windows) limits however long that name is.
    let mut stem = String::new();
    for c in name.chars() {
        if stem.len() + c.len_utf8() > 128 {
            break;
        }
        stem.push(c);
    }
    for suffix in suffixes.into_iter().take(STAGING_ATTEMPTS) {
        let tmp = dir.join(match form {
            StagingName::SuffixThenTag => format!(".{stem}.{suffix:016x}.pdfcraft-tmp"),
            StagingName::TagThenSuffix => format!(".{stem}.pdfcraft-{suffix:016x}.tmp"),
        });
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            // Windows reports a folder at the name as "access denied"; it is taken all the same.
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && tmp.symlink_metadata().is_ok() => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "every temporary file name tried is taken"))
}

/// Preserve an existing regular document's Unix access mode before replacing it. New files
/// retain the staging file's private mode. Links are replaced without copying target metadata.
pub fn preserve_permissions(file: &std::fs::File, target: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match target.symlink_metadata() {
            Ok(metadata) if metadata.is_file() => file.set_permissions(std::fs::Permissions::from_mode(metadata.permissions().mode() & 0o777))?,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    #[cfg(not(unix))]
    let _ = (file, target);
    Ok(())
}

/// Unpredictable staging-name suffixes, so a name can't be planted in advance. Each is the
/// standard library's keyed hash of a counter (`RandomState::hash_one`, currently SipHash-1-3),
/// keyed from the operating system's random source. That is all a staging name needs, but it is
/// not a CSPRNG: don't use these where a value must stay secret or unguessable after it has been
/// seen, such as a token or a key.
pub fn staging_suffixes() -> impl Iterator<Item = u64> {
    use std::hash::BuildHasher;
    let state = std::hash::RandomState::new();
    (0u64..).map(move |i| state.hash_one(i))
}

/// Choose an available output path, adding ` (2)`, ` (3)`, ... before the extension.
/// Existing files, folders and dangling symlinks reserve their names. Exhausting the bounded
/// search returns an error, never an existing path. The caller still performs the write.
pub fn unused_output_path(path: &Path) -> std::io::Result<PathBuf> {
    let stem = path.file_stem().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "output needs a file name"))?;
    for n in 1..10_000u32 {
        let candidate = if n == 1 {
            path.to_path_buf()
        } else {
            let mut name = stem.to_os_string();
            name.push(format!(" ({n})"));
            if let Some(extension) = path.extension() {
                name.push(".");
                name.push(extension);
            }
            path.with_file_name(name)
        };
        match candidate.symlink_metadata() {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("no unused output name for {}; choose another folder", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn staging_is_private_and_preserves_existing_document_access() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir("permissions");
        let target = dir.join("private.pdf");
        std::fs::write(&target, b"old").unwrap();
        for mode in [0o600, 0o640] {
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode)).unwrap();
            let (staged, file) = create_staging(&dir, "private.pdf", StagingName::TagThenSuffix, [mode as u64]).unwrap();
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            preserve_permissions(&file, &target).unwrap();
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, mode);
            drop(file);
            std::fs::remove_file(staged).unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A fresh, empty folder for one test.
    fn test_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pdfcraft-platform-staging-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_name(p: &Path) -> String {
        p.file_name().unwrap().to_string_lossy().into_owned()
    }

    /// The callers' tests plant files at these exact names; if a name changed, those tests would
    /// plant where nothing writes and pass without testing anything.
    #[test]
    fn each_form_keeps_its_exact_name() {
        let dir = test_dir("forms");
        let (tools, _) = create_staging(&dir, "out.pdf", StagingName::SuffixThenTag, [0xab]).unwrap();
        assert_eq!(file_name(&tools), ".out.pdf.00000000000000ab.pdfcraft-tmp");
        let (desktop, _) = create_staging(&dir, "out.pdf", StagingName::TagThenSuffix, [0xab]).unwrap();
        assert_eq!(file_name(&desktop), ".out.pdf.pdfcraft-00000000000000ab.tmp");
        assert_eq!(std::fs::read(&tools).unwrap(), b"", "a new, empty file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_name_is_cut_to_128_bytes_between_characters() {
        let dir = test_dir("long");
        // One byte, then four-byte characters: the 128-byte limit falls inside the 32nd one.
        let name = format!("a{}.pdf", "\u{1F600}".repeat(40));
        let (tmp, _) = create_staging(&dir, &name, StagingName::SuffixThenTag, [1]).unwrap();
        assert_eq!(file_name(&tmp), format!(".a{}.0000000000000001.pdfcraft-tmp", "\u{1F600}".repeat(31)));
        // One-byte characters: exactly 128 of them are kept.
        let (tmp, _) = create_staging(&dir, &"x".repeat(200), StagingName::SuffixThenTag, [2]).unwrap();
        assert_eq!(file_name(&tmp), format!(".{}.0000000000000002.pdfcraft-tmp", "x".repeat(128)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn running_out_of_suffixes_refuses_and_creates_nothing() {
        let dir = test_dir("empty");
        let e = create_staging(&dir, "out.pdf", StagingName::TagThenSuffix, []).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_first_sixteen_names_are_tried() {
        let dir = test_dir("cap");
        let name = |s: u64| dir.join(format!(".out.pdf.{s:016x}.pdfcraft-tmp"));
        for s in 1..=STAGING_ATTEMPTS as u64 {
            std::fs::write(name(s), "PLANTED").unwrap();
        }
        assert!(create_staging(&dir, "out.pdf", StagingName::SuffixThenTag, 1..).is_err());
        assert!(!name(STAGING_ATTEMPTS as u64 + 1).exists(), "a 17th name was tried");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_names_keep_the_extension_and_preserve_existing_files() {
        let dir = test_dir("output-names");
        for name in ["report.pdf", "report (2).pdf", "archive.tar.gz", "LICENSE"] {
            std::fs::write(dir.join(name), b"KEEP").unwrap();
        }
        for (name, next) in [("report.pdf", "report (3).pdf"), ("archive.tar.gz", "archive.tar (2).gz"), ("LICENSE", "LICENSE (2)")] {
            assert_eq!(unused_output_path(&dir.join(name)).unwrap(), dir.join(next));
            assert_eq!(std::fs::read(dir.join(name)).unwrap(), b"KEEP");
        }
        assert_eq!(unused_output_path(&dir.join("new.pdf")).unwrap(), dir.join("new.pdf"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exhausted_output_names_are_refused() {
        let dir = test_dir("output-names-exhausted");
        std::fs::write(dir.join("part.pdf"), b"KEEP").unwrap();
        for n in 2..10_000u32 {
            std::fs::File::create(dir.join(format!("part ({n}).pdf"))).unwrap();
        }
        assert_eq!(unused_output_path(&dir.join("part.pdf")).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(dir.join("part.pdf")).unwrap(), b"KEEP");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[cfg(unix)]
    fn unused_output_names_preserve_dangling_symlinks() {
        let dir = test_dir("output-names-symlink");
        std::os::unix::fs::symlink(dir.join("missing.pdf"), dir.join("part.pdf")).unwrap();
        assert_eq!(unused_output_path(&dir.join("part.pdf")).unwrap(), dir.join("part (2).pdf"));
        assert!(dir.join("part.pdf").symlink_metadata().unwrap().file_type().is_symlink());
        let _ = std::fs::remove_dir_all(dir);
    }
}
