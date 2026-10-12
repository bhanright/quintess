//! Keeps the patched window crates in vendor/ in the build, and their
//! patches in them (vendor/README.md lists each patch and the bug it fixes).
//!
//! Most of these bugs need hardware or an input method to show (two monitors
//! with different scale factors, the Microsoft Korean IME, an X11 or Wayland
//! session, macOS), so these tests read the vendored source. A dependency
//! update that resolves a crate from crates.io again, or a re-vendored copy
//! without a patch, would quietly bring the bug back: re-apply the patch, or
//! drop the copy (and its tests here) once an upstream release has the fix.
//! The behavior these patches add is tested by real calls where it can be:
//! `surface_fit` below, and `catch_errors` with a real GPU device in
//! src/window_start.rs.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A vendored file, with Windows line endings (should git add them) undone.
fn vendored(relative: &str) -> String {
    let path = workspace_root().join("vendor").join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())).replace("\r\n", "\n")
}

/// The part of `text` after `start`, up to the first `end` after it.
fn section<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let (_, after) = text.split_once(start).unwrap_or_else(|| panic!("{start:?} not found"));
    after.split_once(end).map_or(after, |(body, _)| body)
}

fn assert_resolves_to_vendor(crate_name: &str) {
    let lock = std::fs::read_to_string(workspace_root().join("Cargo.lock")).unwrap().replace("\r\n", "\n");
    let entry = lock.split("[[package]]").find(|package| package.contains(&format!("\nname = \"{crate_name}\"\n"))).unwrap_or_else(|| panic!("{crate_name} is not in Cargo.lock"));
    assert!(!entry.contains("\nsource = "), "{crate_name} must resolve to vendor/{crate_name} ([patch.crates-io] in Cargo.toml), not:{entry}");
}

#[test]
fn both_window_crates_resolve_to_their_vendored_copies() {
    assert_resolves_to_vendor("winit");
    assert_resolves_to_vendor("egui-wgpu");
}

#[test]
fn every_changed_vendored_file_names_its_origin() {
    for relative in [
        "winit/src/platform_impl/linux/wayland/seat/mod.rs",
        "winit/src/platform_impl/linux/x11/event_processor.rs",
        "winit/src/platform_impl/linux/x11/ime/mod.rs",
        "winit/src/platform_impl/linux/x11/util/randr.rs",
        "winit/src/platform_impl/macos/ime_startup.rs",
        "winit/src/platform_impl/macos/mod.rs",
        "winit/src/platform_impl/macos/view.rs",
        "winit/src/platform_impl/windows/dark_mode.rs",
        "winit/src/platform_impl/windows/event_loop.rs",
        "winit/src/platform_impl/windows/ime.rs",
        "winit/src/platform_impl/windows/window_state.rs",
        "egui-wgpu/src/lib.rs",
        "egui-wgpu/src/winit.rs",
    ] {
        let text = vendored(relative);
        assert!(text.starts_with("// Vendored for Quintess from PdfCraft"), "vendor/{relative} lost its attribution header");
        assert!(text.contains("Copyright (c) 2026 ArtCraft Team and the\n// PdfCraft contributors"), "vendor/{relative} lost PdfCraft's copyright line");
        assert!(text.contains("PdfCraft patch"), "vendor/{relative} has no marked patch");
    }
    // The upstream license files are kept unchanged.
    for license in ["winit/LICENSE", "egui-wgpu/LICENSE-APACHE", "egui-wgpu/LICENSE-MIT"] {
        assert!(workspace_root().join("vendor").join(license).is_file(), "vendor/{license} is missing");
    }
}

// --- winit ---------------------------------------------------------------

/// winit patch 1: on Windows 11, a window dragged onto a monitor with
/// another scale factor stays there at that scale.
#[test]
fn winit_applies_the_windows_11_suggested_rect_on_a_dpi_change() {
    let event_loop = vendored("winit/src/platform_impl/windows/event_loop.rs");
    assert!(event_loop.contains("if !WIN10_BUILD_VERSION.is_some_and(|build| build < 22000) {\n                new_outer_rect = suggested_rect;"), "the WM_DPICHANGED patch is gone");
}

