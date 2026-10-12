//! Opening the window: which renderer draws it, and what happens when that
//! renderer can't start.
//!
//! OpenGL ("glow") is the default: measured on a one-page document it needs
//! about half the memory of wgpu (about 200 MB against 400 MB), which mostly
//! goes to setting up a Vulkan or DirectX device. The Graphics preference (or
//! QUINTESS_RENDERER=wgpu) chooses wgpu instead. Whichever is tried first, if
//! it fails before the app is created (while the renderer starts), the window
//! is opened again with the other one and the status line says so. A failure
//! after the app was created is reported, not retried: the user was already
//! working, and reopening the window would surprise them.

use std::cell::Cell;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::rc::Rc;

use eframe::Renderer;

/// Why the window couldn't be opened, or closed with an error.
#[derive(Debug)]
pub enum WindowFailure {
    /// eframe returned an error.
    Error(eframe::Error),
    /// The renderer panicked while it started, before the app was created.
    /// eframe has such panics on its start-up path (an OpenGL driver that
    /// offers no pixel format, for example); they are caught so the other
    /// renderer can be tried.
    PanicWhileStarting(String),
}

impl fmt::Display for WindowFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WindowFailure::Error(error) => write!(formatter, "{error}"),
            WindowFailure::PanicWhileStarting(message) => write!(formatter, "the renderer stopped while starting: {message}"),
        }
    }
}

/// Opens the window with these files and returns when it closes.
pub fn run_window(files: Vec<PathBuf>) -> Result<(), WindowFailure> {
    let mut viewport = egui::ViewportBuilder::default().with_title("Quintess").with_app_id("quintess").with_inner_size([1280.0, 860.0]).with_min_inner_size([640.0, 420.0]).with_drag_and_drop(true);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/quintess-256.png")) {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let preference = quintess_core::settings::Settings::load().graphics;
    let requested = std::env::var("QUINTESS_RENDERER").unwrap_or(preference);
    let first_renderer = first_renderer(&requested);
    // A developer aid for testing the fallback: QUINTESS_FAIL_RENDERER=glow
    // or =wgpu makes that renderer fail to start, through a setting no
    // computer can satisfy.
    let broken = std::env::var("QUINTESS_FAIL_RENDERER").unwrap_or_default();
    let options = |renderer| native_options(renderer, viewport.clone(), &broken);

    // Set when eframe creates the app, which it does only once the renderer
    // has started.
    let app_started = Rc::new(Cell::new(false));
    // Before the window opens, so a document the Mac's Finder launched the
    // app with is caught (see desktop.rs).
    let desktop_events = crate::desktop::DesktopEvents::install();
    let first_result = run_once(options(first_renderer), &files, &app_started, desktop_events.inbox(), None);
    let failure = match first_result {
        Err(failure) if should_try_other_renderer(&failure, app_started.get()) => failure,
        other => return other,
    };
    let second_renderer = other_renderer(first_renderer);
    eprintln!("{} couldn't start ({failure}); using {} instead", renderer_name(first_renderer), renderer_name(second_renderer));
    let note = format!("{} couldn't start on this computer, so Quintess is drawing with {} instead.", renderer_name(first_renderer), renderer_name(second_renderer));
    run_once(options(second_renderer), &files, &app_started, desktop_events.inbox(), Some(note))
}

/// The renderer to try first, from the Graphics preference or QUINTESS_RENDERER.
fn first_renderer(requested: &str) -> Renderer {
    if requested.trim().eq_ignore_ascii_case("wgpu") { Renderer::Wgpu } else { Renderer::Glow }
}

fn other_renderer(renderer: Renderer) -> Renderer {
    match renderer {
        Renderer::Glow => Renderer::Wgpu,
        Renderer::Wgpu => Renderer::Glow,
    }
}

/// The renderer as the Graphics preference names it.
fn renderer_name(renderer: Renderer) -> &'static str {
    match renderer {
        Renderer::Glow => "OpenGL",
        Renderer::Wgpu if cfg!(target_os = "macos") => "Metal",
        Renderer::Wgpu => "DirectX / Vulkan",
    }
}

/// Whether a failed run is worth repeating with the other renderer: only
/// when it failed before the app was created, so while the renderer started,
/// and not because there is no event loop at all (no display to draw on,
/// which the other renderer can't fix either).
fn should_try_other_renderer(failure: &WindowFailure, app_started: bool) -> bool {
    if app_started {
        return false;
    }
    !matches!(failure, WindowFailure::Error(eframe::Error::WinitEventLoop(_)))
}

