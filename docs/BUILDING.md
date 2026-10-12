# Building Quintess

Quintess is a Rust workspace that builds one program, `quintess`, which is
both the desktop app and the command-line tool:

- `crates/quintess-core`: the engine.
- `crates/quintess-app`: the desktop app, a library (`run_app`).
- `crates/quintess-cli`: the command-line tool, a library (`run_cli`), and
  the `quintess` program itself (`src/main.rs`), which runs the tool when
  the first argument is a command or an option and starts the app
  otherwise (`quintess report.pdf`; `quintess -- merge` opens a file named
  "merge").
- `crates/quintess-console`: that rule, shared, and on Windows the console
  launcher `quintess-console.exe`, installed as `quintess.com` (see
  "Windows console" below).

It builds on Windows 10 and 11 (x64, and ARM64: see "Windows on ARM"), on
macOS 11 and later (Apple silicon and Intel: see "macOS") and on 64-bit
Linux (see "Linux").

## What you need

- Rust (stable, MSVC toolchain): <https://rustup.rs>. The Visual Studio
  Build Tools with the "Desktop development with C++" workload provide the
  linker and the Windows SDK (whose resource compiler embeds the icon).
- PDFium, which renders pages. It is not in the repository; download the
  Windows x64 build of **chromium/8086** from pdfium-binaries:
  <https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium%2F8086>
  (`pdfium-win-x64.tgz`) and unpack it so these exist:

  ```
  vendor/pdfium/bin/pdfium.dll
  vendor/pdfium/LICENSE
  vendor/pdfium/licenses/
  ```

  Quintess finds the DLL next to the program, in a `pdfium` folder there, or
  in the folder named by the `QUINTESS_PDFIUM_DIR` environment variable; debug
  builds also look in the repository's `vendor/pdfium/bin`.

## Build and test

```
cargo build -j 6
cargo test -j 6 --workspace
```

The repository's `.cargo/config.toml` links the C runtime statically (so
the programs start without the Visual C++ redistributable). To build on
another drive, make `target` a directory junction to a folder there
(`mklink /J target E:\QuintessBuild`), as the author does.

Two crates are built from patched copies in the repository rather than
from crates.io: `vendor/winit` and `vendor/egui-wgpu` (window and input
fixes, taken from PdfCraft; `[patch.crates-io]` in `Cargo.toml`). They are
part of the source, unlike the PDFium and OCR downloads beside them, and
need no extra step. `vendor/README.md` lists the patches and when to drop
the copies; `crates/quintess-app/tests/vendored_window_crates.rs` fails if
Cargo stops using them or a patch goes missing.

The OCR test runs only when the OCR models are present (installed, or
downloaded once by `quintess ocr`); tests never use the network.

Visual check of the app: `QUINTESS_SCREENSHOT=out.png quintess file.pdf`
saves a picture of the window and exits (`QUINTESS_SETUP` options are
documented on `SelfCheck` in `crates/quintess-app/src/app.rs`).

## Windows console

On Windows `quintess.exe` is a Windows (GUI) program, in debug builds too,
so opening a PDF from Explorer or the Start menu shows no console window.
Run with a command from a terminal, it joins the terminal's console
(`AttachConsole`) and writes there, keeping any redirected streams; but cmd
and PowerShell don't wait for a GUI program, so its output can land after
the next prompt. `quintess-console.exe`, installed as `quintess.com` beside
it, fixes that: both shells try `.com` before `.exe`, and the launcher runs
`quintess.exe` with its own console streams, waits and returns the exit
code (for the app it starts it and returns at once). To try it from a
build, copy `target\release\quintess-console.exe` to `quintess.com` in the
same folder.

## Release

1. `cargo build --release -j 6` (about 8 minutes; thin LTO, stripped).
2. `python tools/third_party_notices.py` writes
   `licenses/THIRD-PARTY-NOTICES.txt` from the dependencies actually built.
3. Copy the OCR models into `vendor/ocrs-models/` (from
   `%LOCALAPPDATA%\Quintess\models` after running `quintess ocr` once). They are
   licensed CC BY-SA 4.0; the notices file credits them.
