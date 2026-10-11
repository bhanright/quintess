# Building Quintess

Quintess is a Rust workspace: `crates/quintess-core` (the engine),
`crates/quintess-cli` (the `quintess` command) and `crates/quintess-app` (the
desktop app). It builds on Windows 10 and 11 (x64).

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

The OCR test runs only when the OCR models are present (installed, or
downloaded once by `quintess ocr`); tests never use the network.

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

## Logo and icons

`tools/make_logo.py` generated the logo candidates through OpenRouter;
the chosen one is `logo_source/quintess-logo-source.png`.
`tools/make_icons.py` turns it into `assets/` (the app icon, the window
icon, the `.ico` and the mark used in the app's header), and
`site/tools/make_images.py` makes the website's icons from those.
