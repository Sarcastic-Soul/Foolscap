//! What the window knows about the document it is showing.
//!
//! Edits live here and are applied to the file only when the user saves, so
//! that nothing on disk changes until they ask for it. Because the edits are a
//! plain value, undo is a stack of earlier values.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::worker::Edits;

/// How many steps of history to keep. Each step is a page order and a few
/// rotations, so this is generous without costing much memory.
const HISTORY_LIMIT: usize = 200;

/// Everything undo has to put back.
///
/// The document itself is part of it, not just the edits, because dropping
/// several files merges them into a new document, and undoing that has to
/// return to the one that was open before.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub source: Option<PathBuf>,
    pub path: Option<PathBuf>,
    pub page_count: usize,
    order: Vec<usize>,
    rotations: HashMap<usize, i32>,
    current: usize,
    saved: Edits,
}

#[derive(Debug, Default)]
pub struct State {
    /// The user's file: what the title shows and what Save writes to.
    pub path: Option<PathBuf>,
    /// The file the worker reads pages from. Usually the same as `path`, but
    /// after saving over the open file it is a private copy of the original,
    /// because the edits and the undo history are expressed in terms of the
    /// original's pages.
    pub source: Option<PathBuf>,
    /// Pages in the source document, before any edits.
    pub page_count: usize,
    /// Source page indices, in the order they are currently shown.
    pub order: Vec<usize>,
    /// Extra rotation per source page index. A page with no rotation has no
    /// entry, so that two equal sets of edits compare equal.
    pub rotations: HashMap<usize, i32>,
    /// Selected positions in `order`, not source indices.
    pub selection: HashSet<usize>,
    /// The position shown in the main view.
    pub current: usize,
    /// The edits that match what is on disk at `path`.
    saved: Edits,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl State {
    /// Show a freshly opened document, forgetting any history.
    pub fn open(&mut self, path: PathBuf, page_count: usize) {
        self.undo.clear();
        self.redo.clear();
        self.show_fresh(path, page_count);
    }

    /// Show a document made by merging dropped files, keeping the way back to
    /// the one that was open before.
    pub fn open_merged(&mut self, path: PathBuf, page_count: usize) {
        if self.is_open() {
            let before = self.snapshot();
            self.push_undo(before);
        }
        self.redo.clear();
        self.show_fresh(path, page_count);
    }

    fn show_fresh(&mut self, path: PathBuf, page_count: usize) {
        self.source = Some(path.clone());
        self.path = Some(path);
        self.page_count = page_count;
        self.order = (0..page_count).collect();
        self.rotations.clear();
        self.selection.clear();
        self.current = 0;
        self.saved = self.edits();
    }

    pub fn is_open(&self) -> bool {
        self.path.is_some()
    }

    /// True when there are edits that have not been written to `path`.
    pub fn is_dirty(&self) -> bool {
        self.is_open() && self.edits() != self.saved
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
        let before = self.snapshot();

        for position in self.target_positions() {
            if let Some(page) = self.source_page(position) {
                let turned =
                    (self.rotations.get(&page).copied().unwrap_or(0) + degrees).rem_euclid(360);
                if turned == 0 {
                    self.rotations.remove(&page);
                } else {
                    self.rotations.insert(page, turned);
                }
            }
        }

        self.record(before);
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

        let before = self.snapshot();

        let mut remaining = Vec::with_capacity(self.order.len() - doomed.len());
        for (position, page) in self.order.iter().enumerate() {
            if !doomed.contains(&position) {
                remaining.push(*page);
            }
        }

        self.order = remaining;
        self.selection.clear();
        self.current = self.current.min(self.order.len().saturating_sub(1));
        self.record(before);

        Ok(())
    }

    /// Move one page so that it sits at `destination`.
    pub fn move_page(&mut self, from: usize, destination: usize) {
        if from >= self.order.len() || from == destination {
            return;
        }

        let before = self.snapshot();

        let page = self.order.remove(from);
        let destination = destination.min(self.order.len());
        self.order.insert(destination, page);

        self.selection.clear();
        self.current = destination;
        self.record(before);
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

    pub fn select_all(&mut self) {
        self.selection = (0..self.order.len()).collect();
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

    /// The file name to show in the title.
    pub fn display_name(&self) -> Option<String> {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
    }

    // ---- History -----------------------------------------------------------

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Where undo would go, so the window can tell whether it means reopening
    /// a different file first.
    pub fn peek_undo(&self) -> Option<&Snapshot> {
        self.undo.last()
    }

    pub fn peek_redo(&self) -> Option<&Snapshot> {
        self.redo.last()
    }

    /// Step back. Returns false when there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        let now = self.snapshot();
        self.redo.push(now);
        self.restore(previous);
        true
    }

    /// Step forward again. Returns false when there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let now = self.snapshot();
        self.push_undo(now);
        self.restore(next);
        true
    }

    /// Record that the current edits were written to `path`.
    ///
    /// Every step of history for the same source document now compares against
    /// what was just written, so undoing back to it shows as unedited.
    pub fn mark_saved(&mut self, path: PathBuf, written: Edits) {
        let source = self.source.clone();
        for snapshot in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            if snapshot.source == source {
                snapshot.path = Some(path.clone());
                snapshot.saved = written.clone();
            }
        }
        self.path = Some(path);
        self.saved = written;
    }

