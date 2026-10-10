//! Unlimited undo/redo based on persistent snapshots.
//!
//! Every edit produces a new [`Project`] value; the previous value is pushed
//! on the undo stack. Because projects use persistent collections, consecutive
//! snapshots share almost all their memory: an edit that touches one clip
//! costs roughly one clip plus a few tree nodes, not a full copy.
//!
//! Continuous gestures (dragging a slider, moving a clip) are grouped with
//! [`History::begin_gesture`] / [`History::end_gesture`] so they produce a
//! single undo step.

use crate::project::Project;

#[derive(Clone, Debug)]
struct Entry {
    label: String,
    project: Project,
}

#[derive(Clone, Debug)]
pub struct History {
    current: Project,
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    gesture: Option<Entry>,
    /// Incremented on every change; lets observers detect modifications cheaply.
    revision: u64,
    saved_revision: u64,
}

impl History {
    pub fn new(project: Project) -> Self {
        Self { current: project, undo: Vec::new(), redo: Vec::new(), gesture: None, revision: 0, saved_revision: 0 }
    }

    pub fn project(&self) -> &Project {
        &self.current
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }

    /// Replaces the project without recording history (e.g. after loading).
    pub fn reset(&mut self, project: Project) {
        *self = Self::new(project);
    }

    /// Applies an edit. If the closure returns `Err`, the project is left
    /// untouched and nothing is recorded.
    pub fn apply<E>(&mut self, label: &str, edit: impl FnOnce(&mut Project) -> Result<(), E>) -> Result<(), E> {
        let mut next = self.current.clone();
        edit(&mut next)?;
        if next == self.current {
            return Ok(());
        }
        let prev = std::mem::replace(&mut self.current, next);
        if self.gesture.is_none() {
            self.undo.push(Entry { label: label.to_string(), project: prev });
        } else if let Some(g) = &mut self.gesture
            && g.label.is_empty() {
                g.label = label.to_string();
            }
        self.redo.clear();
        self.revision += 1;
        Ok(())
    }

    /// Like [`apply`](Self::apply) for infallible edits.
    pub fn edit(&mut self, label: &str, edit: impl FnOnce(&mut Project)) {
        let _ = self.apply::<()>(label, |p| {
            edit(p);
            Ok(())
        });
    }

    /// Starts grouping subsequent edits into one undo step.
    pub fn begin_gesture(&mut self, label: &str) {
        if self.gesture.is_none() {
            self.gesture = Some(Entry { label: label.to_string(), project: self.current.clone() });
        }
    }

    /// Ends the current gesture, recording one undo step if anything changed.
    pub fn end_gesture(&mut self) {
        if let Some(g) = self.gesture.take()
            && g.project != self.current
        {
            self.undo.push(g);
            self.redo.clear();
        }
    }

    pub fn in_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    pub fn undo(&mut self) -> bool {
        self.end_gesture();
        let Some(e) = self.undo.pop() else { return false };
        let cur = std::mem::replace(&mut self.current, e.project);
        self.redo.push(Entry { label: e.label, project: cur });
        self.revision += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        self.end_gesture();
        let Some(e) = self.redo.pop() else { return false };
        let cur = std::mem::replace(&mut self.current, e.project);
        self.undo.push(Entry { label: e.label, project: cur });
        self.revision += 1;
        true
    }

    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }
}
