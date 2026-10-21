# Releasing PdfCraft

Every push to the `release` branch runs `.github/workflows/release.yml`. It builds Windows and
Linux packages, verifies the update-signing configuration, and creates or updates a **draft**
GitHub Release named `PdfCraft v<version>`. A maintainer tests and publishes the draft.

## Cutting a release

1. Set the signing configuration below. The workflow checks it before starting the package builds.
2. **Bump the version on `main`** when that version has already been published:

   ```sh
   cargo xtask version
   cargo xtask version set 0.5.1
   ```

   Commit `Cargo.toml` and `Cargo.lock` through the normal review flow.
3. **Merge or fast-forward `main` into `release` and push.** Alternatively, select the `release`
   branch under **Actions > Release > Run workflow**. The optional `version` input applies to every
   build in that run; leave it empty to use `Cargo.toml`.
4. **Wait for the draft.** It contains Windows x64, x86 and ARM64 packages, Linux x86_64 and ARM64
   packages, `SHA256SUMS.txt`, and its Ed25519 signature `SHA256SUMS.txt.sig`.
5. **Test the packages** on Windows and Linux. Craftworks uses the Windows portable ZIP and Linux
   AppImage. A warning about a missing Windows certificate means Authenticode signing is absent;
   the separate update-checksum signature is always required.
6. **Publish the draft** in the GitHub UI. Use a stable version for Craftworks' normal update
   channel; versions with a suffix such as `-rc.1` are marked as pre-releases.

The workflow only creates a draft when running on `release`. Dispatches from other branches are
build checks and depend on the `release` environment's branch rules. Configure those rules to
permit `release` for production signing.

Rebuilding an unpublished version replaces its draft assets. A published version is never replaced:
bump it first. The workflow does not publish automatically, and it cannot draft a release without
all required packages and a valid signature from the configured key.

## What gets built

| Platform | Artifacts | Built on |
|---|---|---|
| Windows 10+ x64 | `pdfcraft-<v>-windows-x64.msi`, `pdfcraft-<v>-windows-x64-portable.zip` | `windows-latest` |
| Windows 10+ x86 (32-bit) | `pdfcraft-<v>-windows-x86.msi`, `pdfcraft-<v>-windows-x86-portable.zip` | `windows-latest` |
| Windows 11 on ARM64 | `pdfcraft-<v>-windows-arm64.msi`, `pdfcraft-<v>-windows-arm64-portable.zip` | `windows-latest` (cross-compiled) |
| Linux x86_64 | `pdfcraft-<v>-linux-x86_64.{AppImage,AppImage.zsync,deb,rpm,tar.gz}`, `pdfcraft-cli-<v>-linux-x86_64.tar.gz` | `ubuntu-22.04` |
| Linux aarch64 | `pdfcraft-<v>-linux-aarch64.{AppImage,AppImage.zsync,deb,rpm,tar.gz}`, `pdfcraft-cli-<v>-linux-aarch64.tar.gz` | `ubuntu-22.04-arm` |

The ARM64 Windows build is cross-compiled on the x64 runner, so signing and WiX work as for the
other Windows builds; `.github/workflows/windows-arm64.yml` installs and runs it on ARM64 hardware.

Every binary reports its version (`pdfcraft --version`, `pdfcraft-cli --version`, Help > About).
The workflow sets `PDFCRAFT_BUILD_SHA` and `PDFCRAFT_BUILD_DATE`; the date is included in the
AppStream metadata. Local packaging fills in these values when they are absent.

