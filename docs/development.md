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

## Standard desktop window controls

Windows and X11 keep their system window frame. On Wayland, PdfCraft requests a decorated window
and enables winit's Adwaita frame when the compositor leaves decorations to the application. This
provides the standard minimize, maximize/restore and close controls. The document toolbar remains
inside the frame. Linux builds use the existing patched winit 0.30.13 and its `wayland-csd-adwaita`
feature; the dependency's Cantarell fallback font is attributed in `ATTRIBUTION.toml`.

The Linux x86_64 release job checks the production dependency features and captures the actual
window under a private nested Weston/Xvfb desktop. The diagnostic artifact is
`diagnostic-wayland-frame-x86_64`; it is separate from release packages. Locally on Linux:

```sh
sudo apt-get install weston xvfb scrot libegl1-mesa libgl1-mesa-dri fonts-cantarell
bash packaging/linux/check-window-frame.sh target/release/pdfcraft target/window-frame
```

The check uses an isolated home and settings directory, opens no user documents, and forces
Wayland so it cannot silently fall back to X11. Screenshots are private diagnostics and are not
committed to the repository.

## Opt-in UI control transport limits

`--control <file>` still binds only to loopback and requires the fresh token in the owner-only
endpoint file. The transport uses four connection workers and queues at most four more sockets;
additional connections are closed. Authentication must complete within five seconds of acceptance,
including queue time. Each request line is limited to 1 MiB including its newline and must arrive
within 30 seconds; partial bytes do not renew that deadline. Reply bodies are limited to 32 MiB and have
a five-second absolute write deadline. Clients should reconnect after a closed or timed-out socket.
No socket or worker is created unless control was explicitly requested.

A request still queued when its reply deadline expires is cancelled atomically and cannot run on
a later frame. If UI dispatch already started, a missing response is reported as an unknown outcome;
clients must inspect the document before retrying an edit. The CLI does not retry a failed request
and rejects mismatched response IDs.

## Contained document scripting

Before building or testing any frontend, bootstrap the portable scripting kernel:

```sh
rustup target add wasm32-wasip1
cargo xtask js-sandbox
cargo test -p pdfcraft-js
```

This explicit command builds `apps/pdfcraft-js-worker` from the same shared form protocol and
existing Boa/XFA/FormCalc implementation as the host crate. It writes an ignored generated Wasm
module to `target/js-sandbox/pdfcraft-js-worker.wasm`. No generated kernel is committed. A custom
build input can be selected with `PDFCRAFT_JS_SANDBOX` at host compile time. The normal build
script only embeds that file; it never invokes Cargo. A missing kernel fails the build with
bootstrap instructions, and there is no unsandboxed fallback. `cargo xtask ci` and the native
release/test workflows perform this bootstrap explicitly.

Desktop and browser frontends execute that kernel through the same Wasmi interpreter. Each call
gets a fresh store with one linear memory capped at 128 MiB (including a 16 MiB guest stack),
one table capped at 16,384 entries, a bounded interpreter stack and a 1.5-billion-unit aggregate work budget.
The budget covers parsing, Boa built-ins and regex work, FormCalc, allocation and host byte
copies. Encoded requests and replies are capped at 4 MiB each; effects and message counts are
checked before they reach the caller. A memory/work/output trap returns an error and no effects;
the whole store is dropped, without leaving a background thread. Wasmi resumes in slices of at most 250,000 instruction units (or one bounded bulk-memory
operation). A monotonic deadline is checked between slices and before host calls: two seconds
by default, shortened by existing `*_within` durations. OS scheduling and the current slice can
add latency; there is no detached thread or exact real-time scheduling guarantee.

The minimal WASIp1 adapter provides only bounded stdin/stdout/stderr, empty arguments and
environment, a clock frozen at the start of each run and deterministic pseudorandom values. It has no
filesystem, directory, network or process-launch capability. Its pseudorandom stream is not a
cryptographic entropy source. Forms that need a progressing timer, very large snapshots or work beyond
these budgets receive an error rather than bypassing the boundary. Physical host memory and
wall-clock performance still depend on the interpreter/platform and need release-profile
measurements; the limits are per script store, not a whole-application memory quota.
