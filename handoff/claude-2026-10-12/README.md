# Claude recovery transfer

This branch transfers paused work. It changes only this handoff directory; it does not
integrate any draft implementation or certify untested features. The application branch
`feature/full-parity-2026-10-12` remains at
`994698dde20a97cc41eccd17552f1797d7dd2298`.

The original chat linked private workspace paths rather than delivering a downloadable
attachment. The verified original bundle is now accessible through GitHub.

## Retrieve without switching your current branch

From your PdfCraft clone:

```sh
git fetch origin refs/heads/handoff/claude-2026-10-12
git show FETCH_HEAD:handoff/claude-2026-10-12/claude-handoff-2026-10-12.tar.gz > claude-handoff-2026-10-12.tar.gz
```

The archive is 442345 bytes. Its SHA256 is:

```text
243ac04ba3e242d6cdf7af600c5d2ed46db6f93719c15ad062481f27ece33d94
```

Extract it outside your source checkout, then read `CLAUDE_HANDOFF.md`,
`WORKSPACE-STATE.json`, `RECOVERY-VERIFICATION.json` and every relevant `lane-handoffs/`
file. Run `sha256sum -c SHA256SUMS` from the extracted bundle directory.
The handoff and workspace state are also readable directly beside this README.

## Preserve the current continuation

Keeping new work on `feature/full-parity-2026-10-12-resume` is appropriate. Restore each
snapshot against its recorded base in an isolated worktree, verify source hashes, then
integrate deliberately with your continuation commits. Most draft lanes start at 6cab152;
main Preflight and text-next start at 994698d. Do not apply all snapshots sequentially as
though they were one compatible patch stack. The `pdfcraft` snapshot has the authoritative
Preflight integration; `pdfcraft-preflight` is the older isolated copy and is not another
change to apply over it.

Preflight has 25 focused passing tests but still needs the full required gates. Reader,
OCR, signature, underline and rendering drafts are uncompiled and untested. The documented
OCR permission blocker is in the uncommitted OCR draft and may not be discoverable from
the committed branch alone. Do not assume any draft is already finished.

## Included and excluded material

The exact bundle contains seven lane handoffs, ten dirty-worktree source snapshots,
recorded bases, binary-safe tracked patches, new-file archives, hashes and selected evidence
records. Verification covered all 64 checksum entries and 272 source hashes. Main Preflight
is 17 modified tracked files plus 9 new files. Archive paths and source/evidence were also
checked for accidental live credentials or control tokens; none were found.

No PDF `plan/` directory exists in the retained worktrees or bundle. The committed parity
inventory and the explicit lane handoffs are available; do not invent the missing plan.
An ignored PhotoCraft continuation note remains local and is not part of this exact bundle.
Build caches, models, installers and full runtime screenshot/log evidence are excluded.
Absolute `/workspace` paths in the handoff describe the old environment and must be mapped
to your new checkout locations. Selected validation manifests are historical records, not
proof that their referenced external artifacts exist in your fresh session.

No development work was resumed to produce this transfer. Do not merge this recovery branch
as the application implementation; consume the snapshots and validate the resulting code.
