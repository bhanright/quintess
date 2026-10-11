//! A watched folder running in the background while the app is open: new
//! PDFs in it are run through an action (see `quintess_core::watch`). The
//! status bar shows its progress and a button to stop it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

use super::*;
use quintess_core::watch::{FolderWatcher, WatchEvent, WatchSettings};

/// How often the folder is looked at.
const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

pub(super) struct RunningWatch {
    stop: Arc<AtomicBool>,
    events: Receiver<WatchEvent>,
    inbox: PathBuf,
    output: PathBuf,
    processed: usize,
    failed: usize,
}

impl QuintessApp {
    pub(super) fn start_watch(&mut self, inbox: PathBuf, action_path: PathBuf, output: Option<PathBuf>, options: quintess_core::batch::RunOptions) {
        self.stop_watch();
        let started = quintess_core::batch::Action::load(&action_path).and_then(|action| FolderWatcher::new(WatchSettings { inbox: inbox.clone(), action, output, options }));
        let Some(mut watcher) = self.report(started) else { return };
        let output = watcher.output().to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, events) = std::sync::mpsc::channel();
        let (stop_flag, ctx) = (stop.clone(), self.worker_context.clone());
        std::thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                for event in watcher.check() {
                    if sender.send(event).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
                // Sleep in short steps so Stop takes effect quickly.
                let wake = std::time::Instant::now() + CHECK_INTERVAL;
                while std::time::Instant::now() < wake && !stop_flag.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        });
        self.info(format!("Watching {}: new PDFs there are processed into {}.", inbox.display(), output.display()));
        self.watch = Some(RunningWatch { stop, events, inbox, output, processed: 0, failed: 0 });
    }

    pub(super) fn stop_watch(&mut self) {
        if let Some(watch) = self.watch.take() {
            watch.stop.store(true, Ordering::Relaxed);
            self.info(format!("Stopped watching {} ({} processed, {} failed).", watch.inbox.display(), watch.processed, watch.failed));
        }
    }

    /// Takes in what the watcher has done since the last frame.
    pub(super) fn receive_watch_events(&mut self) {
        let Some(watch) = self.watch.as_mut() else { return };
        let mut messages = Vec::new();
        while let Ok(event) = watch.events.try_recv() {
            match event {
                WatchEvent::Processed { input, output } => {
                    watch.processed += 1;
                    messages.push((false, format!("Processed {} into {}.", file_name_of(&input), file_name_of(&output))));
                }
                WatchEvent::Failed { input, error } => {
                    watch.failed += 1;
                    messages.push((true, format!("Could not process {}: {error} (moved to the Failed folder).", file_name_of(&input))));
                }
            }
        }
        for (is_error, message) in messages {
            if is_error {
                self.error(message);
            } else {
                self.info(message);
            }
        }
    }

    /// The watch's line in the status bar.
    pub(super) fn watch_status(&mut self, ui: &mut egui::Ui) {
        let Some(watch) = self.watch.as_ref() else { return };
        let mut stop = false;
        ui.label(format!("👁 Watching {}: {} done{}", file_name_of(&watch.inbox), watch.processed, if watch.failed > 0 { format!(", {} failed", watch.failed) } else { String::new() }))
            .on_hover_text(format!("New PDFs in {} go into {}", watch.inbox.display(), watch.output.display()));
        if ui.small_button("Stop").clicked() {
            stop = true;
        }
        ui.separator();
        if stop {
            self.stop_watch();
        }
    }
}

fn file_name_of(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}