    /// The worker now reads `from` through a private copy at `to`, because
    /// `from` is about to be overwritten.
    pub fn relocate_source(&mut self, from: &Path, to: PathBuf) {
        let moves = |source: &mut Option<PathBuf>| {
            if source.as_deref() == Some(from) {
                *source = Some(to.clone());
            }
        };
        moves(&mut self.source);
        for snapshot in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            moves(&mut snapshot.source);
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            source: self.source.clone(),
            path: self.path.clone(),
            page_count: self.page_count,
            order: self.order.clone(),
            rotations: self.rotations.clone(),
            current: self.current,
            saved: self.saved.clone(),
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.source = snapshot.source;
        self.path = snapshot.path;
        self.page_count = snapshot.page_count;
        self.order = snapshot.order;
        self.rotations = snapshot.rotations;
        self.current = snapshot.current.min(self.order.len().saturating_sub(1));
        self.saved = snapshot.saved;
        self.selection.clear();
    }

    /// Keep `before` as an undo step if the edit just made changed anything.
    fn record(&mut self, before: Snapshot) {
        if before.order == self.order && before.rotations == self.rotations {
            return;
        }
        self.push_undo(before);
        self.redo.clear();
    }

    fn push_undo(&mut self, snapshot: Snapshot) {
        self.undo.push(snapshot);
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
    }
}

/// Holds two document states for side-by-side comparison with optional
/// synchronised navigation.
#[derive(Debug, Default)]
pub struct ComparisonState {
    pub left: State,
    pub right: State,
    /// When true, scroll, zoom, and page changes on one side are mirrored to the other.
    pub sync: bool,
}

impl ComparisonState {
    /// Open a document on the left side.
    pub fn open_left(&mut self, path: PathBuf, page_count: usize) {
        self.left.open(path, page_count);
    }

    /// Open a document on the right side.
    pub fn open_right(&mut self, path: PathBuf, page_count: usize) {
        self.right.open(path, page_count);
    }

    /// One side's document state.
    pub fn side(&self, side: ComparisonSide) -> &State {
        match side {
            ComparisonSide::Left => &self.left,
            ComparisonSide::Right => &self.right,
        }
    }

    /// Whether both sides have a document open.
    pub fn both_open(&self) -> bool {
        self.left.is_open() && self.right.is_open()
    }

    /// Apply a page change to one side, mirroring to the other if sync is enabled.
    pub fn set_current(&mut self, side: ComparisonSide, position: usize) {
        match side {
            ComparisonSide::Left => {
                self.left.current = position.min(self.left.visible_pages().saturating_sub(1));
                if self.sync {
                    self.right.current = self
                        .left
                        .current
                        .min(self.right.visible_pages().saturating_sub(1));
                }
            }
            ComparisonSide::Right => {
                self.right.current = position.min(self.right.visible_pages().saturating_sub(1));
                if self.sync {
                    self.left.current = self
                        .right
                        .current
                        .min(self.left.visible_pages().saturating_sub(1));
                }
            }
        }
    }

    /// Toggle synchronised navigation.
    pub fn toggle_sync(&mut self) -> bool {
        self.sync = !self.sync;
        self.sync
    }
}