/// winit patch 2: Wayland keeps a seat's text input (IME) until the seat
/// goes, not until it loses any capability.
#[test]
fn winit_keeps_the_wayland_text_input_until_the_seat_goes() {
    let seat = vendored("winit/src/platform_impl/linux/wayland/seat/mod.rs");
    let (before_remove_seat, _) = seat.split_once("fn remove_seat(").expect("SeatHandler::remove_seat");
    let remove_capability = section(before_remove_seat, "fn remove_capability(", "\n    }\n");
    let remove_seat = section(&seat, "fn remove_seat(", "\n    }\n");
    assert!(!remove_capability.contains("text_input.destroy()"), "losing a capability destroys the seat's text input again");
    assert!(remove_seat.contains("text_input.destroy()"), "removing the seat no longer destroys its text input");
}

/// winit patch 3: on Windows, the composition cursor is read only when the
/// IME reports one (the Microsoft Korean IME never does).
#[test]
fn winit_reads_the_windows_ime_cursor_only_when_it_is_reported() {
    let ime = vendored("winit/src/platform_impl/windows/ime.rs");
    assert!(ime.contains("if composition_flags & GCS_CURSORPOS != 0 {"), "GCS_CURSORPOS is read unconditionally again");
    let event_loop = vendored("winit/src/platform_impl/windows/event_loop.rs");
    assert!(event_loop.contains("get_composing_text_and_cursor(lparam as u32)"), "WM_IME_COMPOSITION no longer passes its flags on");
}

/// winit patch 4: on Windows, a hidden window is maximized only once shown.
#[test]
fn winit_maximizes_a_hidden_windows_window_only_when_it_is_shown() {
    let state = vendored("winit/src/platform_impl/windows/window_state.rs");
    assert!(state.contains("if new.contains(WindowFlags::MAXIMIZED) && diff.contains(WindowFlags::VISIBLE) {\n                    SW_MAXIMIZE"), "a hidden window shown maximized is shown, then maximized again");
    assert!(state.contains("&& new.contains(WindowFlags::VISIBLE)\n            && !diff.contains(WindowFlags::VISIBLE)\n        {"), "maximizing a hidden window shows it");
}

/// winit patch 5: on X11, a replaced input context is focused.
#[test]
fn winit_focuses_the_x11_input_context_it_replaces() {
    let ime = vendored("winit/src/platform_impl/linux/x11/ime/mod.rs");
    assert!(ime.contains("allowed: bool,\n    ) -> Result<bool, ImeContextCreationError> {"), "set_ime_allowed no longer says whether it replaced the context");
    let events = vendored("winit/src/platform_impl/linux/x11/event_processor.rs");
    assert!(events.contains("Ok(true) => ime_focus = Some(window_id),"), "a replaced input context is not focused");
    assert!(events.contains("let _ = ime.get_mut().focus(window_id);"), "a replaced input context is not focused");
}

/// winit patch 6: on macOS, compositions that end without marked text are
/// committed.
#[test]
fn winit_commits_macos_compositions_that_end_without_marked_text() {
    let view = vendored("winit/src/platform_impl/macos/view.rs");
    assert!(view.contains("if pending_commit && ime_enabled && !is_control {"), "insertText commits only while marked text is left");
    assert!(view.contains("self.ivars().pending_commit.set(true);"), "setMarkedText no longer starts a pending commit");
    assert!(view.contains("self.ivars().in_key_down.set(true);"), "a commit outside a key press swallows the next key again");
    assert!(view.contains("NSString::from_str(\"NSUnderline\")"), "validAttributesForMarkedText lost the underline attribute");
}

