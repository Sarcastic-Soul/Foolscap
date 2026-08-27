//! What the window knows about the document it is showing.
//!
//! Edits live here and are applied to the file only when the user saves, so
//! that nothing on disk changes until they ask for it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::worker::Edits;

#[derive(Debug, Default)]
pub struct State {
    pub path: Option<PathBuf>,
    /// Pages in the source document, before any edits.
    pub page_count: usize,
    /// Source page indices, in the order they are currently shown.
    pub order: Vec<usize>,
    /// Extra rotation per source page index.
    pub rotations: HashMap<usize, i32>,
    /// Selected positions in `order`, not source indices.
    pub selection: HashSet<usize>,
    /// The position shown in the main view.
    pub current: usize,
    pub dirty: bool,
}

impl State {
    pub fn open(&mut self, path: PathBuf, page_count: usize) {
        self.path = Some(path);
        self.page_count = page_count;
        self.order = (0..page_count).collect();
        self.rotations.clear();
        self.selection.clear();
        self.current = 0;
        self.dirty = false;
    }

    pub fn is_open(&self) -> bool {
        self.path.is_some()
    }

    pub fn visible_pages(&self) -> usize {
        self.order.len()
    }

    /// The source page shown at a position, if there is one.
    pub fn source_page(&self, position: usize) -> Option<usize> {
        self.order.get(position).copied()
    }

    /// Total rotation to draw a position with.
    pub fn rotation_at(&self, position: usize) -> i32 {
        self.source_page(position)
            .and_then(|page| self.rotations.get(&page))
            .copied()
            .unwrap_or(0)
    }

    /// Positions to act on: the selection, or the current page when nothing is
    /// selected. Acting on nothing when a page is plainly in view would read as
    /// the button being broken.
    pub fn target_positions(&self) -> Vec<usize> {
        if self.selection.is_empty() {
            if self.order.is_empty() {
                Vec::new()
            } else {
                vec![self.current.min(self.order.len() - 1)]
            }
        } else {
            let mut positions: Vec<usize> = self.selection.iter().copied().collect();
            positions.sort_unstable();
            positions
        }
    }

    pub fn rotate(&mut self, degrees: i32) {
        for position in self.target_positions() {
            if let Some(page) = self.source_page(position) {
                let entry = self.rotations.entry(page).or_insert(0);
                *entry = (*entry + degrees).rem_euclid(360);
            }
        }
        self.dirty = true;
    }

    /// Remove the targeted pages. Refuses to empty the document.
    pub fn delete(&mut self) -> Result<(), &'static str> {
        let doomed: HashSet<usize> = self.target_positions().into_iter().collect();

        if doomed.is_empty() {
            return Err("nothing is selected");
        }
        if doomed.len() >= self.order.len() {
            return Err("a document must keep at least one page");
        }

        let mut remaining = Vec::with_capacity(self.order.len() - doomed.len());
        for (position, page) in self.order.iter().enumerate() {
            if !doomed.contains(&position) {
                remaining.push(*page);
            }
        }

        self.order = remaining;
        self.selection.clear();
        self.current = self.current.min(self.order.len().saturating_sub(1));
        self.dirty = true;

