//! Requests the desktop makes of the app outside its window.
//!
//! On macOS, documents opened from the Finder (double-click, Open With, a
//! drop on the Dock icon, `open -a Quintess file.pdf`) don't arrive as
//! command-line arguments: Launch Services sends the app an "open
//! documents" Apple event, and the Dock's Quit sends a "quit" one. winit,
//! under eframe, handles neither, so without this the Mac would answer
//! "Quintess cannot open files in the PDF format". The `fmv-macos-events`
//! crate (MIT OR Apache-2.0) registers handlers for both before the launch
//! event arrives, without replacing winit's application delegate; the idea
//! and its use here follow PdfCraft's `apps/pdfcraft/src/apple_events.rs`
//! (github.com/storytold/pdfcraft at 6d843a8e, MIT OR Apache-2.0,
//! Copyright (c) 2026 ArtCraft Team and the PdfCraft contributors),
//! modified for Quintess.
//!
//! Also on macOS: the application menu's Quit (Cmd+Q) is pointed at the
//! window's close button, so it asks about unsaved changes like closing
//! the window does, instead of ending the program at once; and an Edit menu
//! lets Cmd+V paste into the native file dialogs' name fields.
//!
//! On Windows and Linux files come as arguments, so this does nothing there.

use std::path::PathBuf;

/// What the desktop asked for. Only macOS sends these.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum DesktopRequest {
    /// Open these files, as if they had been dropped on the window.
    Open(Vec<PathBuf>),
    /// Quit, asking about unsaved changes first.
    Quit,
}

/// Keeps the handlers registered; hold it until the window has closed.
pub struct DesktopEvents {
    #[cfg(target_os = "macos")]
    _registration: fmv_macos_events::Registration,
    inbox: DesktopInbox,
}

/// The queue of requests, which the app empties each frame.
#[derive(Clone)]
pub struct DesktopInbox {
    #[cfg(target_os = "macos")]
    inbox: fmv_macos_events::Inbox,
}

impl DesktopEvents {
    /// Registers the handlers. Called on the main thread before the window
    /// opens, so the event that launched the app is caught too.
    pub fn install() -> Self {
        #[cfg(target_os = "macos")]
        {
            let (registration, inbox) = fmv_macos_events::Registration::install();
            DesktopEvents { _registration: registration, inbox: DesktopInbox { inbox } }
        }
        #[cfg(not(target_os = "macos"))]
        DesktopEvents { inbox: DesktopInbox {} }
    }

    pub fn inbox(&self) -> DesktopInbox {
        self.inbox.clone()
    }
}

impl DesktopInbox {
    /// Once the window exists: requests arriving later wake the app (it
    /// draws only when something changes), and the menus are adjusted.
    pub fn connect(&self, ctx: &egui::Context) {
        #[cfg(target_os = "macos")]
        {
            let ctx = ctx.clone();
            self.inbox.set_wake(move || ctx.request_repaint());
            mac_menus::route_quit_through_the_window();
            fmv_macos_events::install_edit_menu();
        }
        #[cfg(not(target_os = "macos"))]
        let _ = ctx;
    }

    /// The requests that arrived since the last call.
    pub fn take(&self) -> Vec<DesktopRequest> {
        #[cfg(target_os = "macos")]
        {
            self.inbox
                .drain()
                .into_iter()
                .map(|event| match event {
                    fmv_macos_events::Event::Open(paths) => DesktopRequest::Open(paths),
                    fmv_macos_events::Event::Quit => DesktopRequest::Quit,
                })
                .collect()
        }
        #[cfg(not(target_os = "macos"))]
        Vec::new()
    }
}

#[cfg(target_os = "macos")]
mod mac_menus {
    use objc2::sel;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::MainThreadMarker;

    /// winit's application menu ends the program on Quit (`terminate:`),
    /// skipping the question about unsaved changes. Sending the window
    /// `performClose:` instead (through the responder chain, to whichever
    /// window is in front) makes Quit and Cmd+Q a click on the close
    /// button, which the app answers like any close request; closing its
    /// only window ends the app.
    pub fn route_quit_through_the_window() {
        let Some(main_thread) = MainThreadMarker::new() else { return };
        let application = NSApplication::sharedApplication(main_thread);
        let Some(main_menu) = application.mainMenu() else { return };
        // SAFETY: on the main thread (checked above); the menu items are
        // AppKit's own, and `performClose:` takes the sender like
        // `terminate:` does, so only the selector and its target change.
        unsafe {
            for menu_index in 0..main_menu.numberOfItems() {
                let Some(submenu) = main_menu.itemAtIndex(menu_index).and_then(|item| item.submenu()) else { continue };
                for item_index in 0..submenu.numberOfItems() {
                    let Some(item) = submenu.itemAtIndex(item_index) else { continue };
                    if item.action() == Some(sel!(terminate:)) {
                        item.setAction(Some(sel!(performClose:)));
                        item.setTarget(None);
                    }
                }
            }
        }
    }
}