/// winit patch 7: on macOS, disallowing IME discards the composition.
#[test]
fn winit_discards_the_macos_composition_when_ime_is_disallowed() {
    let view = vendored("winit/src/platform_impl/macos/view.rs");
    assert!(section(&view, "pub(super) fn set_ime_allowed(", "\n    }\n").contains("input_context.discardMarkedText();"), "disallowing IME leaves the composition");
}

/// winit patch 8: on macOS, text inserted outside a key press (the emoji
/// picker) is committed.
#[test]
fn winit_commits_macos_text_inserted_outside_a_key_press() {
    let view = vendored("winit/src/platform_impl/macos/view.rs");
    assert!(view.contains("} else if !self.ivars().in_key_down.get()\n                && self.ivars().ime_allowed.get()"), "text inserted outside a key press is dropped again");
    assert!(!section(&view, "fn selected_range(&self) -> NSRange {", "\n        }\n").contains("marked_text"), "selectedRange reports a selection");
}

/// winit patch 9: on macOS, a key committed through IME is sent once.
#[test]
fn winit_sends_a_key_committed_through_macos_ime_only_once() {
    let view = vendored("winit/src/platform_impl/macos/view.rs");
    assert!(view.contains("self.ivars().committed_in_key_down.set(true);"), "a commit in keyDown isn't recorded");
    assert!(view.contains("let had_ime_input = self.ivars().committed_in_key_down.replace(false) || had_ime_input;"), "a key committed through IME is sent again as KeyboardInput");
}

/// winit patch 10: on macOS, Apple Korean's first key is interpreted again
/// once.
#[test]
fn winit_retries_the_first_apple_korean_key_once() {
    let policy = vendored("winit/src/platform_impl/macos/ime_startup.rs");
    assert!(policy.contains("source.starts_with(\"com.apple.inputmethod.Korean.\")"), "the retry is no longer limited to Apple Korean");
    let view = vendored("winit/src/platform_impl/macos/view.rs");
    assert!(view.contains("let retry = self.ivars().ime_startup.borrow_mut().finish(serial, same_context);"), "keyDown no longer retries the first key");
    assert!(view.contains("self.ivars().ime_startup.borrow_mut().insert(&string);"), "insertText no longer reports to the retry policy");
}

/// winit patch 11: on X11 without Xft.dpi, the scale comes from the X
/// screen's configured DPI before the panel's physical density.
#[test]
fn winit_takes_the_x11_scale_from_the_server_dpi() {
    let randr = vendored("winit/src/platform_impl/linux/x11/util/randr.rs");
    assert!(randr.contains("let screen = self.default_root();"));
    assert!(randr.contains("default_dpi_factor(\n                    self.get_xft_dpi(),"));
    assert!(randr.contains("screen.width_in_millimeters.into()"));
    assert!(randr.contains("calc_dpi_factor(screen_pixels, screen_mm)"));
}

// --- egui-wgpu -----------------------------------------------------------

/// egui-wgpu patch 1: a window larger than the GPU's texture limit gets a
/// surface within it, drawn at a matching scale, instead of a panic.
#[test]
fn egui_wgpu_fits_every_surface_it_sizes_within_the_gpu_limit() {
    let painter = vendored("egui-wgpu/src/winit.rs");
    for patch in [
        "let (width, height, render_scale) = surface_fit(window_width, window_height, max_side);",
        "let (fit_width, fit_height, _) = surface_fit(width, height, self.max_surface_side());",
        "pixels_per_point: pixels_per_point * surface_state.render_scale,",
        "old_state.window_width,\n            old_state.window_height,",
    ] {
        assert!(painter.contains(patch), "vendor/egui-wgpu lost its surface size patch: {patch}");
    }
}