4. Compile the installer with Inno Setup 6: `ISCC installer\quintess.iss`
   (set `QUINTESS_BUILD` to the Cargo target folder if it isn't `target`).
5. Write the installer's checksum beside it, as the update check expects:
   `(Get-FileHash installer\output\Quintess-<version>-setup.exe).Hash.ToLower() + "  Quintess-<version>-setup.exe"`
   into `Quintess-<version>-setup.exe.sha256`.
6. Publish both files on a GitHub release tagged `v<version>`.

`.github/workflows/windows-release.yml` does all of this on GitHub's
runners for x64 and ARM64 (Actions > Windows release > Run workflow, and on
every `v*` tag, where it attaches both installers and their `.sha256`
files to the release). It also installs each installer silently, checks
the installed files against the layout above
(`tools/check_windows_install.py`), runs the command line and takes a
self-check screenshot. The notices file and the version in
`installer/quintess.iss` are taken from the repository, so update both
before tagging.

## Windows on ARM

The same sources build for Windows on ARM (`aarch64-pc-windows-msvc`).
Every dependency builds for it: ring needs `clang` on the PATH for its ARM
assembly (the LLVM installer, or `winget install LLVM.LLVM`), and the rest
(Boa, the windows crate, WinRT speech through `tts`, rten's NEON code for
OCR, spellbook) need nothing extra. PDFium comes from the same
pdfium-binaries release: `pdfium-win-arm64.tgz`, unpacked into
`vendor/pdfium-win-arm64`.

On an ARM64 PC (or GitHub's `windows-11-arm` runner) with the ARM64 Rust
toolchain:

```
cargo build --release -j 6 --target aarch64-pc-windows-msvc
set QUINTESS_PDFIUM_DIR=%CD%\vendor\pdfium-win-arm64\bin
cargo test -j 6 --workspace --target aarch64-pc-windows-msvc
ISCC /DArch=arm64 installer\quintess.iss
```

The installer is `installer\output\Quintess-<version>-arm64-setup.exe`. It
can also be cross-built on an x64 PC (`rustup target add
aarch64-pc-windows-msvc`, the Visual Studio "ARM64 build tools" component
and clang), but not tested there. `.cargo/config.toml` links the C runtime
statically for both Windows targets.

The ARM64 installer allows only ARM64 PCs (`ArchitecturesAllowed=arm64`):
its program can't run on an x64 one. It shares the x64 installer's AppId
and folder, so it upgrades an x64 installation in place. The x64 installer
still installs on ARM64 PCs, which run it under emulation, but first
suggests the ARM64 one; and the update check (`updates::pick_installer`)
offers ARM64 PCs the ARM64 installer even from the x64 build
(`system::native_architecture` sees through the emulation).

## macOS

The workspace builds on macOS 11 (Big Sur) and later, with Xcode's
command-line tools (`xcode-select --install`) and Rust from rustup. Unpack
PDFium **chromium/8086** from pdfium-binaries into `vendor/pdfium-mac`:
`pdfium-mac-arm64.tgz` (or `pdfium-mac-x64.tgz` on an Intel Mac) for
development, `pdfium-mac-univ.tgz` (both processors) for a release. Then:

```
QUINTESS_PDFIUM_DIR=$PWD/vendor/pdfium-mac/lib cargo test -j 6 --workspace
```

The release is one universal program in an app bundle on a disk image. With
both targets installed (`rustup target add aarch64-apple-darwin
x86_64-apple-darwin`), the OCR models in `vendor/ocrs-models`, the
universal PDFium and the notices regenerated:

```
python3 tools/package_macos.py
```

builds both targets, joins them with `lipo`, assembles `Quintess.app`
(`Contents/MacOS/quintess`, `Contents/Frameworks/libpdfium.dylib`,
`Contents/Resources` with the icon, `dictionaries`, `models` and
`licenses`, and `Contents/Info.plist` from `installer/macos/Info.plist.in`,
which declares PDFs so the Finder's Open With lists Quintess), signs it and
writes `installer/output/Quintess-<version>-macos.dmg` with a `.sha256`.
`quintess_core::system` finds the bundle's parts (`resources_folder`,
`libraries_folder`); PDFium is opened by its full path, so the program
needs no rpath. The icon, `assets/macos/quintess.icns`, comes from
`tools/make_icons.py`.

Signing: without an Apple Developer ID the bundle is ad-hoc signed
(`codesign -s -`), which Apple silicon requires of every program but
Gatekeeper doesn't trust, so people open it the first time as the user
guide's "Installing on macOS" explains. To sign for real, add the GitHub
secrets `APPLE_CERTIFICATE` (a base64 Developer ID Application .p12),
`APPLE_CERTIFICATE_PASSWORD` and optionally `KEYCHAIN_PASSWORD`;
`installer/macos/import-signing-certificate.sh` then imports it and
`package_macos.py` signs with the hardened runtime
(`installer/macos/entitlements.plist`). Adding `APPLE_ID`, `APPLE_PASSWORD`
(an app-specific password) and `APPLE_TEAM_ID` also notarizes and staples
the app and the disk image. Locally, set `MACOS_SIGNING_IDENTITY` (and the
notarization variables) before running the script.

`.github/workflows/macos-release.yml` runs the tests on Apple silicon,
packages the disk image, installs it on an Apple silicon and an Intel
runner, runs the command line, takes self-check screenshots (including one
of a PDF opened with `open -a`, the way the Finder opens files) and, on
`v*` tags, attaches the disk image to the release.

What differs on macOS: the Finder sends files to open as Apple events,
received in `crates/quintess-app/src/desktop.rs`, which also points the
application menu's Quit at the window's close button so unsaved changes are
asked about; settings and data are in `~/Library/Application
Support/Quintess`; the Windows core fonts that macOS also ships are found
by their Windows file names (`system_fonts::find`); the UI uses San
Francisco and Helvetica Neue; printing uses CUPS as on Linux; scanning
needs SANE from Homebrew; the certificate store, Microsoft Office
conversion and self-update are Windows only. The packaging is adapted from
PdfCraft (MIT OR Apache-2.0; credited in each file and in the notices).

## Linux

The same workspace builds on 64-bit Linux. On Ubuntu or Debian:

```
sudo apt install build-essential pkg-config libclang-dev libspeechd-dev \
  libxkbcommon-dev libgl1-mesa-dev libegl1-mesa-dev libwayland-dev libx11-dev \
  fonts-liberation fonts-dejavu-core
```

(`libclang-dev` and `libspeechd-dev` build the read-aloud bindings; the
others are what the window and the tests need.) Install Rust with rustup,
then unpack the Linux build of PDFium **chromium/8086**
(`pdfium-linux-x64.tgz` from the same pdfium-binaries release) into
`vendor/pdfium-linux`, so `vendor/pdfium-linux/lib/libpdfium.so` exists, and
point the tests at it:

```
QUINTESS_PDFIUM_DIR=$PWD/vendor/pdfium-linux/lib cargo test -j 6 --workspace
```

Packages: with the OCR models in `vendor/ocrs-models` and the notices
regenerated, `python3 tools/package_linux.py` builds the release program
and writes `installer/output/quintess_<version>_amd64.deb` and
`quintess-<version>-linux-x86_64.tar.gz`, each with a `.sha256` file. It
reads the glibc version the program needs and the libraries it links from
the built file, and writes the .deb's dependencies from them. The program
needs the glibc of the system it was built on or later, so releases are
built on Ubuntu 22.04 (glibc 2.35) by `.github/workflows/linux-release.yml`,
which also installs both packages, takes self-check screenshots under Xvfb
and attaches the packages to the release on version tags.

AppImage: `python3 tools/package_linux.py --appimage --appimagetool FILE
--appimage-runtime FILE` also writes `Quintess-<version>-x86_64.AppImage`
(+ `.sha256`). It needs `patchelf` and `libspeechd2` on the build system, and
appimagetool 1.9.1 and the type2-runtime release 20251108 (the workflow
downloads both and checks their checksums). The AppDir is the tarball's
program folder under `usr/lib/quintess`, plus the build system's
`libspeechd.so.2` (the program's RUNPATH is set to `$ORIGIN` in that copy
only), `installer/linux/appimage/AppRun`, and the desktop file, icons and
AppStream metadata named `cc.quintess.Quintess`.

Flatpak: `installer/linux/flatpak/build-bundle.sh quintess-<version>-linux-x86_64.tar.gz`
writes `Quintess-<version>-x86_64.flatpak` (+ `.sha256`) beside the tarball.
It needs `flatpak` and `flatpak-builder`; the manifest,
`installer/linux/flatpak/cc.quintess.Quintess.yml`, repackages the tarball
against org.freedesktop.Platform 26.08 and builds libspeechd and CUPS's
`lp`, `lpstat` and `lpoptions`, which the runtime lacks. Its permissions are
explained in its comments, and notes at its end list what a from-source
Flathub submission needs. The workflow builds both, runs them under Xvfb
(the AppImage with `--appimage-extract-and-run`, as runners have no FUSE)
and sends a print job from inside the Flatpak to a CUPS queue on the runner.
The metadata template is `installer/linux/cc.quintess.Quintess.metainfo.xml.in`.

From Windows, the Linux program can be cross-built (not run) with zig:
`pip install ziglang cargo-zigbuild libclang`, the headers and library of
Ubuntu's `libspeechd-dev`/`libspeechd2` unpacked into a sysroot, then
`cargo zigbuild --target x86_64-unknown-linux-gnu.2.31` with `LIBCLANG_PATH`,
`BINDGEN_EXTRA_CLANG_ARGS` (zig's glibc headers and the sysroot) and
`-L <sysroot>/usr/lib/x86_64-linux-gnu` set. `tools/package_linux.py
--skip-build --target x86_64-unknown-linux-gnu.2.31` then packages them.

Install layout (the same on Windows: everything beside the program):
the `quintess` program, `libpdfium.so`, `dictionaries/`, `models/` and
`licenses/` live in one folder, `/usr/lib/quintess` for the .deb and
`~/.local/lib/quintess` or `/opt/quintess` for the tarball, with
`quintess` linked from a `bin` folder (there is no console launcher on
Linux). The program finds its files through its own (link-resolved)
location. Settings go to
`~/.config/quintess`, data (models, recovered documents, crash log) to
`~/.local/share/quintess`, following `XDG_CONFIG_HOME` and `XDG_DATA_HOME`.

## Logo and icons

`tools/make_logo.py` generated the logo candidates through OpenRouter;
the chosen one is `logo_source/quintess-logo-source.png`.
`tools/make_icons.py` turns it into `assets/` (the app icon, the window
icon, the `.ico`, the mark used in the app's header and the Linux hicolor
icons in `assets/linux/icons`), and
`site/tools/make_images.py` makes the website's icons from those.

`assets/fonts/quintess-icons.otf` is the app's icon font where Segoe Fluent
Icons is missing (Linux): `tools/make_icon_font.py` cuts it from Google's
Material Icons (Outlined, Apache-2.0) and moves each glyph to the code point
of the Segoe icon it replaces. A new icon in `theme.rs` needs a line in that
script's table and a rerun.
