//! Recording an action: while recording, each page, stamp, redaction,
//! cleanup and conversion step the person applies is noted as a batch step,
//! to be saved and run on other files (Tools ▸ Run an action on files).
//! Text edits, comments, signing and form filling depend on the particular
//! document, so they are not recorded.

use super::*;
use vellum_core::batch::{Action, Step};

impl VellumApp {
    /// Notes a step when recording.
    pub(super) fn record(&mut self, step: Step) {
        if let Some(steps) = self.recording.as_mut() {
            steps.push(step);
        }
    }

    /// Makes an edit and, when it succeeds, records the step it stands for.
    pub(super) fn edit_recording(&mut self, label: &str, step: Step, change: impl FnOnce(&mut Pdf) -> anyhow::Result<()>) {
        if self.try_edit(label, change) {
            self.record(step);
        }
    }

    pub(super) fn toggle_recording(&mut self) {
        if self.recording.is_some() {
            self.stop_recording(true);
        } else {
            self.recording = Some(Vec::new());
            self.info("Recording an action: apply the steps you want, then press Stop and save.");
        }
    }

    /// Ends recording; with `save`, asks where to keep the action and opens
    /// the dialog that runs it on files.
    fn stop_recording(&mut self, save: bool) {
        let Some(steps) = self.recording.take() else { return };
        if !save {
            self.info("Recording discarded.");
            return;
        }
        if steps.is_empty() {
            self.info("Nothing was recorded, so there is no action to save.");
            return;
        }
        let Some(path) = rfd::FileDialog::new().add_filter("Action", &["json"]).set_file_name("My action.json").save_file() else {
            // Keep the steps if the person backed out of choosing a file.
            self.recording = Some(steps);
            return;
        };
        let descriptions: Vec<String> = steps.iter().map(Step::describe).collect();
        let result = Action::new(steps).save(&path);
        if self.report(result).is_some() {
            self.info(format!("Saved the action to {}. Choose files to run it on.", path.display()));
            self.dialogs.push(Dialog::Batch { action: Some(path), inputs: Vec::new(), steps: descriptions });
        }
    }

    pub(super) fn recording_bar(&mut self, ui: &mut egui::Ui) {
        let Some(steps) = self.recording.as_ref() else { return };
        let listed: Vec<String> = steps.iter().map(Step::describe).collect();
        let mut stop = None;
        let mut drop_last = false;
        ui.horizontal(|ui| {
            ui.colored_label(Color32::from_rgb(210, 40, 40), RichText::new("● Recording an action").strong());
            if listed.is_empty() {
                ui.weak("Apply page, stamp, redaction, cleanup or conversion steps. Text edits and comments aren't recorded.");
            } else {
                ui.label(format!("{} step{}: {}", listed.len(), if listed.len() == 1 { "" } else { "s" }, listed.join(" → ")));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Discard").clicked() {
                    stop = Some(false);
                }
                if ui.button(RichText::new("Stop and save…").strong()).clicked() {
                    stop = Some(true);
                }
                if !listed.is_empty() && ui.button("Forget last step").on_hover_text("Leave the last step out of the recording (the document keeps it)").clicked() {
                    drop_last = true;
                }
            });
        });
        if drop_last {
            if let Some(steps) = self.recording.as_mut() {
                steps.pop();
            }
        }
        if let Some(save) = stop {
            self.stop_recording(save);
        }
    }
}