/// egui-wgpu patch 2: a surface the device can't configure, or a renderer
/// it can't create, is returned as an error (eframe returns it from
/// `run_native`, and Quintess tries OpenGL), not a panic.
#[test]
fn egui_wgpu_returns_start_up_gpu_errors() {
    let painter = vendored("egui-wgpu/src/winit.rs");
    let add_surface = section(&painter, "async fn add_surface(", "\n    fn ");
    for patch in [
        "let installed = catch_errors(&device, || {\n            self.install_surface(surface, viewport_id, size.width, size.height, false);\n        })",
        "return Err(crate::WgpuError::ConfigureSurface(error));",
    ] {
        assert!(add_surface.contains(patch), "vendor/egui-wgpu lost its surface configure patch: {patch}");
    }
    let lib = vendored("egui-wgpu/src/lib.rs");
    assert!(lib.contains("winit::catch_errors(&device, || Renderer::new(&device, target_format, options))"), "creating the renderer panics on a GPU error again");
}

#[test]
fn a_window_larger_than_the_gpu_limit_gets_a_surface_within_it() {
    use eframe::egui_wgpu::winit::surface_fit;
    // 3440 x 1369 points restored at 250% asked an 8192-pixel device for this.
    let (width, height, scale) = surface_fit(8600, 3423, 8192);
    assert_eq!((width, height), (8192, 3260));
    // Both sides shrink alike and egui draws at that factor, so the whole
    // window is drawn.
    assert!((8600.0 * scale - 8192.0).abs() < 0.01, "{scale}");
    assert!((3423.0 * scale - height as f32).abs() < 1.0, "{scale}");
    // Within the limit nothing changes, up to and including the limit.
    assert_eq!(surface_fit(8600, 3423, 16384), (8600, 3423, 1.0));
    assert_eq!(surface_fit(8192, 8192, 8192), (8192, 8192, 1.0));
    assert_eq!(surface_fit(0, 0, 8192), (0, 0, 1.0));
    // One side over the limit: a window stretched across monitors.
    assert_eq!(surface_fit(8193, 600, 8192).0, 8192);
    assert_eq!(surface_fit(600, 8193, 8192).1, 8192);
}

#[test]
fn surface_fit_never_empties_or_overflows_a_surface() {
    use eframe::egui_wgpu::winit::surface_fit;
    for (width, height, max_side) in [(1, u32::MAX, 8192), (u32::MAX, 1, 2048), (u32::MAX, u32::MAX, 16384), (0, u32::MAX, 8192), (u32::MAX, 0, 8192), (5, 7, 0), (40_000, 3, 1)] {
        let (fit_width, fit_height, scale) = surface_fit(width, height, max_side);
        let case = format!("{width} x {height} in {max_side}: {fit_width} x {fit_height} at {scale}");
        assert!(fit_width <= max_side.max(1) && fit_height <= max_side.max(1), "{case}");
        // A side that wasn't zero stays non-zero (an empty surface can't be
        // configured), and a zero side stays zero (egui-wgpu skips it).
        assert_eq!((fit_width == 0, fit_height == 0), (width == 0, height == 0), "{case}");
        assert!(scale.is_finite() && scale > 0.0 && scale <= 1.0, "{case}");
    }
    // The longer side lands exactly on the limit, and the shorter one keeps
    // the window's proportions to within a pixel.
    for max_side in [2048, 8192, 16384] {
        for long in (max_side + 1..=max_side * 5).step_by(997).chain([max_side * 2, max_side * 4, u32::MAX]) {
            for short in [1, 3, 600, max_side / 3, max_side - 1, max_side, long - 1, long] {
                let (fit_long, fit_short, scale) = surface_fit(long, short, max_side);
                assert_eq!(fit_long, max_side, "{long} x {short} in {max_side}");
                let expected = f64::from(short) * f64::from(max_side) / f64::from(long);
                assert!((f64::from(fit_short) - expected).abs() <= 1.0, "{long} x {short} in {max_side}: {fit_short}");
                assert_eq!(surface_fit(short, long, max_side), (fit_short, fit_long, scale), "transposed");
            }
        }
    }
}