/// Which side of the comparison view an action applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonSide {
    Left,
    Right,
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
        assert!(!state.is_dirty());
        assert!(state.is_open());
        assert!(!state.can_undo());
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
    fn a_full_turn_leaves_the_document_unedited() {
        let mut state = opened(2);
        state.select_only(0);
        for _ in 0..4 {
            state.rotate(90);
        }
        assert!(state.rotations.is_empty());
        assert!(!state.is_dirty());
    }

    #[test]
    fn deleting_removes_the_selection() {
        let mut state = opened(4);
        state.select_only(1);
        state.toggle_selection(3);

        state.delete().unwrap();

        assert_eq!(state.order, vec![0, 2]);
        assert!(state.is_dirty());
    }

    #[test]
    fn deleting_the_last_page_is_refused() {
        let mut state = opened(1);
        state.select_only(0);

        assert!(state.delete().is_err());
        assert_eq!(state.order.len(), 1);
        assert!(!state.can_undo(), "a refused edit leaves no history");
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
    fn select_all_selects_every_position() {
        let mut state = opened(3);
        state.select_all();
        assert_eq!(state.target_positions(), vec![0, 1, 2]);
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
        assert!(!state.is_dirty());
        assert!(!state.can_undo());
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

    // ---- Undo and redo -----------------------------------------------------

    #[test]
    fn undo_reverses_each_kind_of_edit_in_turn() {
        let mut state = opened(4);
        state.select_only(1);
        state.rotate(90);
        state.move_page(0, 3);
        state.select_only(0);
        state.delete().unwrap();

        assert_eq!(state.order, vec![2, 3, 0]);

        assert!(state.undo());
        assert_eq!(state.order, vec![1, 2, 3, 0], "delete undone");

        assert!(state.undo());
        assert_eq!(state.order, vec![0, 1, 2, 3], "move undone");
        assert_eq!(state.rotations.get(&1), Some(&90));

        assert!(state.undo());
        assert!(state.rotations.is_empty(), "rotation undone");
        assert!(!state.is_dirty());

        assert!(!state.undo(), "nothing left");
    }

    #[test]
    fn redo_replays_what_was_undone() {
        let mut state = opened(3);
        state.move_page(0, 2);
        state.select_only(1);
        state.rotate(-90);
        let edited = state.edits();

        state.undo();
        state.undo();
        assert_eq!(state.order, vec![0, 1, 2]);

        assert!(state.redo());
        assert!(state.redo());
        assert_eq!(state.edits(), edited);
        assert!(!state.redo(), "nothing left");
    }

    #[test]
    fn a_new_edit_discards_the_redo_steps() {
        let mut state = opened(3);
        state.move_page(0, 2);
        state.undo();
        assert!(state.can_redo());

        state.move_page(2, 0);
        assert!(!state.can_redo());
    }

    #[test]
    fn undo_puts_back_the_page_in_view_and_clears_the_selection() {
        let mut state = opened(4);
        state.current = 3;
        state.selection.insert(0);
        state.selection.insert(1);
        state.delete().unwrap();
        assert_eq!(state.current, 1, "the view was clamped to what is left");

        state.undo();

        assert_eq!(state.current, 3);
        assert!(state.selection.is_empty());
    }

    #[test]
    fn undoing_back_to_the_saved_point_is_clean() {
        let mut state = opened(3);
        state.move_page(0, 2);
        state.mark_saved(PathBuf::from("/tmp/report.pdf"), state.edits());
        assert!(!state.is_dirty());

        state.move_page(0, 2);
        assert!(state.is_dirty());

        state.undo();
        assert!(!state.is_dirty(), "back where the file on disk is");

        state.undo();
        assert!(state.is_dirty(), "before the save is now an unsaved change");
    }

    #[test]
    fn save_as_renames_the_document_throughout_its_history() {
        let mut state = opened(2);
        state.move_page(0, 1);
        state.mark_saved(PathBuf::from("/tmp/copy.pdf"), state.edits());

        state.undo();
        assert_eq!(state.path, Some(PathBuf::from("/tmp/copy.pdf")));
        assert_eq!(state.source, Some(PathBuf::from("/tmp/report.pdf")));
        assert_eq!(state.suggested_name("", "pdf"), "copy.pdf");
    }

    #[test]
    fn relocating_the_source_follows_through_history() {
        let mut state = opened(2);
        state.move_page(0, 1);
        state.relocate_source(
            Path::new("/tmp/report.pdf"),
            PathBuf::from("/tmp/copy-1.pdf"),
        );

        assert_eq!(state.source, Some(PathBuf::from("/tmp/copy-1.pdf")));
        assert_eq!(
            state.peek_undo().unwrap().source,
            Some(PathBuf::from("/tmp/copy-1.pdf"))
        );
        assert_eq!(state.path, Some(PathBuf::from("/tmp/report.pdf")));
    }

    #[test]
    fn undoing_a_merge_returns_to_the_previous_document() {
        let mut state = opened(3);
        state.move_page(0, 2);
        let before = state.edits();

        state.open_merged(PathBuf::from("/tmp/merged.pdf"), 7);
        assert_eq!(state.page_count, 7);
        assert!(!state.is_dirty());

        let back = state.peek_undo().unwrap();
        assert_eq!(back.source, Some(PathBuf::from("/tmp/report.pdf")));
        assert_eq!(back.page_count, 3);

        state.undo();
        assert_eq!(state.path, Some(PathBuf::from("/tmp/report.pdf")));
        assert_eq!(state.edits(), before);
        assert!(state.is_dirty(), "the earlier edits are still unsaved");

        state.redo();
        assert_eq!(state.path, Some(PathBuf::from("/tmp/merged.pdf")));
        assert_eq!(state.page_count, 7);
    }

    #[test]
    fn opening_another_document_forgets_history() {
        let mut state = opened(3);
        state.move_page(0, 2);

        state.open(PathBuf::from("/tmp/other.pdf"), 2);
        assert!(!state.can_undo());
        assert!(!state.can_redo());
    }

    #[test]
    fn history_is_bounded() {
        let mut state = opened(2);
        for _ in 0..(HISTORY_LIMIT + 50) {
            state.move_page(0, 1);
        }
        let mut steps = 0;
        while state.undo() {
            steps += 1;
        }
        assert_eq!(steps, HISTORY_LIMIT);
    }
}