**Fonts:** every job checks out [craft-fonts](https://github.com/storytold/craft-fonts) at the commit
pinned in `release.yml` and builds with `CRAFT_FONTS_DIR` and `CRAFT_FONTS_REQUIRED=1`, so releases
embed its Japanese fonts and fail rather than ship without them (`AGENTS.md` §1.4). Bump the pin
deliberately.

**OCR models:** every desktop package ships the Scan & OCR models. The packaging scripts run
`cargo xtask models` to fetch each `kind = "model"` file in `ATTRIBUTION.toml`, verified by SHA-256,
with its licence and attribution text. Models live in `models\` beside the Windows executable
and in `share/pdfcraft/models` beside `bin/` on Linux. Packaging needs network access.

### Windows

`packaging/windows/package.ps1 -Arch x64|x86|arm64` builds with `-C target-feature=+crt-static`, so
neither the MSI nor the portable zip needs the Visual C++ redistributable.

- Before packaging, it reads both executables' PE headers: the machine type must match `-Arch`, and
  `pdfcraft.exe` must be a GUI-subsystem program (no console window, #57) while `pdfcraft-cli.exe`
  stays a console program.
- `pdfcraft.wxs` (WiX v5) installs per machine into Program Files with a Start Menu shortcut, a
  desktop shortcut (on by default) and an App Paths entry. Both shortcuts are plain links to
  `pdfcraft.exe`, not advertised MSI shortcuts (#107, #143). The MSI version is the numeric `X.Y.Z`
  (MSI has no pre-release field), and same-version upgrades are allowed so release candidates replace
  each other. Icon ids end in `.ico` or `.exe` (Windows Installer requires it); packaging-lint checks
  this.
- `installer-ui.wxs` supplies native welcome, maintenance, progress, files-in-use and outcome
  dialogs. Full UI confirms success with Finish; failures and cancellations have distinct messages.
  The welcome dialog has a "Create a desktop shortcut" checkbox, ticked by default. `/qn` and `/qb`
  stay unattended and create the desktop shortcut unless `INSTALLDESKTOPSHORTCUT=0` is passed.
- `test-msi.ps1 <file.msi>` checks the compiled shortcut and dialog tables. Packaging runs it in a
  child process before signing, so the MSI isn't held open when signtool runs. The ARM64 install
  smoke test checks that both all-users shortcuts point at the installed `pdfcraft.exe` and are
  removed on uninstall, and that `INSTALLDESKTOPSHORTCUT=0` skips the desktop one.
- The portable zip holds both executables, the README, the licences, the OFL licence of each
  embedded craft-fonts family, and `portable.txt`. That marker beside `pdfcraft.exe` keeps the
  settings, logs, crash recovery and new digital IDs in `PdfCraftData\` next to the exe instead of
  `%APPDATA%` and `%LOCALAPPDATA%` (`crates/ui-egui/src/portable.rs`).
- **Signing:** `packaging/windows/sign.ps1` signs both executables and the MSI with `signtool`
  (SHA-256, RFC 3161 timestamp), from a `.pfx` (`WINDOWS_CERTIFICATE`) or Azure Trusted Signing
  (`AZURE_*`), whichever is configured. It is the one place to change when signing changes.

Locally: `dotnet tool install -g wix --version 5.0.2`, then `pwsh packaging/windows/package.ps1 -Arch x64`.

### Linux

The [Ubuntu guide](ubuntu.md) covers installation and source builds on Ubuntu 24.04 / 26.04 LTS
x86_64. `ubuntu.yml` installs the same `.deb` in both distributions and checks CLI and desktop
startup with the automatic renderer and OpenGL.

`packaging/linux/package.sh` stages one FHS tree (both binaries, the desktop entry, hicolor icons,
AppStream metainfo) and makes every format from it: an **AppImage** (any distribution, nothing to
install), a **.deb** and an **.rpm** (built with [nfpm](https://nfpm.goreleaser.com) from
`nfpm.yaml`; they integrate with the menu, MIME and icon caches), a **.tar.gz**, and a CLI-only
**`pdfcraft-cli-<v>-linux-<arch>.tar.gz`** (the stripped `pdfcraft-cli` plus the licences and
README, for servers, CI and agent sandboxes). `--formats` picks a subset (`appimage deb rpm tar cli`).

Official release binaries are built on Ubuntu 22.04, so they need only **glibc ≥ 2.35**:
Ubuntu 22.04+, Debian 12+, Fedora 36+, RHEL 10. Local builds can require a newer glibc; the `.deb`
dependency is derived from both binaries' required GLIBC symbols, with 2.35 as its minimum.
Windowing (X11, Wayland,
xkbcommon) and the GPU (Vulkan, EGL) are loaded at runtime from the system; the .deb and .rpm declare
them as dependencies (see `nfpm.yaml`).

Each AppImage embeds update information
(`gh-releases-zsync|treeforjs|pdfcraft|latest|pdfcraft-*-linux-<arch>.AppImage.zsync`), and its
`.zsync` is published beside it, so AppImageUpdate and AppImageLauncher can update it in place,
downloading only the changed blocks. `latest` is the newest published, non-pre-release version.
`package.sh` writes the `.zsync` when `zsyncmake` (the `zsync` package) is installed and warns
otherwise; the release job checks both.

Locally (on Linux, with nfpm): `packaging/linux/package.sh` or `--formats "deb tar"`.

## Signing configuration

Under **Settings > Environments > release**, add secret **`UPDATE_SIGNING_KEY`** containing the
complete Ed25519 private-key PEM, including its BEGIN and END lines. Keep the private key out of
source control and downloaded packages.

Under **Settings > Secrets and variables > Actions > Variables**, add repository variable
**`CRAFT_UPDATE_PUBLIC_KEY`** containing the matching public key as a single line of base64 DER.
Every desktop build embeds this public key. Generate it from your private key on Linux with:

```sh
openssl pkey -in ~/.craft-signing/key.pem -pubout -outform DER | base64 -w0
echo
```

Base64 of the raw 32-byte Ed25519 public key is also accepted. Both values are required for
production releases. Before building, the workflow checks the public key and verifies that the
private key matches it. Before drafting, it signs the checksums, checks the signature length,
and verifies the signature with the public key. A missing, malformed or mismatched key fails the
run. See [Craftworks key setup](https://github.com/treeforjs/craftworks/blob/main/docs/updates.md)
for key generation and backup instructions.

Windows Authenticode signing is optional and separate from update verification. These additional
secrets also live in the `release` environment:

| Secret | Used for |
|---|---|
| `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD` | base64 `.pfx` code-signing certificate and its password |
| `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET` | service principal for Azure Trusted Signing (instead of a `.pfx`) |
| `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_CERT_PROFILE` | Trusted Signing endpoint, account and certificate profile |

`GITHUB_TOKEN` creates the draft; only the final job gets `contents: write`.

## Checks

`.github/workflows/packaging-lint.yml` checks workflow syntax, shell and PowerShell scripts, and
package metadata on pull requests. Windows packaging checks PE architecture and subsystem values,
MSI tables and the CLI version. Linux packaging checks the installed files, OCR models, CLI archive,
AppImage startup and its update metadata before uploading the artifacts.

## Local scripting-kernel bootstrap

Native and browser builds embed an isolated generated scripting kernel. Before local packaging,
run `rustup target add wasm32-wasip1` and `cargo xtask js-sandbox`. Release workflows perform
this explicit step after any version override. Do not commit the generated Wasm module.
See [development.md](development.md#contained-document-scripting) for the runtime limits.
