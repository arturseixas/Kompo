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
            && g.label.is_empty()
        {
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

    /// During a gesture, re-applies an edit from the state the gesture started
    /// in (so dragging back and forth never accumulates clamping errors).
    /// Outside a gesture this behaves like [`apply`](Self::apply).
    pub fn apply_from_gesture_base<E>(&mut self, label: &str, edit: impl FnOnce(&mut Project) -> Result<(), E>) -> Result<(), E> {
        let Some(g) = &self.gesture else { return self.apply(label, edit) };
        let mut next = g.project.clone();
        edit(&mut next)?;
        if next != self.current {
            self.current = next;
            self.redo.clear();
            self.revision += 1;
        }
        Ok(())
    }

    /// Applies a view preference stored in the project (e.g. track heights)
    /// without creating an undo step.
    pub fn view_change(&mut self, f: impl FnOnce(&mut Project)) {
        f(&mut self.current);
        self.revision += 1;
    }

    /// Cancels the gesture, restoring the state it started from.
    pub fn cancel_gesture(&mut self) {
        if let Some(g) = self.gesture.take()
            && g.project != self.current
        {
            self.current = g.project;
            self.revision += 1;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;

    #[test]
    fn gesture_is_one_step_and_reapplies_from_base() {
        let mut h = History::new(Project::new("a"));
        h.begin_gesture("rename");
        for i in 0..10 {
            h.apply_from_gesture_base::<()>("rename", |p| {
                p.name = format!("{}{}", p.name, i);
                Ok(())
            })
            .unwrap();
        }
        // only the last application counts (base + "9")
        assert_eq!(h.project().name, "a9");
        h.end_gesture();
        assert_eq!(h.undo_depth(), 1);
        h.undo();
        assert_eq!(h.project().name, "a");
    }

    #[test]
    fn cancel_restores() {
        let mut h = History::new(Project::new("a"));
        h.begin_gesture("x");
        h.edit("x", |p| p.name = "b".into());
        h.cancel_gesture();
        assert_eq!(h.project().name, "a");
        assert!(!h.can_undo());
    }

    #[test]
    fn noop_edits_are_not_recorded() {
        let mut h = History::new(Project::new("a"));
        h.edit("nothing", |_| {});
        assert!(!h.can_undo());
        assert!(!h.is_dirty());
    }
}
