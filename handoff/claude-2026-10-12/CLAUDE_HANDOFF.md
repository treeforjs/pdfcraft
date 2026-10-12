# Artcraft continuation handoff for Claude

The user asked to pause this session and have Claude resume. All agents have been told to
stop. The active priority is **PdfCraft feature parity first**; PhotoCraft and VectorCraft
are preserved but paused. This is substantial unfinished work, not a completed parity claim.

This handoff was written on 2026-10-12. Read the lane handoffs before editing. The current
workspace contains valuable uncommitted work. Do not reset, clean, overwrite, or blindly
apply recovery patches to worktrees that already contain those changes.

## User intent and authorization

- Audit, fix and push `treeforjs/photocraft`, `treeforjs/vectorcraft`, `treeforjs/pdfcraft`,
  and the other forked Artcraft apps. The user authorized parallel agents and continuing
  implementation; the latest instruction prioritizes PdfCraft until parity.
- Ubuntu 24.04/26.04 LTS, x86_64 is the first installation target. Prior work also documented
  Windows drivers for RTX PRO 4000/2000 Blackwell, GeForce RTX 30/40/50 and Intel integrated
  graphics. Physical GPU and Windows runtime validation has not happened here.
- Remove Discord links; prepare runnable individual/combined installers. Earlier audit and
  combined-installer checkpoints exist. Do not bundle OS GPU drivers into these app packages.
- Resume under the user's fresh Claude request without asking again for routine fixes,
  tests, branch pushes or already-authorized implementation. Do not silently merge to the
  default branch or claim unfinished work is production-ready.

## Latest delivered PdfCraft checkpoint

Repository: <https://github.com/treeforjs/pdfcraft>

Pushed branch: `feature/full-parity-2026-10-12`

Pushed commit: `994698dde20a97cc41eccd17552f1797d7dd2298`

It implements source-preserving selected-range/mixed-run text formatting and reflow,
Unicode-scalar ranges, original glyph/TJ/spacing handling, faithful explicit PDF draft
preview, selected bold/italic controls, atomic Apply/Cancel, early input limits and
conservative refusal of unsupported graphics states. Uniform whole-paragraph rewriting
still has a separate legacy path. Rotated/skewed/reflected mixed-run editing, underlines,
text stroking and complex ExtGState remain gaps. The live typing overlay can approximate
fonts; the explicit rendered PDF preview is the faithful preview.

Validation for this exact pushed commit:

- 1,809 native tests passed, five ignored, across 85 library/binary/integration/example/doc
  batches; strict all-target Clippy, formatting, 30 WASM crates, asset/layer/parity/deny gates.
- Independent original Type3/mixed-font CLI fixture and external Poppler render/text oracle.
- Exact packaged binaries installed and checked as an ordinary user on Ubuntu 24.04/26.04,
  X11/Wayland, baseline/retained/new-feature cases: all 12 passed without harness correction.
- All 52 retained PNGs visually reviewed through their 22 distinct hash-identical contents.
  Proposed/accepted PNG equality and unchanged bytes/history during preview are asserted.

Package:
`/workspace/ubuntu-support/full-parity-2026-10-12/pdfcraft/pdfcraft-0.5.0+parity2-linux-x86_64.deb`

Package SHA256:
`3c7b38f5ec87648297c6e15f6f6e2ae11457a2fd60853ed533a716c0e8c69f10`

The version is `0.5.0+parity2-1`. A matching tar.gz is alongside it. This is a development
build, not a full-parity release. Optional craft-fonts CJK input was absent. Tests used
software Mesa in containers; real GNOME desktops, printing, native Windows and physical
NVIDIA/Intel hardware remain unverified.

Evidence roots:

- `/workspace/ubuntu-support/full-parity-2026-10-12/pdfcraft-source-validation.json`
- `/workspace/ubuntu-support/pdfcraft-checkpoint-2026-10-12/PDFCRAFT-CHECKPOINT.json`
- The latter directory also contains `pdfcraft/BUILD-RECEIPT.json`, immutable 12-case
  aggregate, `HARNESS-INPUTS.json`, screenshot index, visual review and the PDF-only gate.
- `python3 validate_pdf_checkpoint.py PDFCRAFT-CHECKPOINT.json` passed. It authorizes only
  this exact PdfCraft package, not the pending three-app combined milestone.

Do not rerun this exact checkpoint merely because work resumed. Future changed packages
need new receipts/attempts. Preserve frozen harnesses and evidence rather than overwriting.

## Current worktrees and validation status

Lane handoffs live at `/workspace/ubuntu-support/full-parity-2026-10-12/` and are copied into
the portable bundle under `lane-handoffs/`. Read them for precise commands and recovery.

