# pdfcraft-platform

Layer L7 holds operating-system services that both the desktop app (`pdfcraft-ui-egui`) and the automation tools (`pdfcraft-automation`) need. Those two crates sit on the same layer and can't depend on each other, so a helper they share lives here instead of being copied into each (`xtask/src/layers.rs` allows both edges). It has no UI dependencies. File reads use the standard library, plus Unix constants for its safe nonblocking open API.

`staging` creates the temporary file an atomic save writes before renaming it over the target. The file is opened with `create_new`, so anything already at the name (a file, a hard link, a symbolic link even when dangling, a folder) is skipped and never written through. On Windows a folder at the name fails with "access denied" rather than "already exists", and is skipped only when something is actually there. Names carry an unpredictable 64-bit suffix from `staging_suffixes` (the standard library's keyed hash, currently SipHash-1-3, keyed from the operating system's random source), which stops a name being planted in advance but is not a CSPRNG; fixed suffixes are only for tests. At most 16 names are tried, and then the save is refused. The target's name is cut to 128 bytes on a character boundary so the staging name fits every system's limit. Each caller keeps its own name form (`.{name}.{suffix}.pdfcraft-tmp` for automation, `.{name}.pdfcraft-{suffix}.tmp` for the desktop save). Writing, closing the file before the rename, renaming and removing the staging file on failure stay with the caller.

Synthetic tests pin both name forms, the 128-byte cut, the attempt limit and refusal when no name is left. The planted-file, link, folder and failed-rename tests stay with each caller, where they exercise the whole save.

`unused_output_path` selects available names for multi-file exports in both the desktop app and automation. It preserves the extension while adding numeric suffixes, treats dangling symlinks as occupied, and refuses after 9,999 names instead of selecting an existing file.

`read::regular_file` accepts only regular file handles, caps each read and the total byte count,
checks cancellation/deadlines between 64 KiB chunks, and refuses files that grow past the limit.
Unix opens are nonblocking so a FIFO substituted between the path and handle checks cannot hang
an identity import. Synthetic tests cover regular files, directories, growth, cancellation and
Unix FIFOs; no user files are read.