/// The window and renderer settings for one run.
fn native_options(renderer: Renderer, viewport: egui::ViewportBuilder, broken: &str) -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions { renderer, viewport, ..Default::default() };
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        // egui-wgpu asks the device for 8192-pixel textures, whatever the
        // GPU offers. A window wider than that (restored at another
        // monitor's scale, or stretched across monitors) then gets a
        // surface shrunk to fit (vendor/egui-wgpu patch 1), drawn softer;
        // and a GPU limited to 4096 refuses the device outright. Asking for
        // what the GPU actually offers avoids both; the other requirements
        // stay egui-wgpu's own.
        let device_descriptor = std::sync::Arc::clone(&setup.device_descriptor);
        setup.device_descriptor = std::sync::Arc::new(move |adapter| {
            let mut descriptor = device_descriptor(adapter);
            descriptor.required_limits = surface_texture_limits(descriptor.required_limits, &adapter.limits());
            descriptor
        });
        if renderer == Renderer::Wgpu && broken == "wgpu" {
            // No graphics back end at all: no adapter can be found, and
            // eframe returns an error.
            setup.instance_descriptor.backends = eframe::wgpu::Backends::empty();
        }
    }
    if renderer == Renderer::Glow && broken == "glow" {
        // No OpenGL driver offers a 255-bit depth buffer. eframe panics when
        // the driver offers no matching pixel format, as a driver missing
        // what eframe asks for would, so this tests the panic path.
        options.depth_buffer = u8::MAX;
    }
    options
}

/// The device limits to ask for: egui-wgpu's own, with the largest texture
/// side the GPU supports (the window's surface is a texture of that kind).
fn surface_texture_limits(mut required: eframe::wgpu::Limits, supported: &eframe::wgpu::Limits) -> eframe::wgpu::Limits {
    required.max_texture_dimension_2d = supported.max_texture_dimension_2d;
    required
}

