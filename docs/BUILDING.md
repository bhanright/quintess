# Building Vellum

Vellum is a Rust workspace: `crates/vellum-core` (the engine),
`crates/vellum-cli` (the `vellum` command) and `crates/vellum-app` (the
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

  Vellum finds the DLL next to the program, in a `pdfium` folder there, or
  in the folder named by the `VELLUM_PDFIUM_DIR` environment variable; debug
  builds also look in the repository's `vendor/pdfium/bin`.

## Build and test

```
cargo build -j 6
cargo test -j 6 --workspace
```

The repository's `.cargo/config.toml` links the C runtime statically (so
the programs start without the Visual C++ redistributable) and, on the
author's machine, puts build output on another drive (`target-dir`);
remove that line to build into `target/`.

The OCR test runs only when the OCR models are present (installed, or
downloaded once by `vellum ocr`); tests never use the network.

## Release

1. `cargo build --release -j 6` (about 8 minutes; thin LTO, stripped).
2. `python tools/third_party_notices.py` writes
   `licenses/THIRD-PARTY-NOTICES.txt` from the dependencies actually built.
3. Copy the OCR models into `vendor/ocrs-models/` (from
   `%LOCALAPPDATA%\Vellum\models` after running `vellum ocr` once). They are
   licensed CC BY-SA 4.0; the notices file credits them.
4. Compile the installer with Inno Setup 6: `ISCC installer\vellum.iss`
   (set `VELLUM_BUILD` to the Cargo target folder if it isn't `target`).
