//! Checking for a newer Quintess: asked about once on first run (opt in),
//! then at most daily if allowed, or any time from Help ▸ Check for updates.
//! A newer release can be downloaded and its installer started.

use egui::RichText;
use quintess_core::updates::{self, Release};

use super::*;

/// The update window's state.
pub(super) enum UpdateWindow {
    /// First run: should Quintess check automatically?
    AskPreference,
    Available(Release),
    /// The installer is downloaded and ready to run.
    Ready { path: PathBuf, verified: bool, version: String },
}

/// How long between automatic checks.
const CHECK_INTERVAL: chrono::TimeDelta = chrono::TimeDelta::hours(24);

impl QuintessApp {
    /// At startup: asks about automatic checks once, or checks if it's time.
    pub(super) fn startup_update_check(&mut self, ctx: &egui::Context) {
        if self.update_startup_done || self.self_check.is_some() {
            return;
        }
        self.update_startup_done = true;
        match self.settings.check_for_updates {
            None => self.update_window = Some(UpdateWindow::AskPreference),
            Some(true) => {
                let due = self.settings.last_update_check.as_deref().and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok()).is_none_or(|last| chrono::Local::now().fixed_offset() - last > CHECK_INTERVAL);
                if due {
                    self.check_for_updates(ctx, false);
                }
            }
            Some(false) => {}
        }
    }

    /// Asks GitHub for the latest release in the background. A manual check
    /// always reports its result; an automatic one speaks only when there is
    /// something new (and not a version the person chose to skip).
    pub(super) fn check_for_updates(&mut self, ctx: &egui::Context, manual: bool) {
        self.start_job(ctx, "Checking for updates", move |_| match updates::newer_release() {
            Ok(found) => Ok(JobOutcome::UpdateChecked { found, manual, reached: true }),
            Err(error) if manual => Err(error),
            // An automatic check fails quietly, and is tried again at the next start.
            Err(_) => Ok(JobOutcome::UpdateChecked { found: None, manual: false, reached: false }),
        });
    }

    /// `reached`: GitHub answered. Only then does the next automatic check
    /// wait a day; a failed one is tried again the next time Quintess starts.
    pub(super) fn update_checked(&mut self, found: Option<Release>, manual: bool, reached: bool) {
        if reached {
            self.settings.last_update_check = Some(chrono::Local::now().to_rfc3339());
            let _ = self.settings.save();
        }
        match found {
            Some(release) if manual || self.settings.skipped_version.as_deref() != Some(release.version.as_str()) => self.update_window = Some(UpdateWindow::Available(release)),
            Some(_) => {}
            None if manual => self.info(format!("Quintess {} is the latest version.", updates::CURRENT_VERSION)),
            None => {}
        }
    }

    fn download_update(&mut self, ctx: &egui::Context, release: Release) {
        let version = release.version.clone();
        self.start_job(ctx, "Downloading the update", move |progress| {
            let folder = std::env::temp_dir().join("Quintess update");
            let (path, verified) = updates::download_installer(&release, &folder, &|done, total| progress((done / 1024) as u32, (total / 1024).max(1) as u32))?;
            Ok(JobOutcome::UpdateDownloaded { path, verified, version })
        });
    }

    pub(super) fn show_update_window(&mut self, ctx: &egui::Context) {
        let Some(window) = self.update_window.take() else { return };
        let colors = crate::theme::palette(ctx);
        let mut keep = true;
        let mut next: Option<UpdateWindow> = None;
        let title = match &window {
            UpdateWindow::AskPreference => "Check for updates?",
            UpdateWindow::Available(_) => "A new version of Quintess",
            UpdateWindow::Ready { .. } => "Ready to install",
        };
        egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).default_width(440.0).show(ctx, |ui| match &window {
            UpdateWindow::AskPreference => {
                ui.label("Should Quintess check for new versions automatically?");
                ui.label(RichText::new("It would ask GitHub once a day whether a newer release exists. Nothing about you or your documents is sent, and you can change this in Preferences or check by hand from the Help menu.").color(colors.text_muted));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if crate::theme::primary_button(ui, "Yes, check daily").clicked() {
                        self.settings.check_for_updates = Some(true);
                        let _ = self.settings.save();
                        self.check_for_updates(ui.ctx(), false);
                        keep = false;
                    }
                    if ui.button("No thanks").clicked() {
                        self.settings.check_for_updates = Some(false);
                        let _ = self.settings.save();
                        keep = false;
                    }
                });
            }
            UpdateWindow::Available(release) => {
                ui.label(RichText::new(format!("Quintess {} is available", release.version)).size(18.0).strong());
                ui.label(RichText::new(format!("You have {}.", updates::CURRENT_VERSION)).color(colors.text_muted));
                if !release.notes.trim().is_empty() {
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        ui.label(RichText::new(release.notes.trim()).color(colors.text_muted));
                    });
                }
                if cfg!(not(windows)) {
                    // Linux installs come from a package; Quintess doesn't replace itself.
                    ui.add_space(6.0);
                    use quintess_core::system::{linux_package, LinuxPackage};
                    let advice = match linux_package() {
                        LinuxPackage::Flatpak => "Download the new .flatpak file from the release page and install it with flatpak install --user, as before.",
                        LinuxPackage::AppImage => "Download the new .AppImage from the release page, make it executable and use it in place of this one.",
                        LinuxPackage::Installed => "Download it from the release page: the .deb if you installed with apt or dpkg, the .tar.gz if you used install.sh.",
                    };
                    ui.label(RichText::new(advice).color(colors.text_muted));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if release.installer.is_some() && crate::theme::primary_button(ui, "Download and install").clicked() {
                        self.download_update(ui.ctx(), release.clone());
                        keep = false;
                    }
                    if ui.button("Release page").clicked() && release.page.starts_with("https://github.com/") {
                        let _ = quintess_core::system::open_in_default_app(&release.page);
                    }
                    if ui.button("Skip this version").clicked() {
                        self.settings.skipped_version = Some(release.version.clone());
                        let _ = self.settings.save();
                        keep = false;
                    }
                    if ui.button("Later").clicked() {
                        keep = false;
                    }
                });
            }
            UpdateWindow::Ready { path, verified, version } => {
                ui.label(format!("Quintess {version} is downloaded."));
                if *verified {
                    ui.label(RichText::new("✔ It matches the checksum published with the release.").color(colors.success));
                } else {
                    ui.label(RichText::new("The release didn't publish a checksum, so the download couldn't be verified beyond coming from GitHub.").color(colors.warning));
                }
                let unsaved = self.tabs.iter().filter(|t| t.dirty).count();
                if unsaved > 0 {
                    ui.label(RichText::new(format!("Save your work first: {unsaved} document{} unsaved changes.", if unsaved == 1 { " has" } else { "s have" })).color(colors.warning));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if crate::theme::primary_button(ui, "Install now (Quintess closes)").clicked() {
                        match std::process::Command::new(path).spawn() {
                            Ok(_) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
                            Err(error) => next = Some(UpdateWindow::Ready { path: path.clone(), verified: *verified, version: version.clone() }).inspect(|_| {
                                self.error(format!("Could not start the installer: {error}"));
                            }),
                        }
                        keep = false;
                    }
                    if ui.button("Show the file").clicked() {
                        let _ = quintess_core::system::reveal_in_file_manager(path);
                    }
                    if ui.button("Later").clicked() {
                        keep = false;
                    }
                });
            }
        });
        if keep {
            self.update_window = Some(window);
        } else if let Some(next) = next {
            self.update_window = Some(next);
        }
    }
}
