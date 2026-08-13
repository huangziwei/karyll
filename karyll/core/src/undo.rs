//! Undo history.
//!
//! Edits are grouped, so one undo removes a burst of typing rather than one
//! character. A group stays open while typing continues at the same place and
//! closes at a natural boundary — a newline, a cursor move, a save.

use crate::buffer::Buffer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Insert { at: usize, text: Vec<char> },
    Delete { at: usize, text: Vec<char> },
}

impl Edit {
    /// The edit that undoes this one.
    fn inverse(&self) -> Edit {
        match self {
            Edit::Insert { at, text } => Edit::Delete {
                at: *at,
                text: text.clone(),
            },
            Edit::Delete { at, text } => Edit::Insert {
                at: *at,
                text: text.clone(),
            },
        }
    }

    /// Apply to `buf`, returning where the cursor belongs afterwards.
    fn apply(&self, buf: &mut Buffer) -> usize {
        match self {
            Edit::Insert { at, text } => {
                buf.insert(*at, text);
                at + text.len()
            }
            Edit::Delete { at, text } => {
                buf.remove(*at, text.len());
                *at
            }
        }
    }
}

#[derive(Default)]
pub struct History {
    done: Vec<Vec<Edit>>,
    undone: Vec<Vec<Edit>>,
    /// The group still accumulating, if any.
    open: Option<Vec<Edit>>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an edit into the open group, opening one if needed.
    ///
    /// Any new edit invalidates the redo stack — the future it led to is no
    /// longer reachable.
    pub fn record(&mut self, edit: Edit) {
        self.undone.clear();
        self.open.get_or_insert_with(Vec::new).push(edit);
    }

    /// Close the open group, so the next edit starts a new undo step.
    pub fn commit(&mut self) {
        if let Some(group) = self.open.take()
            && !group.is_empty()
        {
            self.done.push(group);
        }
    }

    pub fn can_undo(&self) -> bool {
        self.open.is_some() || !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Undo one group. Returns where the cursor belongs, or `None` if there was
    /// nothing to undo.
    pub fn undo(&mut self, buf: &mut Buffer) -> Option<usize> {
        self.commit();
        let group = self.done.pop()?;
        let mut cursor = None;
        for edit in group.iter().rev() {
            cursor = Some(edit.inverse().apply(buf));
        }
        self.undone.push(group);
        cursor
    }

    /// Redo one group. Returns where the cursor belongs, or `None` if there was
    /// nothing to redo.
    pub fn redo(&mut self, buf: &mut Buffer) -> Option<usize> {
        let group = self.undone.pop()?;
        let mut cursor = None;
        for edit in &group {
            cursor = Some(edit.apply(buf));
        }
        self.done.push(group);
        cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(b: &Buffer) -> String {
        b.chars().collect()
    }

    #[test]
    fn undo_and_redo_round_trip() {
        let mut buf = Buffer::from_text("hello");
        let mut h = History::new();

        let e = Edit::Insert {
            at: 5,
            text: " world".chars().collect(),
        };
        e.apply(&mut buf);
        h.record(e);
        h.commit();
        assert_eq!(text(&buf), "hello world");

        assert_eq!(h.undo(&mut buf), Some(5));
        assert_eq!(text(&buf), "hello");
        assert_eq!(h.redo(&mut buf), Some(11));
        assert_eq!(text(&buf), "hello world");
    }

    #[test]
    fn a_group_undoes_as_one_step() {
        let mut buf = Buffer::new();
        let mut h = History::new();
        for (i, c) in "word".chars().enumerate() {
            let e = Edit::Insert {
                at: i,
                text: vec![c],
            };
            e.apply(&mut buf);
            h.record(e);
        }
        h.commit();
        assert_eq!(text(&buf), "word");
        h.undo(&mut buf);
        assert_eq!(text(&buf), "", "the whole burst should go at once");
        assert!(!h.can_undo());
    }

    #[test]
    fn undo_without_committing_closes_the_open_group() {
        let mut buf = Buffer::new();
        let mut h = History::new();
        let e = Edit::Insert {
            at: 0,
            text: "hi".chars().collect(),
        };
        e.apply(&mut buf);
        h.record(e);
        assert!(h.can_undo());
        h.undo(&mut buf);
        assert_eq!(text(&buf), "");
    }

    #[test]
    fn a_new_edit_discards_the_redo_stack() {
        let mut buf = Buffer::from_text("a");
        let mut h = History::new();
        let e = Edit::Insert {
            at: 1,
            text: vec!['b'],
        };
        e.apply(&mut buf);
        h.record(e);
        h.commit();
        h.undo(&mut buf);
        assert!(h.can_redo());

        let e = Edit::Insert {
            at: 1,
            text: vec!['c'],
        };
        e.apply(&mut buf);
        h.record(e);
        assert!(!h.can_redo(), "the undone future is unreachable now");
    }

    #[test]
    fn deletes_undo_to_the_original_text() {
        let mut buf = Buffer::from_text("你好世界");
        let mut h = History::new();
        let taken = buf.slice(1..3);
        let e = Edit::Delete { at: 1, text: taken };
        e.apply(&mut buf);
        h.record(e);
        h.commit();
        assert_eq!(text(&buf), "你界");
        h.undo(&mut buf);
        assert_eq!(text(&buf), "你好世界");
    }

    #[test]
    fn nothing_to_undo_or_redo_is_not_an_error() {
        let mut buf = Buffer::new();
        let mut h = History::new();
        assert_eq!(h.undo(&mut buf), None);
        assert_eq!(h.redo(&mut buf), None);
    }

    #[test]
    fn multiple_groups_unwind_in_order() {
        let mut buf = Buffer::new();
        let mut h = History::new();
        for (i, word) in ["one", "two", "three"].iter().enumerate() {
            let at = buf.len();
            let e = Edit::Insert {
                at,
                text: word.chars().collect(),
            };
            e.apply(&mut buf);
            h.record(e);
            h.commit();
            assert_eq!(h.done.len(), i + 1);
        }
        assert_eq!(text(&buf), "onetwothree");
        h.undo(&mut buf);
        assert_eq!(text(&buf), "onetwo");
        h.undo(&mut buf);
        assert_eq!(text(&buf), "one");
        h.redo(&mut buf);
        assert_eq!(text(&buf), "onetwo");
    }
}
