//! Split view: the document in two panes side by side, each with its own
//! page, zoom and scroll position, for reading one part while looking at
//! another.
//!
//! The tab's own view fields (zoom, fit, current page) and its viewer state
//! always belong to the active pane, the one last clicked, so every command
//! that zooms or turns pages acts on it. The other pane's view is kept here
//! and swapped in only while that pane is drawn.

use super::*;

/// The view of the pane that isn't active.
pub(super) struct OtherPane {
    zoom: f32,
    fit: FitMode,
    current_page: usize,
    scroll_to_page: Option<usize>,
    state: ViewerState,
}

impl OtherPane {
    /// A second view starting where the tab is.
    fn like(tab: &DocumentTab) -> Self {
        Self { zoom: tab.zoom, fit: tab.fit, current_page: tab.current_page, scroll_to_page: Some(tab.current_page), state: ViewerState::default() }
    }

    /// Exchanges this view with the tab's.
    fn swap_with(&mut self, tab: &mut DocumentTab, state: &mut ViewerState) {
        std::mem::swap(&mut self.zoom, &mut tab.zoom);
        std::mem::swap(&mut self.fit, &mut tab.fit);
        std::mem::swap(&mut self.current_page, &mut tab.current_page);
        std::mem::swap(&mut self.scroll_to_page, &mut tab.scroll_to_page);
        std::mem::swap(&mut self.state, state);
    }

    pub(super) fn current_page(&self) -> usize {
        self.current_page
    }
}

impl QuintessApp {
    /// Turns the other pane to a page (for the app's self-check pictures).
    pub(super) fn set_other_pane_page(&mut self, page: usize) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let pane = self.other_panes.entry(tab.id).or_insert_with(|| OtherPane::like(tab));
        pane.current_page = page;
        pane.scroll_to_page = Some(page);
    }

    pub(super) fn toggle_split_view(&mut self) {
        self.split_view = !self.split_view;
        if !self.split_view {
            self.other_panes.clear();
            self.right_pane_active = false;
        }
    }

    /// Draws the active tab in two panes and returns their edits.
    pub(super) fn split_viewer(&mut self, ui: &mut egui::Ui) -> Vec<ViewerAction> {
        let tab = &mut self.tabs[self.active];
        let state = self.viewer_states.entry(tab.id).or_default();
        let other = self.other_panes.entry(tab.id).or_insert_with(|| OtherPane::like(tab));
        let right_active = self.right_pane_active;
        let mut actions = Vec::new();
        let mut pane_rects = [egui::Rect::NOTHING; 2];
        ui.columns(2, |columns| {
            for (pane, column) in columns.iter_mut().enumerate() {
                let active = (pane == 1) == right_active;
                if !active {
                    other.swap_with(tab, state);
                }
                pane_rects[pane] = column.max_rect();
                actions.extend(viewer::show(column, tab, state, self.tool, &self.tool_settings, &self.worker, self.night_mode, pane));
                if active {
                    // A thin outline marks the pane that commands act on.
                    column.painter().rect_stroke(column.max_rect().shrink(1.0), 0.0, egui::Stroke::new(1.5, crate::theme::highlight_alpha(140)), egui::StrokeKind::Inside);
                } else {
                    other.swap_with(tab, state);
                }
            }
        });
        // Clicking in the other pane makes it the active one.
        let pressed_at = ui.input(|i| if i.pointer.any_pressed() { i.pointer.interact_pos() } else { None });
        let inactive = if right_active { 0 } else { 1 };
        if pressed_at.is_some_and(|p| pane_rects[inactive].contains(p)) {
            other.swap_with(tab, state);
            self.right_pane_active = !right_active;
        }
        actions
    }
}
