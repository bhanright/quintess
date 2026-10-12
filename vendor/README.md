# Vendored crates

`vendor/` holds two kinds of things. PDFium and the OCR models are downloaded
binaries and stay out of git (docs/BUILDING.md says where to get them). The
three crates below are source: patched copies of crates.io releases, which
`[patch.crates-io]` in the workspace `Cargo.toml` uses instead of the
published ones. The window crates (winit, egui-wgpu) come first; lopdf, the
PDF object model, has a section of its own at the end.

Both window crates come from PdfCraft (<https://github.com/storytold/pdfcraft>,
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

## lopdf 0.45.0 (MIT)

The editable object model (`crates/quintess-core`, `Pdf`). The copy is
PdfCraft's `vendor/lopdf` at commit
`6d843a8e8b22e4442666eb9e12f506c40458af3d` (PdfCraft's changes: Copyright
(c) 2026 ArtCraft Team and the PdfCraft contributors, MIT OR Apache-2.0,
adapted with attribution), with Quintess's own patches on top. lopdf's
`LICENSE` is kept unchanged. Each changed file starts with a comment naming
where it came from; PdfCraft's changes are marked `PdfCraft patch`,
Quintess's `Quintess patch` with the reason. The published crate's tests are
kept but need assets it leaves out, so they don't run; the tests below do.

Checked on 2026-10-12: 0.45.0 is the newest lopdf release, so upgrading was
not an option. Drafts of upstream reports for Quintess's patches are in
`vendor/lopdf-upstream.md` (not filed yet). **Drop a patch** once a lopdf
release has the fix (or the equivalent) and its test still passes with
the release; drop the copy once no patch is left. To update, apply the
patches to the new release and keep `crates/quintess-core/tests/lopdf_patches.rs`
passing (it also checks that Cargo resolves lopdf to this copy and counts
the patch markers).

PdfCraft's patches (tests in PdfCraft's `inspect` module, plus
`a_png_predictor_with_absurd_columns_does_not_overflow` here):

| # | Files | What it fixes | Upstream |
|---|---|---|---|
| P1 | `filters/png.rs` | PNG predictor rows longer than the data are refused before allocating (a fuzzed `/Columns 4294967295` allocated two 4 GiB rows); empty frames allocate nothing | PdfCraft's own |
| P2 | `object.rs` | Predictor row widths saturate or are refused instead of overflowing (the `object.rs:1237` panic the mutation fuzzer found in Quintess) | lopdf#589 (part) |
| P3 | `object.rs` | Sub-byte TIFF predictor scratch bounded by the samples present | lopdf#589 |
| P4 | `reader.rs` | Lookup chains through compressed objects and indirect `/Length` keep their cycle history and stop at 16 (a crafted chain overflowed the stack) | PdfCraft's own |
| P5 | `object_stream.rs` | Object-stream members parsed within their own extent; repeated offsets skipped | PdfCraft's own |

Quintess's patches. Tests are in `crates/quintess-core/tests/lopdf_patches.rs`;
the pdf.js corpus files each fixed are checked there too when the corpus is
fetched.

| # | Files | What it fixes | Test | Drop when |
|---|---|---|---|---|
| Q1 | `reader.rs` | The password is prepared (SASLprep and UTF-8 for revisions 5 and 6, PDFDocEncoding for 2 to 4) before the file key is made, as it already was for the password check; a password those steps change was accepted but gave a wrong key (saslprep-r6.pdf) | `an_aes_256_password_is_prepared_with_saslprep_before_hashing`, `an_aes_128_password_is_converted_to_pdfdocencoding_before_hashing` | lopdf prepares the password in `Reader::authenticate_and_setup_encryption` |
| Q2 | `encryption/algorithms.rs` | In revisions 2 to 4 an owner password is turned into the user password it unlocks before the file key is made; the key came from the owner password and the contents stayed scrambled | `the_owner_password_of_an_aes_128_file_decrypts_it` | `compute_file_encryption_key` handles owner passwords |
| Q3 | `reader.rs` | An encryption dictionary written in the trailer (`/Encrypt << >>`) is used; lopdf expected a reference and took the file for unencrypted (issue6010_1.pdf, issue6010_2.pdf) | `an_encryption_dictionary_written_in_the_trailer_is_used` | lopdf accepts a direct `/Encrypt` |
| Q4 | `reader.rs`, `document.rs` | References inside the encryption dictionary (`/CF 7 0 R`, a crypt filter given by reference) are followed; the contents stayed encrypted and Quintess refused to save (issue7665.pdf) | `crypt_filters_kept_in_separate_objects_are_followed` | lopdf resolves references in `/Encrypt` before decrypting |
| Q5 | `parser/mod.rs` | Any whitespace may stand between a stream's data and `endstream`, and spaces or tabs before an indented `endstream` when the length is recovered; the stream was dropped (issue1721.pdf, issue5592.pdf) | `whitespace_before_endstream_keeps_the_stream`, `a_wrong_length_with_an_indented_endstream_is_recovered` | lopdf's stream parser accepts whitespace there |
| Q6 | `reader.rs` | When the object at a table entry's offset doesn't parse, the newest other copy of that object in the file that does is used, as PDFium does; the object was dropped (issue13783.pdf) | `a_damaged_latest_copy_falls_back_to_an_earlier_one` | lopdf falls back to earlier revisions |
| Q7 | `parser_aux.rs` | A cross-reference stream `/Index` start near the largest integer no longer overflows (the `parser_aux.rs:633` panic the fuzzer found) | `a_cross_reference_stream_index_near_the_largest_integer_does_not_overflow` | lopdf checks the addition |
| Q8 | `document.rs`, `processor.rs` | `traverse_objects` and `prune_objects` keep the references found in a set; searching a list made pruning quadratic (50 s per save of pdf.js's pdf.pdf) | `pruning_a_large_document_takes_linear_time` | lopdf uses a set (Quintess still prunes with its own `document::remove_unreachable_objects`, which can stay) |

Repairs that belong to Quintess rather than lopdf are in `Pdf::from_bytes`
(`crates/quintess-core/src/document.rs`): references with a generation the
file doesn't have are repointed at the one it has (issue7496.pdf), and a
catalog whose `/Pages` names a page gets a page tree (issue21436.pdf).
PDFium skips SASLprep too, so `engine::open_document` retries with the
prepared password.