/// Runs the window once. A panic before the app was created is returned as
/// `PanicWhileStarting`; a panic after it carries on unwinding, as before
/// (the crash handler has already saved the user's work and said so).
fn run_once(options: eframe::NativeOptions, files: &[PathBuf], app_started: &Rc<Cell<bool>>, desktop: crate::desktop::DesktopInbox, note: Option<String>) -> Result<(), WindowFailure> {
    let files = files.to_vec();
    let started = Rc::clone(app_started);
    let creator: eframe::AppCreator<'_> = Box::new(move |cc| {
        started.set(true);
        crate::crash::renderer_started();
        let mut app = crate::app::QuintessApp::new(cc, files, desktop);
        if let Some(note) = note {
            app.startup_note(note);
        }
        Ok(Box::new(app))
    });
    crate::crash::renderer_starting();
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| eframe::run_native("Quintess", options, creator)));
    crate::crash::renderer_started();
    match outcome {
        Ok(result) => result.map_err(WindowFailure::Error),
        Err(payload) if !app_started.get() => Err(WindowFailure::PanicWhileStarting(panic_message(payload.as_ref()))),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        text.to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "(no message)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wgpu_is_chosen_only_by_name_and_anything_else_means_opengl() {
        assert_eq!(first_renderer("wgpu"), Renderer::Wgpu);
        assert_eq!(first_renderer(" WGPU "), Renderer::Wgpu);
        assert_eq!(first_renderer("automatic"), Renderer::Glow);
        assert_eq!(first_renderer(""), Renderer::Glow);
        assert_eq!(first_renderer("vulkan"), Renderer::Glow);
    }

    #[test]
    fn each_renderer_falls_back_to_the_other() {
        assert_eq!(other_renderer(Renderer::Glow), Renderer::Wgpu);
        assert_eq!(other_renderer(Renderer::Wgpu), Renderer::Glow);
    }

    #[test]
    fn only_a_failure_before_the_app_started_is_retried() {
        let panic = || WindowFailure::PanicWhileStarting("no pixel format".into());
        assert!(should_try_other_renderer(&panic(), false));
        assert!(!should_try_other_renderer(&panic(), true));
        let no_surface_format = || WindowFailure::Error(eframe::Error::Wgpu(eframe::egui_wgpu::WgpuError::NoSurfaceFormatsAvailable));
        assert!(should_try_other_renderer(&no_surface_format(), false));
        assert!(!should_try_other_renderer(&no_surface_format(), true));
    }

    #[test]
    fn the_device_asks_for_the_largest_texture_the_gpu_supports() {
        let required = eframe::wgpu::Limits { max_texture_dimension_2d: 8192, ..eframe::wgpu::Limits::downlevel_webgl2_defaults() };
        for supported_side in [4096, 8192, 16384] {
            let supported = eframe::wgpu::Limits { max_texture_dimension_2d: supported_side, ..eframe::wgpu::Limits::default() };
            let asked = surface_texture_limits(required.clone(), &supported);
            assert_eq!(asked.max_texture_dimension_2d, supported_side);
            // Everything else stays as egui-wgpu asked.
            assert_eq!(eframe::wgpu::Limits { max_texture_dimension_2d: 8192, ..asked }, required);
        }
    }

    #[test]
    fn the_failure_aids_only_change_the_renderer_they_name() {
        let viewport = egui::ViewportBuilder::default;
        let normal = native_options(Renderer::Glow, viewport(), "");
        assert_eq!(normal.depth_buffer, 0);
        assert_eq!(native_options(Renderer::Glow, viewport(), "glow").depth_buffer, u8::MAX);
        // The renderer tried next starts normally.
        assert_eq!(native_options(Renderer::Wgpu, viewport(), "glow").depth_buffer, 0);
        let backends = |options: &eframe::NativeOptions| match &options.wgpu_options.wgpu_setup {
            eframe::egui_wgpu::WgpuSetup::CreateNew(setup) => setup.instance_descriptor.backends,
            eframe::egui_wgpu::WgpuSetup::Existing(_) => panic!("Quintess lets egui-wgpu create its own device"),
        };
        assert!(backends(&native_options(Renderer::Wgpu, viewport(), "wgpu")).is_empty());
        assert!(!backends(&normal).is_empty());
        assert!(!backends(&native_options(Renderer::Glow, viewport(), "wgpu")).is_empty());
    }

    /// A real device made the way the app makes one (Quintess's own device
    /// limits), or None on a computer without any GPU adapter. CI runners
    /// usually have a software one (WARP, llvmpipe).
    fn test_device() -> Option<(eframe::wgpu::Instance, eframe::wgpu::Adapter, eframe::wgpu::Device, eframe::wgpu::Queue)> {
        let options = native_options(Renderer::Wgpu, egui::ViewportBuilder::default(), "");
        let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &options.wgpu_options.wgpu_setup else { panic!("Quintess lets egui-wgpu create its own device") };
        let instance = pollster::block_on(options.wgpu_options.wgpu_setup.new_instance());
        let adapter_options = eframe::wgpu::RequestAdapterOptions { power_preference: setup.power_preference, ..Default::default() };
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&adapter_options)) else {
            eprintln!("skipping: no GPU adapter on this computer");
            return None;
        };
        let Ok((device, queue)) = pollster::block_on(adapter.request_device(&(setup.device_descriptor)(&adapter))) else {
            eprintln!("skipping: the GPU adapter gives no device");
            return None;
        };
        // The app's device asks for the largest texture the GPU supports.
        assert_eq!(device.limits().max_texture_dimension_2d, adapter.limits().max_texture_dimension_2d);
        Some((instance, adapter, device, queue))
    }

    /// vendor/egui-wgpu patch 2: an error the GPU raises while a window is
    /// set up is returned, not handed to wgpu's default handler, which
    /// panics. The error here is one wgpu raises before anything reaches the
    /// driver: a texture one pixel wider than the device allows.
    #[test]
    fn a_gpu_error_while_a_window_is_set_up_is_returned_not_a_panic() {
        use eframe::egui_wgpu::winit::catch_errors;
        use eframe::wgpu;
        let Some((_instance, _adapter, device, _queue)) = test_device() else { return };
        let texture = |width: u32| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wider than the device allows"),
                size: wgpu::Extent3d { width, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let too_wide = device.limits().max_texture_dimension_2d.saturating_add(1);
        let caught = pollster::block_on(catch_errors(&device, || texture(too_wide)));
        assert!(matches!(caught, Err(wgpu::Error::Validation { .. })), "{caught:?}");
        // The device stays usable, and nothing is left behind to catch later
        // errors by mistake.
        assert!(pollster::block_on(catch_errors(&device, || texture(16))).is_ok());
        assert_eq!(pollster::block_on(catch_errors(&device, || 7)).ok(), Some(7));
    }

    /// vendor/egui-wgpu patch 2: a renderer the device rejects (here, an
    /// invalid three-sample pipeline) is returned as an error, and the
    /// device can still make a valid one afterwards.
    #[test]
    fn a_renderer_the_gpu_rejects_is_returned_and_the_device_stays_usable() {
        use eframe::egui_wgpu;
        let Some((instance, adapter, device, queue)) = test_device() else { return };
        let mut options = native_options(Renderer::Wgpu, egui::ViewportBuilder::default(), "");
        options.wgpu_options.wgpu_setup = egui_wgpu::WgpuSetup::Existing(egui_wgpu::WgpuSetupExisting { instance: instance.clone(), adapter, device, queue });
        for samples in [3, 1, 3, 1] {
            let renderer_options = egui_wgpu::RendererOptions { msaa_samples: samples, ..Default::default() };
            let result = pollster::block_on(egui_wgpu::RenderState::create(&options.wgpu_options, &instance, None, renderer_options));
            if samples == 3 {
                let error = result.err().expect("an invalid pipeline must be returned as an error");
                assert!(error.to_string().contains("egui_pipeline"), "{error}");
            } else {
                assert!(result.is_ok(), "the device must stay usable: {:?}", result.err());
            }
        }
    }
}