| Lane | Authoritative source | State at pause | Handoff |
|---|---|---|---|
| Preflight | `/workspace/pdfcraft` | 26 uncommitted files atop pushed994698d; 25 focused tests pass, preflight strict Clippy/fmt pass; full centralized gates pending | `handoff-preflight.md` |
| Reader selection | `/workspace/pdfcraft-selection` | 29 files on base6cab152; scoped source review clear; 23 new tests unexecuted; uncompiled/unintegrated | `handoff-reader-selection.md` |
| OCR review | `/workspace/pdfcraft-ocr` | 15 files on base6cab152; uncompiled; confirmed permission blocker | `handoff-ocr.md` |
| Signatures | `/workspace/pdfcraft-signatures` | 35 files on base6cab152; uncompiled; no OpenSSL/network/test execution | `handoff-signatures.md` |
| Underline editing | `/workspace/pdfcraft-text-next` | 13 files on base994698d; uncompiled; output expansion budget unresolved | `handoff-text-editing.md` |
| Native vector renderer | `/workspace/pdfcraft-rendering` | 12 files on base6cab152; uncompiled; review and fidelity/performance evidence incomplete | `handoff-rendering.md` |
| Independent validation | immutable evidence directories | Delivered checkpoint validated; preflight/reader scoped review clear, other review incomplete | `handoff-runtime-review.md` |

Full old PDF base: `6cab152bc24f80629627ad928e327027088727c4`.

Preflight already integrated into main is authoritative. Its old isolated
`/workspace/pdfcraft-preflight` is a recovery/reference copy, not another patch to merge.
No uncommitted feature source has been pushed. GitHub alone does not contain these drafts.

## Suggested first actions

1. Read root/repository `AGENTS.md` and `CLAUDE.md`, this handoff, and lane handoffs. Inspect
   current status against `WORKSPACE-STATE.json`. Preserve all uncommitted work.
2. Finish review/required centralized gates for main's preflight change, or deliberately
   integrate the reader lane first and validate the combined source. Main owns preflight;
   its patch must not be applied a second time. Reader was next in the build queue.
3. Integrate lanes sequentially using contextual/three-way patches. Shared engine/UI/tool
   registrations overlap and most branches predate994698d. Do not replace shared files
   wholesale or rebuild all worktrees simultaneously.
4. Fix the known OCR authority blocker before integration: prepare/review currently can
   render/infer and expose recognized words before copy/modification permission checks.
   Enforce authority before backend invocation and on draft access, preserve owner override,
   and add encrypted-document rejection tests (details in OCR handoff).
5. Bound authored underline output-operator expansion before cloning/emitting; complete
   signature and renderer security/resource reviews. Unexecuted tests are not evidence.
6. Run focused tests, actual CLI/UI workflows and independent oracles, then required
   workspace/native/WASM/asset/parity gates. Commit/push only validated source; package
   exact clean commits with new build receipts and installed acceptance.
7. Continue through PdfCraft's inventory. These six lanes are only a beginning. The
   inventory `docs/parity-inventory-2026-10-12.md` at994698d records 830 rows: 420 labeled
   shipped, 73 partial, 314 planned, 23 not applicable. The 387 unfinished applicable rows
   are starting labels, not equal-sized tasks or independent proof of quality. Do not
   change rows to shipped just because a command/test exists.

## Important unfinished feature details

- Preflight: structural General/Print/PDF-A2b/A3b profiles and exact-value/revision-bound
  atomic selected fixups. Full ISO PDF/A/X/UA conformance is absent. Legacy `compliant`
  now conservatively false with explicit incomplete-validation fields. A deep/wide path
  memory amplification was fixed with shared ancestry and a retained-path budget.
- Reader: cross-page anchors, document-wide selection, paragraph selection, complete
  deferred copy, bounded extraction and copy permission. Drag ownership and late-frame
  copy cancellation were reviewed and fixed; actual tests still need execution.
- OCR: review/correct/apply/cancel, source binding, cancellation without partial apply,
  input geometry bounds and balanced invisible-text state. English model baseline works
  upright but 90/180-degree and deskew/order cases expose real gaps. Multilingual labels
  cannot substitute for multilingual models and Unicode-safe PDF output.
- Signatures: opt-in bounded TSA transport, explicit trust/time provenance, standalone and
  signing timestamps, local DER evidence validation/DSS. OCSP ENUMERATED parser correction
  is included. Delegated OCSP certificate nesting and TSA-chain evidence coverage need
  review; no blanket B-LT/B-LTA or long-term-validity claim.
- Text-next: author-owned marked-content association for glyph underline paths, avoiding
  geometric deletion of unrelated artwork. Uniform rewrite unification and transformed
  local-frame editing are still planned, not implemented.
- Rendering: original numeric vector lexer/compiler/device, automatic whole-page fallback,
  bounded cache. The final strict-lexer/Flate-only framing corrections have not been
  independently re-reviewed or compiled. Text/fonts/images/forms/transparency still use
  hayro. Independent sheared/anisotropic stroke comparison is still required.

## Environment and build discipline