        Ok(())
    }

    /// Move one page so that it sits at `destination`.
    pub fn move_page(&mut self, from: usize, destination: usize) {
        if from >= self.order.len() || from == destination {
            return;
        }

        let page = self.order.remove(from);
        let destination = destination.min(self.order.len());
        self.order.insert(destination, page);

        self.selection.clear();
        self.current = destination;
        self.dirty = true;
    }

    pub fn select_only(&mut self, position: usize) {
        self.selection.clear();
        self.selection.insert(position);
        self.current = position;
    }

    pub fn toggle_selection(&mut self, position: usize) {
        if !self.selection.remove(&position) {
            self.selection.insert(position);
        }
        self.current = position;
    }

    /// The pending edits, in the form the worker applies them.
    pub fn edits(&self) -> Edits {
        Edits {
            order: self.order.clone(),
            rotations: self.rotations.clone(),
        }
    }

    /// A sensible name to suggest when saving.
    pub fn suggested_name(&self, suffix: &str, extension: &str) -> String {
        let stem = self
            .path
            .as_ref()
            .and_then(|path| path.file_stem())
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document".to_string());

        format!("{stem}{suffix}.{extension}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(pages: usize) -> State {
        let mut state = State::default();
        state.open(PathBuf::from("/tmp/report.pdf"), pages);
        state
    }

    #[test]
    fn opening_shows_every_page_in_order() {
        let state = opened(4);
        assert_eq!(state.order, vec![0, 1, 2, 3]);
        assert!(!state.dirty);
        assert!(state.is_open());
    }

    #[test]
    fn rotation_accumulates_and_wraps() {
        let mut state = opened(2);
        state.select_only(0);

        state.rotate(270);
        assert_eq!(state.rotation_at(0), 270);

        state.rotate(180);
        assert_eq!(state.rotation_at(0), 90);
        assert_eq!(state.rotation_at(1), 0, "the other page is untouched");
    }

    #[test]
    fn with_nothing_selected_the_current_page_is_the_target() {
        let mut state = opened(3);
        state.current = 2;

        state.rotate(90);

        assert_eq!(state.rotation_at(2), 90);
        assert_eq!(state.rotation_at(0), 0);
    }

    #[test]
    fn rotation_follows_the_page_not_the_position() {
        let mut state = opened(3);
        state.select_only(0);
        state.rotate(90);

        state.move_page(0, 2);

        assert_eq!(state.order, vec![1, 2, 0]);
        assert_eq!(state.rotation_at(2), 90, "the rotated page moved with it");
        assert_eq!(state.rotation_at(0), 0);
    }

    #[test]
    fn deleting_removes_the_selection() {
        let mut state = opened(4);
        state.select_only(1);
        state.toggle_selection(3);

        state.delete().unwrap();

        assert_eq!(state.order, vec![0, 2]);
        assert!(state.dirty);
    }

    #[test]
    fn deleting_the_last_page_is_refused() {
        let mut state = opened(1);
        state.select_only(0);

        assert!(state.delete().is_err());
        assert_eq!(state.order.len(), 1);
    }

    #[test]
    fn deleting_everything_selected_is_refused() {
        let mut state = opened(3);
        for position in 0..3 {
            state.toggle_selection(position);
        }

        assert!(state.delete().is_err());
    }

    #[test]
    fn moving_a_page_forward_lands_where_asked() {
        let mut state = opened(4);
        state.move_page(0, 2);
        assert_eq!(state.order, vec![1, 2, 0, 3]);
    }

    #[test]
    fn moving_a_page_backward_lands_where_asked() {
        let mut state = opened(4);
        state.move_page(3, 0);
        assert_eq!(state.order, vec![3, 0, 1, 2]);
    }

    #[test]
    fn moving_beyond_the_end_clamps() {
        let mut state = opened(3);
        state.move_page(0, 99);
        assert_eq!(state.order, vec![1, 2, 0]);
    }

    #[test]
    fn moving_a_page_onto_itself_changes_nothing() {
        let mut state = opened(3);
        state.move_page(1, 1);
        assert_eq!(state.order, vec![0, 1, 2]);
        assert!(!state.dirty);
    }

    #[test]
    fn toggling_selects_then_deselects() {
        let mut state = opened(3);

        state.toggle_selection(1);
        assert!(state.selection.contains(&1));

        state.toggle_selection(1);
        assert!(state.selection.is_empty());
    }

    #[test]
    fn the_suggested_name_follows_the_original() {
        let state = opened(1);
        assert_eq!(
            state.suggested_name("-compressed", "pdf"),
            "report-compressed.pdf"
        );
    }

    #[test]
    fn an_unsaved_document_still_suggests_a_name() {
        let state = State::default();
        assert_eq!(state.suggested_name("", "pdf"), "document.pdf");
    }

    #[test]
    fn edits_describe_what_the_worker_must_apply() {
        let mut state = opened(3);
        state.select_only(0);
        state.rotate(90);
        state.move_page(0, 2);

        let edits = state.edits();
        assert_eq!(edits.order, vec![1, 2, 0]);
        assert_eq!(edits.rotations.get(&0), Some(&90));
        assert!(!edits.is_identity(3));
    }
}
