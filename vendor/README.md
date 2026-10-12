# Vendored crates

`vendor/` holds two kinds of things. PDFium and the OCR models are downloaded
binaries and stay out of git (docs/BUILDING.md says where to get them). The
two crates below are source: patched copies of crates.io releases, which
`[patch.crates-io]` in the workspace `Cargo.toml` uses instead of the
published ones.

Both copies come from PdfCraft (<https://github.com/storytold/pdfcraft>,
MIT OR Apache-2.0, the same license as Quintess), `vendor/winit` and
`vendor/egui-wgpu` at commit `6d843a8e8b22e4442666eb9e12f506c40458af3d`.
PdfCraft's changes are Copyright (c) 2026 ArtCraft Team and the PdfCraft
contributors. Each upstream license file is kept unchanged; each file
PdfCraft changed starts with a comment naming where it came from, and each
change in it is marked `PdfCraft patch`. Quintess's own changes to the copies
are those header comments and two comments marking changes PdfCraft left
unmarked (`winit/src/platform_impl/macos/mod.rs`, `windows/event_loop.rs`).
`licenses/THIRD-PARTY-NOTICES.txt` (`tools/third_party_notices.py`) lists
both crates as modified.

`crates/quintess-app/tests/vendored_window_crates.rs` checks that Cargo
resolves both crates to these copies and that every patch is still in them;
`crates/quintess-app/src/window_start.rs` tests `catch_errors` and the
renderer error with a real GPU device. Most of the bugs need hardware or an
input method to show, which is why most tests read the source.

**Drop a copy** (and its tests) once eframe depends on an upstream release
that has the fixes: some winit patches are merged on winit's master
branch (0.31), others are pull requests or PdfCraft's own; the egui-wgpu surface-size
bug is reported as emilk/egui#8361. To update a copy,
take PdfCraft's newer one, or apply the patches to the new release, and keep
the tests passing. Checked on 2026-10-11: the newest releases are winit
0.30.13 (0.31 is in beta, and eframe 0.36 needs 0.30) and egui-wgpu 0.36.2,
so upgrading was not an option.

## winit 0.30.13 (Apache-2.0)

Used by both renderers, so every patch reaches Quintess on its system.

| # | System | What it fixes | Upstream |
|---|---|---|---|
| 1 | Windows 11 | A window dragged onto a monitor with another scale factor grew and stayed on the wrong monitor at the wrong scale: winit nudged it back onto the monitor being left. Windows 11 now applies the rect Windows suggests. | winit#4341 (master) |
| 2 | Wayland | Losing any seat capability (a tablet unplugged, a remote session ending) destroyed the seat's text input, so typing through an input method stopped until restart. | winit#4747 |
| 3 | Windows | The Microsoft Korean IME never reports a composition cursor, so winit read 0 and egui drew no caret while composing. The cursor is now at the end of the composition unless the IME reports one. | winit#4746 |
| 4 | Windows | A hidden window restored maximized was shown white before its first frame. Quintess does not restore maximized windows today, so this is dormant. | winit#4587 |
| 5 | X11 | Switching text fields replaced the input context without focusing it, so IBus or fcitx5 typing stopped until the window lost and regained focus. | winit#4727 |
| 6 | macOS | Input methods that end a composition before inserting text lost the text; Apple Korean dropped the key after a syllable. | winit#4650 / #4651 |
| 7 | macOS | Leaving a text field mid-composition kept the syllable, which came back in the next field. | winit#4745 |
| 8 | macOS | Text from the emoji picker was dropped. | winit#4749 (part) |
| 9 | macOS | Apple Korean's Space arrived twice. | (to be proposed) |
| 10 | macOS | The first key in a fresh app reached Apple Korean as a raw jamo (ㅎㅏㄴ instead of 한). | winit#4744 |
| 11 | X11 | Without `Xft.dpi`, the scale now comes from the X screen's configured DPI before the panel's physical density, as other toolkits do (`WINIT_X11_SCALE_FACTOR` still overrides). | PdfCraft's own |

The published crate's `docs/res/*.svg` and `examples/data/*.png` are left
out, so its `window` example does not build; nothing in Quintess uses it.

## egui-wgpu 0.36.2 (MIT OR Apache-2.0)

Used only when wgpu draws (the Graphics preference "DirectX / Vulkan", or
the fallback when OpenGL can't start). OpenGL has neither problem.

| # | What it fixes |
|---|---|
| 1 | A window larger than the GPU's texture limit (restored at another monitor's scale, stretched across monitors) panicked in `Surface::configure`. The surface is now fitted within the limit (`winit::surface_fit`) and egui draws at the matching scale. Quintess also asks the device for the GPU's real limit (often 16384, not egui-wgpu's 8192; `window_start.rs`), so fitting is rarely needed. |
| 2 | A driver that couldn't configure the window's surface, or rejected the renderer's pipelines, panicked before the app started. Both now run inside wgpu error scopes (`winit::catch_errors`) and are returned as `WgpuError::ConfigureSurface` / `CreateRenderer`, so `run_native` returns an error and Quintess tries OpenGL. Still a panic: an error in a later reconfigure (resize, a lost surface) and a device lost during the first configure. |

The published crate leaves out egui's license files; `LICENSE-APACHE` and
`LICENSE-MIT` are from the egui 0.36.2 tag (as PdfCraft took them).