The workspace is Debian13, approximately four CPUs and17GiB RAM. At pause, overlay free
space is about2GiB and `/tmp` about3GiB; check `df -h` before builds. No Cargo/rustc or
acceptance run should remain active. Docker containers may remain idle for later testing.

Use one consolidated PDF Cargo target and one build job:

```sh
cd /workspace/pdfcraft
. /workspace/native-deps/env.sh
export CARGO_TARGET_DIR=/workspace/pdfcraft/target/agent-parity
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
unset CRAFT_FONTS_DIR
```

Do not repurpose HOME/CODEX_HOME or set PKG_CONFIG_SYSROOT_DIR. The repository requires
strict Clippy, native/WASM tests, formatting, assets/layers/parity/deny gates before green
commits. See its current `cargo xtask ci` for exact commands. Focused preflight egui tests
needed no display/GPU override. Real packaged UI acceptance uses ordinary UID1001 in
`craft-ubuntu24`/`craft-ubuntu26` with Xvfb/Weston through
`/workspace/ubuntu-support/docker-local`.

OCR models are verified and packaged at `/usr/share/pdfcraft/models`; a host copy is in
ignored `/workspace/pdfcraft-ocr/assets/models`. Set `PDFCRAFT_MODELS` there for actual-model
tests. Do not skip models silently or redownload unnecessarily. Tesseract eng/osd and
OpenSSL are available as independent tools. Copyleft tools are external oracles only.

Generated test ELFs were selectively reclaimed after recording hashes/logs. Some interrupted
Vector tests had compiled but never executed; their removal is not a passing test result.
WASM targets are symlinked to `/tmp/artcraft-full-parity-wasm-cache/{app}`; Photo release
target is symlinked under `/tmp/artcraft-full-parity-release/photocraft`. Never blindly clean
all targets or frozen packages. Prior cleanup receipts live in the full-parity evidence root.

The cloud-environment runtime skill was read for network/proxy rules; read it again in a
fresh environment if needed. Do not print credentials. This workspace's current approvals
are disabled; authorized Git pushes already worked.

## Other preserved work

PhotoCraft: `/workspace/photocraft`, branch `feature/full-parity-2026-10-12`, base
`b1ec586d58e6937d52dd90a1fdc654bebe2972ff`; 51 uncommitted files. Smart Hue/Colorize,
mask painting/history, stack UI, blend fixes. Full final gates incomplete. After an oracle
regression, a narrow correction restored the existing5/86 floor; do not lower that floor.
See `photocraft-paused-for-pdf.json` and the Photo section of `handoff-ocr.md`.

VectorCraft: `/workspace/vectorcraft`, same full-parity branch, base
`d06459cbe77afe25479ad66276785b0b340e3500`; 52 uncommitted files. Raster effects/filter
gallery and ruler-origin work; full CI was stopped before compiled integrations ran.
Preserved state is in `vector-paused-checkpoint/`. Separate detached
`/workspace/vectorcraft-tab-leaders` contains14 uncompiled changed/new files. A known
unfixed tabs-ruler tick loop on huge finite frame widths can perform billions of iterations;
fix it before shipping that work. See `vector-tab-leaders-partial-source-review.json`.

DesignCraft, EffectCraft, FilmCraft and LightCraft audit branches are clean. Exact branches
and commits, plus every dirty worktree, are listed in `WORKSPACE-STATE.json`.

Earlier frozen combined installer:
`/workspace/ubuntu-support/parity-wave1/installers/Artcraft-Ubuntu-Parity-1-x86_64.run`

SHA256: `ab78c9ac20064e75837fe0b3d932172c7796828fb0e4149496de8b2e6f7d4597`.
It has earlier24 installed cases and four combined-installer checks; it does not contain
the later994698d PDF checkpoint. The newer three-app combined installer is not approved by
its gate because Photo/Vector work remains unfinished. Keep this distinction explicit.

## Portable recovery bundle

`/workspace/claude-handoff-2026-10-12.tar.gz` contains this handoff, all lane handoffs,
workspace state, source snapshots and selected evidence. It excludes Cargo caches,
credentials, large models and installers. The existing workspace is authoritative.

Each dirty repository snapshot has its exact base/branch, binary-safe `tracked.patch`,
`new-files.tar.gz`, and source hashes. On a fresh clone only, check out that exact base,
apply the tracked patch and extract its new files into the repository; then verify hashes.
Do not apply snapshots to current dirty worktrees. The preflight snapshot under `pdfcraft`
is authoritative; `pdfcraft-preflight` is the older isolated copy. Later integration must
preserve changes from every lane and resolve shared registration conflicts carefully.

SHA256SUMS inside the bundle verifies its files. Full runtime screenshots/logs and build
artifacts remain at the absolute paths above; selected manifests in the bundle are records,
not a replacement for missing retained evidence. A fresh environment must install its own
build dependencies and recreate runtime acceptance for new artifacts.
