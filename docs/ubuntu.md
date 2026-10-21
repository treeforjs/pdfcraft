# PdfCraft on Ubuntu

The supported Ubuntu targets are **24.04 LTS and 26.04 LTS on x86_64 (amd64)**.
Both Wayland and X11 desktop sessions are supported. The CLI also works without a display.

## Install a package

Download the Linux x86_64 `.deb` from the release you want to use, then install it with APT
so its dependencies are installed too:

```sh
cd ~/Downloads
sudo apt install ./pdfcraft-*-linux-x86_64.deb
pdfcraft --version
pdfcraft-cli --version
```

Choose **PdfCraft** in the application menu, use **Open With** on a PDF, or run
`pdfcraft "some document.pdf"`. PdfCraft does not change your default PDF application.
To choose it yourself:

```sh
xdg-mime default io.github.treeforjs.pdfcraft.desktop application/pdf
```

The package includes the CLI and OCR models. CUPS printing and the desktop file-dialog portal
are recommended dependencies; keep APT's default recommendation handling enabled. An Ubuntu
desktop already provides a portal backend; the package also recommends the GTK fallback.

## Build the current checkout

Install a current stable Rust toolchain with [rustup](https://rustup.rs/), then run these commands
from the repository root. Build as your regular user; only system dependencies need sudo.

```sh
sudo packaging/linux/install-ubuntu-deps.sh build
sudo packaging/linux/install-ubuntu-deps.sh runtime
rustup target add wasm32-wasip1
cargo xtask js-sandbox
cargo build --locked --release -p pdfcraft -p pdfcraft-cli
./target/release/pdfcraft "some document.pdf"
./target/release/pdfcraft-cli info "some document.pdf"
```

The first build can take several minutes. On a machine with limited RAM, add `-j 2` to the Cargo
command. A binary built on Ubuntu 26.04 can require a newer glibc than Ubuntu 24.04 provides;
build on **24.04** when sharing one binary with both systems. Official release packaging uses
an older Ubuntu baseline. The `.deb` records the actual glibc requirement of the built binaries,
so APT refuses installation on a system whose libc is too old. Optional Japanese fonts require the `CRAFT_FONTS_DIR` build input
described in the [README](../README.md#get-started).

For a locally installable `.deb`, install [nfpm](https://nfpm.goreleaser.com/install/) and run:

```sh
packaging/linux/package.sh --skip-build --formats deb
sudo apt install --reinstall ./dist/release/pdfcraft-*-linux-x86_64.deb
```

Packaging fetches SHA-256-verified OCR models from their attributed upstream sources and includes
the desktop entry, icons, licences and CLI. `--reinstall` replaces an older build with the same
version number. See [Releasing](releasing.md#linux) for other formats.

## NVIDIA and Intel graphics

The priority hardware is **RTX PRO 4000 Blackwell and RTX PRO 2000 Blackwell**, followed by
GeForce RTX 30 / 40 / 50 series and Intel integrated graphics. PdfCraft uses Vulkan or OpenGL
for its desktop renderer. The Ubuntu checks below use Mesa software rendering; these physical
GPU models have not been tested in this environment.

For NVIDIA, first apply Ubuntu's normal software and kernel updates and reboot. Follow
[Ubuntu's desktop driver procedure](https://ubuntu.com/desktop/docs/en/latest/how-to/graphics/install-nvidia-drivers/):

```sh
sudo apt update
sudo apt install ubuntu-drivers-common vulkan-tools
sudo ubuntu-drivers list
sudo ubuntu-drivers install
sudo reboot
```

Use the desktop driver recommended for your hardware. **Blackwell requires NVIDIA's open kernel
module**, including RTX PRO Blackwell and GeForce RTX 50 cards; see
[NVIDIA's requirement](https://download.nvidia.com/XFree86/Linux-x86_64/580.95.05/README/kernel_open.html).
Ubuntu publishes `nvidia-driver-595-open` for both
[24.04 updates](https://packages.ubuntu.com/noble-updates/amd64/nvidia-driver-595-open) and
[26.04](https://packages.ubuntu.com/resolute/nvidia-driver-595-open). This is a supported branch
example, not a minimum-version claim: use the current recommended `-open` branch for Blackwell.
[NVIDIA's supported-GPU list](https://download.nvidia.com/XFree86/Linux-x86_64/595.84/README/supportedchips.html)
explicitly includes RTX PRO 4000 Blackwell and RTX PRO 2000 Blackwell.
With Secure Boot enabled, complete any key-enrollment step requested during installation/reboot.

After reboot, verify the driver and Vulkan device:

```sh
nvidia-smi
modinfo -F license nvidia
vulkaninfo --summary
```

For the open NVIDIA module, `modinfo` reports `Dual MIT/GPL`.
For Intel integrated graphics, keep Ubuntu's kernel, firmware and
[Mesa drivers](https://docs.mesa3d.org/drivers/anv.html#runtime-dependencies) current:

```sh
sudo apt install libvulkan1 mesa-vulkan-drivers libegl1 libegl-mesa0 \
  libgl1 libgl1-mesa-dri linux-firmware vulkan-tools mesa-utils
vulkaninfo --summary
glxinfo -B
```

`glxinfo` checks an X11/XWayland session; `vulkaninfo` should identify the intended physical GPU.
`llvmpipe` identifies software rendering. New Intel hardware on Ubuntu 24.04 may require the
[supported HWE kernel](https://ubuntu.com/kernel/docs/reference/hwe-kernels/):
`sudo apt install --install-recommends linux-generic-hwe-24.04`, followed by a reboot.
Ubuntu 26.04 uses its current default kernel. See
[Intel's platform guidance](https://dgpu-docs.intel.com/installation-guides/installing-packages-from-the-intel-ppa.html)
for the kernel support of newer GPU generations.

PdfCraft normally selects the GPU driving the display. On a hybrid laptop it favors the built-in
panel's GPU. To request the high-performance adapter explicitly:

```sh
WGPU_POWER_PREF=high pdfcraft "some document.pdf"
```

This requests a suitable high-performance device; it does not identify a particular NVIDIA card
when several are installed. The [development guide](development.md#environment-variables)
describes renderer controls and where to find the selected adapter in the log.

## Desktop troubleshooting

If the window is blank or the graphics driver fails, try the OpenGL renderer:

```sh
PDFCRAFT_RENDERER=gl pdfcraft "some document.pdf"
```

On virtual machines without GPU acceleration, Mesa's software renderer is also available:

```sh
LIBGL_ALWAYS_SOFTWARE=1 PDFCRAFT_RENDERER=gl pdfcraft "some document.pdf"
```

Run from a logged-in desktop session so Wayland/X11 and the session D-Bus are available. If a file
dialog does not open, verify that `xdg-desktop-portal` and your desktop's portal backend are
installed, then log out and back in. X11 additionally needs `libxkbcommon-x11-0`; it is a package
dependency even though the executable loads it dynamically. Diagnostics are written to
`~/.local/share/pdfcraft/logs/pdfcraft.log` (or under `XDG_DATA_HOME` when set).

## Compatibility check

The [Ubuntu workflow](../.github/workflows/ubuntu.yml) builds a `.deb` on 24.04, installs it in
fresh 24.04 and 26.04 containers, and exercises both the automatic renderer and OpenGL under Xvfb
with Mesa as an ordinary user. Packages are built with a restrictive umask to check that their
resources remain readable after installation. It creates, saves, reopens, extracts text and renders a synthetic PDF, opens a file URI
with a space in its name, and captures the running desktop through its opt-in control channel.
This checks package dependencies and X11 startup; actual GNOME Wayland sessions, hardware GPU
drivers, printing and native file dialogs still need checks on those desktops. Local verification
also installed the `.deb` on both Ubuntu releases and exercised X11 and native Wayland under
headless Weston as a different, unprivileged user, including OCR-model/resource readability and
the same PDF and screenshot checks.

To run the same smoke test locally after installing or building:

```sh
sudo packaging/linux/install-ubuntu-deps.sh smoke
xvfb-run -a dbus-run-session -- packaging/linux/smoke-test.sh \
  "$PWD/target/release/pdfcraft" "$PWD/target/release/pdfcraft-cli"
```

It uses private temporary files and isolated settings, and removes them when it finishes.
Set `PDFCRAFT_SMOKE_OUTPUT` to retain the synthetic screenshots and diagnostics for inspection.
