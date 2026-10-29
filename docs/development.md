# Developing PdfCraft

Working instructions for agents and contributors are in `AGENTS.md`. This page
collects what the desktop app reads from its environment and where it writes its diagnostics.

Platform setup: [Ubuntu 24.04 / 26.04](ubuntu.md) and [Windows 11 graphics drivers](windows-gpu.md).

## Logs

The desktop app writes its `log` records to standard error and to `logs/pdfcraft.log` in its
settings folder, next to `app.ron`: Linux and FreeBSD `~/.local/share/pdfcraft/logs/` (or
`$XDG_DATA_HOME/pdfcraft/logs/`), macOS `~/Library/Application Support/PdfCraft/logs/`, Windows
`%APPDATA%\PdfCraft\data\logs\`. In portable mode (a `portable.txt` file next to the executable, as
the Windows portable zip ships) the settings folder is `PdfCraftData\` beside the executable, and
crash recovery and new digital IDs go there too. A start from a desktop menu, Finder or the Start
menu has no terminal, so this file is what to attach to a bug report: a failed autosave, a page that
would not render, a render worker that could not start and the report of an internal error all land
there.
Each launch moves the previous log to `pdfcraft.1.log` (and that one to `pdfcraft.2.log`), so the
log of a run that crashed survives the next start. The file stops growing at 16 MiB. `--version`
writes no file.

By default PdfCraft's own crates (`pdfcraft*`) log at `info` and everything else at `warn`.
`RUST_LOG` replaces that with env_logger-style directives, for example `RUST_LOG=debug`,
`RUST_LOG=warn,pdfcraft_render=trace` or `RUST_LOG=info,wgpu_core=warn`; a directive ending in `*`
covers every target starting with it (`pdfcraft*=debug`). The logger is
`apps/pdfcraft/src/logging.rs`. It never records the control-channel token or document passwords.

## GPU surface limits

The desktop renderer enables the selected adapter's supported 2D texture extent, while keeping
egui-wgpu's other device requirements. Restored windows on mixed-DPI monitors can exceed its
default 8192-pixel extent even when the GPU supports a larger surface (#577). The enabled extent
never exceeds the adapter's capability. A window larger than even that (restored at another
monitor's scale, or stretched across monitors) gets a surface fitted within the limit and is drawn
at a correspondingly lower scale instead of panicking in `Surface::configure` (vendored egui-wgpu,
`surface_fit`; see `vendor/README.md`). On DX12, PdfCraft's Windows default, the surface is
stretched over the window: softer, but complete and lined up with the pointer. wgpu's GL backend
copies it unscaled into a corner of the window instead.

## Renderer fallback

PdfCraft draws with wgpu, and starts again with eframe's own OpenGL renderer (glow) when wgpu fails
before the app is created: no adapter, no device, or a window surface the device can't configure.
The last of these was a panic, not an error, because wgpu's default error handler panics and
eframe configures the surface before any app code runs; a 2015 Intel GPU whose driver timed out in
`Surface::configure` on wgpu's GL backend closed PdfCraft at launch (#519). Vendored egui-wgpu
configures a new window's surface inside `winit::catch_errors` and returns a validation,
out-of-memory or internal error as `WgpuError::ConfigureSurface` (see `vendor/README.md`). Still a
panic, as upstream: an error in a later reconfigure (resizing, a lost surface); a device lost
during that first configure (wgpu reports it only to a callback, so the first frame panics, after
the app has started); and a window shown at zero size, which is first configured on resize. If
OpenGL can't start either, PdfCraft exits with both errors in the log. The fallback is logged at
`error`, with the wgpu error. `PDFCRAFT_RENDERER` skips it or chooses OpenGL from the start.

## Environment variables

| Variable | Effect |
|---|---|
| `RUST_LOG` | Log levels for standard error and the log file (see [Logs](#logs)) |
| `RUST_BACKTRACE` | `1` adds a backtrace to the report of an internal error |
| `XDG_DATA_HOME` | Linux/FreeBSD: base of the settings folder (`pdfcraft/`), the log folder and crash recovery |
| `WGPU_POWER_PREF` | GPU choice; by default PdfCraft draws on the GPU that drives the primary display (Windows) or the built-in panel (Linux), else the low-power (integrated) GPU. The log says which one was chosen |
| `WGPU_BACKEND` | Graphics backend; by default Windows uses Direct3D 12, falling back to OpenGL |
| `PDFCRAFT_RENDERER` | `gl` starts with OpenGL (glow) and never loads wgpu; `wgpu` reports a wgpu failure instead of retrying with OpenGL; unset, wgpu is retried with OpenGL when it can't start (see [Renderer fallback](#renderer-fallback)) |
| `CRAFT_FONTS_DIR` | Build time: a [craft-fonts](https://github.com/storytold/craft-fonts) checkout to embed (Japanese fonts) |
| `PDFCRAFT_SYSTEM_FONTS` | `0` stops the desktop app from using an installed font for characters its embedded fonts lack (`cargo xtask screenshots` sets it) |


## Network and dependency review (2026-10-29)

The source review covered PdfCraft, PhotoCraft, VectorCraft and the shared Craftworks launcher:
first-party runtime network entry points, build scripts, packaging/workflow commands and all four
Cargo lockfiles. No telemetry SDK integration or automatic document upload was identified in
these reviewed paths. This is a scoped source review and advisory scan, not a guarantee that
all dependency source, binaries or future changes are safe.

The editors now pin the published hardened `craft-update` 0.3.0 revision
`a057bf66b8caf197434cce4704b16d8af25c3953`. The old 0.1.0 pin lacked the shared library's
private per-download staging, prepared-file ownership and install-time checksum revalidation;
those protections were already implemented and tested in Craftworks but were not reaching the
editors. A PdfCraft integration regression downloads a signed synthetic update, changes its
staged bytes without changing its size, and checks that installation refuses it and preserves
the old app. Both the consumed pinned source and the current shared source were reviewed.

The editors default to **no automatic release checks**. Missing update settings, including a
partial object without `enabled`, keep background checks off. Existing saved `enabled: true`
values are preserved; older versions also saved that value by default, so an existing user who
wants no background checks must turn it off in General settings. Manual Help checks still work.
The updater sends a generic app/version User-Agent to `api.github.com`, then fetches requested
release assets through GitHub. It does not attach document contents, identity preferences,
recent-file lists or a machine identifier. Checks and downloads remain separate actions;
signed checksums and hashes still gate installation.

Other reviewed network surfaces:

- PdfCraft's Submit Form sends selected form data or the PDF only from an explicit send action.
  Timestamping and fetching OCSP/CRL evidence are explicit signing operations; they disclose
  timestamp requests or certificate identifiers to their configured endpoints. Internet-access
  policy, timeouts, size caps and no-redirect rules apply. Opening/validating a PDF does not
  start these fetches. Document JavaScript has no network or filesystem host API.
- The editors' browser builds load application/font resources from their hosting origin;
  separately served craft-fonts faces use pinned Subresource Integrity. Hosting access logs
  can therefore see normal resource requests. Desktop control servers are opt-in loopback
  surfaces; they are not outbound telemetry. The reviewed plug-in hosts expose no networking.
- Craftworks deliberately checks its own and the three editors' releases on launch. Changing
  `AutoCheck` does not disable this separate launcher catalogue behavior. Its GET requests
  disclose the requester IP and generic app/version to GitHub; no document or installed-app
  inventory is posted by the reviewed release-check code.
- Developer-only corpus/font downloads and release publishing are explicit build/maintenance
  workflows. Source, attribution and repository links do not themselves trigger requests.
- Adobe namespace addresses in XMP, XFDF and XFA are compatibility identifiers in the
  reviewed reader/writer code. Those string literals do not fetch their named addresses.

`cargo audit --file Cargo.lock --json` used RustSec database commit
`ee1a49a2769a24a596afc1c20cb33748d2769586` (2026-10-29). Updates remove the applicable lockfile
findings for crypto-bigint/ctutils, Tokio, rustls-webpki, ruzstd and Craftworks' zip dependency.
Craftworks keeps its custom bounded extractor, link rejection and fixed output permissions
when upgrading zip; it does not use the generic extract helpers named in the advisories.

PdfCraft retains the already documented [RSA timing advisory](https://rustsec.org/advisories/RUSTSEC-2023-0071.html),
which has no patched version: native private-key operations use aws-lc-rs; the browser's local,
user-requested RSA signer uses blinding. This is not a claim of a fixed RSA implementation.
Unmaintained transitive dependencies remain: zvariant/zvariant_derive/zbus_names in the editors,
rustybuzz/ttf-parser in PhotoCraft and VectorCraft, paste in PhotoCraft, and ttf-parser in Craftworks.
PdfCraft’s `cargo deny check` also passes its advisory exceptions, bans, licences and source policy; it warns that the Windows-only clipboard-win 5.4.1 lockfile version is yanked. Advisory scans do not detect every backdoor or prove author intent.

A remaining hardening gap is VectorCraft's opt-in desktop control server:
`apps/vectorcraft/src/control_server.rs` accepts loopback JSON requests without a bearer token.
Its JSON/HTTP rejection and line/connection caps do not authenticate local clients. The MCP
bridge also has no token handshake. This is an inbound local-control weakness, not evidence of
an outbound callback; do not expose or use that port with untrusted local clients. PdfCraft and
PhotoCraft have token-authenticated control channels. Adding compatible token support to
VectorCraft's host, MCP bridge and clients remains follow-up work.


## Optional workstation CI runners

The 2026-10-29 local setup registers one repository-scoped GitHub Actions runner for each of
the four family repositories. Workflows can request `[self-hosted, Linux, X64, craft-local]`;
existing hosted workflows are unchanged. The boot-enabled user services are
`craft-actions-{pdfcraft,photocraft,vectorcraft,craftworks}.service`. The local operator notes
are in `~/.local/share/craft-actions/README.txt`.

All four services share `craft-ci.slice`: at most one logical CPU combined, lowest CPU/IO
weights, nice 19, idle disk scheduling, a 4 GiB memory high-water mark and 6 GiB hard cap.
Cargo builds, Rayon and Rust test concurrency default to one. Performance measurements on
this runner are unsuitable for raising parity or performance claims while simulations run.
Each runner has a separate filesystem/process sandbox without the user's home, live project
folders or desktop credential sockets; Rust tools and the pre-job admission hook are read-only.
Networking is shared with the host. The hook admits only owner-triggered push, schedule or
manual jobs for the four repositories, and refuses PR jobs and other actors. The services use
GitHub's official `runsvc.sh` entry point and reconnect after network failures. Registration,
service restart, sandbox boundaries and admission cases were checked; no physical reboot or
GitHub workflow job was part of that setup verification.
