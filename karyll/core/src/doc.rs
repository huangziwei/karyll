//! A document: text, a cursor, a selection, and its undo history.
//!
//! Positions here are character indices into the buffer. Visual navigation —
//! moving by wrapped line rather than by logical line — needs the layout, and
//! so belongs with whatever owns the font, not here.
//!
//! **The selection lives here rather than in the app**, because every edit has
//! to interact with it: typing replaces it, backspace deletes it. Held outside,
//! each of those call sites would have to remember to check. The rule is
//! enforced in the two methods that edit, not in the dozen that call them.

use std::ops::Range;

use crate::buffer::Buffer;
use crate::undo::{Edit, History};
use crate::word;

pub struct Document {
    buffer: Buffer,
    cursor: usize,
    /// Where a selection began. The selection is the span between this and the
    /// cursor, in whichever order they happen to fall — so extending leftwards
    /// needs no special case.
    ///
    /// Not to be confused with `group_anchor` below, which is undo bookkeeping
    /// and has nothing to do with selecting.
    anchor: Option<usize>,
    history: History,
    /// Where the cursor was when the open undo group started, so an edit that
    /// jumps elsewhere can close the group first.
    group_anchor: Option<usize>,
    dirty: bool,
}

impl Document {
    pub fn new() -> Self {
        Self::from_text("")
    }

    pub fn from_text(s: &str) -> Self {
        Self {
            buffer: Buffer::from_text(s),
            cursor: 0,
            anchor: None,
            history: History::new(),
            group_anchor: None,
            dirty: false,
        }
    }

    pub fn text(&self) -> String {
        self.buffer.chars().collect()
    }

    pub fn chars(&self) -> Vec<char> {
        self.buffer.chars().collect()
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The selected span, ordered, or `None` when nothing is selected.
    ///
    /// An anchor sitting exactly on the cursor is not a selection — shift-arrow
    /// out and back leaves an empty span, and reporting that as a selection
    /// would make the next keystroke "replace nothing" instead of typing.
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        let (start, end) = (anchor.min(self.cursor), anchor.max(self.cursor));
        (start < end).then_some(start..end)
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection()
            .map(|range| self.buffer.slice(range).into_iter().collect())
    }

    /// Extend the selection to `idx`, anchoring where the cursor is if this is
    /// the start of one.
    pub fn extend_to(&mut self, idx: usize) {
        let idx = idx.min(self.buffer.len());
        self.anchor.get_or_insert(self.cursor);
        if idx != self.cursor {
            self.break_undo_group();
        }
        self.cursor = idx;
    }

    /// Select `range`, leaving the cursor at its end so that extending from
    /// here continues in the direction a writer just moved.
    pub fn select(&mut self, range: Range<usize>) {
        let end = range.end.min(self.buffer.len());
        self.anchor = Some(range.start.min(end));
        self.cursor = end;
        self.break_undo_group();
    }

    pub fn select_all(&mut self) {
        self.select(0..self.buffer.len());
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// Delete the selection if there is one, reporting whether there was.
    ///
    /// The two edit paths call this first, which is what makes "typing replaces
    /// the selection" true everywhere rather than everywhere someone remembered.
    pub fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let at = range.start;
        let text = self.buffer.slice(range.clone());
        self.record(Edit::Delete { at, text }, at);
        self.buffer.remove(at, range.len());
        self.cursor = at;
        self.anchor = None;
        // Left open, so an insert that follows immediately joins this group and
        // replacing a selection undoes in one step rather than two.
        self.group_anchor = Some(at);
        true
    }

    /// True when there are edits not yet written to disk.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_saved(&mut self) {
        self.history.commit();
        self.group_anchor = None;
        self.dirty = false;
    }

    /// Close the current undo group, so the next edit starts a new one.
    pub fn break_undo_group(&mut self) {
        self.history.commit();
        self.group_anchor = None;
    }

    fn record(&mut self, edit: Edit, at: usize) {
        // Typing that resumes somewhere else is a new thought; give it its own
        // undo step rather than folding it into the previous burst.
        if self.group_anchor.is_some_and(|a| a != at) {
            self.history.commit();
        }
        self.history.record(edit);
        self.dirty = true;
    }

    pub fn insert(&mut self, text: &str) {
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            return;
        }
        self.delete_selection();
        let at = self.cursor;
        self.record(
            Edit::Insert {
                at,
                text: chars.clone(),
            },
            at,
        );
        self.buffer.insert(at, &chars);
        self.cursor = at + chars.len();
        self.group_anchor = Some(self.cursor);
        // A newline ends the thought as well as the line.
        if chars.contains(&'\n') {
            self.break_undo_group();
        }
    }

    pub fn insert_char(&mut self, c: char) {
        self.insert(c.encode_utf8(&mut [0u8; 4]));
    }

    /// Delete the character before the cursor, or the selection if there is one.
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let at = self.cursor - 1;
        let text = self.buffer.slice(at..self.cursor);
        self.record(Edit::Delete { at, text }, self.cursor);
        self.buffer.remove(at, 1);
        self.cursor = at;
        self.group_anchor = Some(self.cursor);
    }

    /// Delete the character under the cursor, or the selection if there is one.
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor >= self.buffer.len() {
            return;
        }
        let at = self.cursor;
        let text = self.buffer.slice(at..at + 1);
        self.record(Edit::Delete { at, text }, at);
        self.buffer.remove(at, 1);
        self.group_anchor = Some(self.cursor);
    }

    /// Put `with` in place of `range`, as one undo step.
    ///
    /// An empty `with` is a deletion. [`Document::insert`] returns early on an
    /// empty string, so the deletion is spelled out rather than left to it.
    pub fn replace_range(&mut self, range: Range<usize>, with: &str) {
        self.select(range);
        if with.is_empty() {
            self.delete_selection();
            self.break_undo_group();
        } else {
            self.insert(with);
        }
    }

    /// Put `with` in place of every one of `ranges`, as a single undo step.
    /// Reports how many were changed.
    ///
    /// **Applied last first**, so each range still describes the text it was
    /// found in: a replacement of a different length moves everything after it.
    ///
    /// One undo step, because it was one decision.
    ///
    /// `ranges` must be ordered and non-overlapping, which is what
    /// [`crate::find::matches`] returns.
    pub fn replace_all(&mut self, ranges: &[Range<usize>], with: &str) -> usize {
        let text: Vec<char> = with.chars().collect();
        self.break_undo_group();
        let mut changed = 0;
        for range in ranges.iter().rev() {
            if range.is_empty() || range.end > self.buffer.len() {
                continue;
            }
            let removed = self.buffer.slice(range.clone());
            self.record(
                Edit::Delete {
                    at: range.start,
                    text: removed,
                },
                range.start,
            );
            self.buffer.remove(range.start, range.len());
            if !text.is_empty() {
                self.record(
                    Edit::Insert {
                        at: range.start,
                        text: text.clone(),
                    },
                    range.start,
                );
                self.buffer.insert(range.start, &text);
            }
            changed += 1;
        }
        // Closed here, so typing after a replace-all is its own undo step.
        self.break_undo_group();
        self.cursor = self.cursor.min(self.buffer.len());
        self.anchor = None;
        changed
    }

    pub fn undo(&mut self) {
        if let Some(cursor) = self.history.undo(&mut self.buffer) {
            self.cursor = cursor.min(self.buffer.len());
            self.anchor = None;
            self.group_anchor = None;
            self.dirty = true;
        }
    }

    pub fn redo(&mut self) {
        if let Some(cursor) = self.history.redo(&mut self.buffer) {
            self.cursor = cursor.min(self.buffer.len());
            self.anchor = None;
            self.group_anchor = None;
            self.dirty = true;
        }
    }

    /// Move the cursor to `idx`, clamped to the document, dropping any
    /// selection.
    ///
    /// This is the plain move, and it clears — it is what a tap and an undo
    /// restore call, and both should. Extending is `extend_to`, a separate verb
    /// so the difference is legible at the call site rather than hidden in a
    /// boolean argument.
    pub fn set_cursor(&mut self, idx: usize) {
        let idx = idx.min(self.buffer.len());
        if idx != self.cursor {
            self.break_undo_group();
        }
        self.anchor = None;
        self.cursor = idx;
    }

    /// Left arrow: collapse to the near edge of a selection, or step back one.
    ///
    /// Collapsing rather than stepping is what every text field does, and the
    /// difference shows the moment someone selects a word and presses left
    /// meaning "put me before that".
    pub fn move_left(&mut self) {
        match self.selection() {
            Some(range) => self.set_cursor(range.start),
            None => self.set_cursor(self.cursor.saturating_sub(1)),
        }
    }

    pub fn move_right(&mut self) {
        match self.selection() {
            Some(range) => self.set_cursor(range.end),
            None => self.set_cursor(self.cursor + 1),
        }
    }

    pub fn extend_left(&mut self) {
        self.extend_to(self.cursor.saturating_sub(1));
    }

    pub fn extend_right(&mut self) {
        self.extend_to(self.cursor + 1);
    }

    /// Index of the first character on the logical line containing `idx`.
    pub fn line_start(&self, idx: usize) -> usize {
        let idx = idx.min(self.buffer.len());
        (0..idx)
            .rev()
            .find(|&i| self.buffer.char_at(i) == Some('\n'))
            .map_or(0, |i| i + 1)
    }

    /// Index just past the last character of the logical line containing `idx`,
    /// not counting the newline itself.
    pub fn line_end(&self, idx: usize) -> usize {
        let len = self.buffer.len();
        let idx = idx.min(len);
        (idx..len)
            .find(|&i| self.buffer.char_at(i) == Some('\n'))
            .unwrap_or(len)
    }

    pub fn move_to_line_start(&mut self) {
        self.set_cursor(self.line_start(self.cursor));
    }

    pub fn move_to_line_end(&mut self) {
        self.set_cursor(self.line_end(self.cursor));
    }

    pub fn move_to_start(&mut self) {
        self.set_cursor(0);
    }

    pub fn move_to_end(&mut self) {
        self.set_cursor(self.buffer.len());
    }

    pub fn extend_to_start(&mut self) {
        self.extend_to(0);
    }

    pub fn extend_to_end(&mut self) {
        self.extend_to(self.buffer.len());
    }

    pub fn extend_to_line_start(&mut self) {
        self.extend_to(self.line_start(self.cursor));
    }

    pub fn extend_to_line_end(&mut self) {
        self.extend_to(self.line_end(self.cursor));
    }

    /// Where the word boundary to either side of the cursor falls.
    ///
    /// The buffer has a gap in the middle of it, so there is no `&[char]` to
    /// hand out and the document is flattened per call. That is the same cost
    /// the renderer already pays on every paint, against a keystroke that
    /// happens at most a few times a second, so it is not worth a cleverer
    /// arrangement.
    fn word_boundary(&self, forward: bool) -> usize {
        let chars = self.chars();
        if forward {
            word::word_end(&chars, self.cursor)
        } else {
            word::word_start(&chars, self.cursor)
        }
    }

    pub fn move_word_left(&mut self) {
        self.set_cursor(self.word_boundary(false));
    }

    pub fn move_word_right(&mut self) {
        self.set_cursor(self.word_boundary(true));
    }

    pub fn extend_word_left(&mut self) {
        self.extend_to(self.word_boundary(false));
    }

    pub fn extend_word_right(&mut self) {
        self.extend_to(self.word_boundary(true));
    }

    /// Delete back to the start of the word — or the selection, if one is up.
    ///
    /// A selection wins because that is what the writer is pointing at; only
    /// with nothing selected does this mean "the word behind me".
    pub fn delete_word_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        let start = self.word_boundary(false);
        if start < self.cursor {
            self.select(start..self.cursor);
            self.delete_selection();
        }
    }

    pub fn delete_word_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        let end = self.word_boundary(true);
        if end > self.cursor {
            self.select(self.cursor..end);
            self.delete_selection();
        }
    }

    /// Delete back to the start of the line — ⌘⌫ on a Mac.
    pub fn delete_to_line_start(&mut self) {
        if self.delete_selection() {
            return;
        }
        let start = self.line_start(self.cursor);
        if start < self.cursor {
            self.select(start..self.cursor);
            self.delete_selection();
        }
    }

    pub fn delete_to_line_end(&mut self) {
        if self.delete_selection() {
            return;
        }
        let end = self.line_end(self.cursor);
        if end > self.cursor {
            self.select(self.cursor..end);
            self.delete_selection();
        }
    }

    /// Select the word at `idx` — what a double-tap asks for.
    pub fn select_word_at(&mut self, idx: usize) {
        let range = word::word_at(&self.chars(), idx);
        self.select(range);
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_advances_the_cursor() {
        let mut d = Document::new();
        d.insert("hello");
        assert_eq!(d.text(), "hello");
        assert_eq!(d.cursor(), 5);
        assert!(d.is_dirty());
    }

    #[test]
    fn typing_cjk_counts_characters_not_bytes() {
        let mut d = Document::new();
        d.insert("你好");
        assert_eq!(d.cursor(), 2);
        d.backspace();
        assert_eq!(d.text(), "你");
        assert_eq!(d.cursor(), 1);
    }

    #[test]
    fn backspace_at_the_start_does_nothing() {
        let mut d = Document::from_text("abc");
        d.set_cursor(0);
        d.backspace();
        assert_eq!(d.text(), "abc");
        assert_eq!(d.cursor(), 0);
    }

    #[test]
    fn delete_at_the_end_does_nothing() {
        let mut d = Document::from_text("abc");
        d.set_cursor(3);
        d.delete();
        assert_eq!(d.text(), "abc");
    }

    #[test]
    fn a_burst_of_typing_undoes_as_one_step() {
        let mut d = Document::new();
        for c in "hello".chars() {
            d.insert_char(c);
        }
        d.undo();
        assert_eq!(d.text(), "");
    }

    #[test]
    fn a_newline_ends_the_undo_group() {
        let mut d = Document::new();
        for c in "one\ntwo".chars() {
            d.insert_char(c);
        }
        d.undo();
        assert_eq!(d.text(), "one\n", "only the second line should go");
        d.undo();
        assert_eq!(d.text(), "");
    }

    #[test]
    fn moving_the_cursor_ends_the_undo_group() {
        let mut d = Document::new();
        d.insert("hello");
        d.set_cursor(0);
        d.insert(">");
        assert_eq!(d.text(), ">hello");
        d.undo();
        assert_eq!(d.text(), "hello", "the two bursts are separate steps");
    }

    #[test]
    fn undo_then_redo_restores_the_cursor_too() {
        let mut d = Document::new();
        d.insert("abc");
        d.undo();
        assert_eq!(d.cursor(), 0);
        d.redo();
        assert_eq!(d.text(), "abc");
        assert_eq!(d.cursor(), 3);
    }

    #[test]
    fn saving_clears_dirty_until_the_next_edit() {
        let mut d = Document::new();
        d.insert("x");
        assert!(d.is_dirty());
        d.mark_saved();
        assert!(!d.is_dirty());
        d.insert("y");
        assert!(d.is_dirty());
    }

    #[test]
    fn logical_line_bounds() {
        let d = Document::from_text("one\ntwo\nthree");
        assert_eq!(d.line_start(0), 0);
        assert_eq!(d.line_end(0), 3);
        assert_eq!(d.line_start(5), 4);
        assert_eq!(d.line_end(5), 7);
        assert_eq!(d.line_start(13), 8);
        assert_eq!(d.line_end(13), 13);
    }

    #[test]
    fn line_bounds_on_a_blank_line() {
        let d = Document::from_text("a\n\nb");
        assert_eq!(d.line_start(2), 2);
        assert_eq!(d.line_end(2), 2);
    }

    #[test]
    fn cursor_movement_clamps_to_the_document() {
        let mut d = Document::from_text("ab");
        d.move_left();
        assert_eq!(d.cursor(), 0);
        for _ in 0..5 {
            d.move_right();
        }
        assert_eq!(d.cursor(), 2);
    }

    #[test]
    fn home_and_end_stay_on_their_line() {
        let mut d = Document::from_text("one\ntwo");
        d.set_cursor(5);
        d.move_to_line_start();
        assert_eq!(d.cursor(), 4);
        d.move_to_line_end();
        assert_eq!(d.cursor(), 7);
    }

    mod replacing {
        use super::*;
        use crate::find;

        /// Every occurrence of `needle`, the way the find bar hands them over.
        fn hits(d: &Document, needle: &str) -> Vec<Range<usize>> {
            let chars = d.chars();
            find::matches(&chars, &needle.chars().collect::<Vec<_>>())
        }

        #[test]
        fn one_occurrence_becomes_the_other_word() {
            let mut d = Document::from_text("the colour of it");
            d.replace_range(4..10, "color");
            assert_eq!(d.text(), "the color of it");
        }

        #[test]
        fn replacing_one_with_nothing_deletes_it() {
            let mut d = Document::from_text("a very good line");
            d.replace_range(2..7, "");
            assert_eq!(d.text(), "a good line");
        }

        /// A forwards loop derails: every replacement of a different length
        /// moves the ranges after it, and by the last one they point into the
        /// middle of words.
        #[test]
        fn a_longer_replacement_does_not_derange_the_ones_after_it() {
            let mut d = Document::from_text("cat, cat, cat");
            let found = hits(&d, "cat");
            assert_eq!(d.replace_all(&found, "leopard"), 3);
            assert_eq!(d.text(), "leopard, leopard, leopard");
        }

        #[test]
        fn a_shorter_replacement_lands_the_same_way() {
            let mut d = Document::from_text("leopard, leopard, leopard");
            let found = hits(&d, "leopard");
            assert_eq!(d.replace_all(&found, "cat"), 3);
            assert_eq!(d.text(), "cat, cat, cat");
        }

        #[test]
        fn replacing_all_with_nothing_removes_every_one() {
            let mut d = Document::from_text("it was very very good");
            let found = hits(&d, "very ");
            assert_eq!(d.replace_all(&found, ""), 2);
            assert_eq!(d.text(), "it was good");
        }

        /// Forty replacements are one decision, so they are one undo.
        #[test]
        fn all_of_them_undo_together() {
            let mut d = Document::from_text("cat, cat, cat");
            let found = hits(&d, "cat");
            d.replace_all(&found, "leopard");
            d.undo();
            assert_eq!(d.text(), "cat, cat, cat");
            d.redo();
            assert_eq!(d.text(), "leopard, leopard, leopard");
        }

        /// Typing afterwards must not be swept up by the same undo.
        #[test]
        fn what_is_typed_next_is_its_own_step() {
            let mut d = Document::from_text("cat");
            let found = hits(&d, "cat");
            d.replace_all(&found, "dog");
            d.move_to_end();
            d.insert("s");
            assert_eq!(d.text(), "dogs");
            d.undo();
            assert_eq!(d.text(), "dog");
            d.undo();
            assert_eq!(d.text(), "cat");
        }

        #[test]
        fn cjk_replaces_by_character_rather_than_by_byte() {
            let mut d = Document::from_text("書いた、書いた");
            let found = hits(&d, "書");
            assert_eq!(d.replace_all(&found, "描"), 2);
            assert_eq!(d.text(), "描いた、描いた");
        }

        /// Nothing to replace is not a document with an empty undo step in it.
        #[test]
        fn no_matches_changes_nothing() {
            let mut d = Document::from_text("hello");
            assert_eq!(d.replace_all(&[], "x"), 0);
            assert_eq!(d.text(), "hello");
            assert!(!d.is_dirty());
        }

        /// A stale range from a search run before the last edit must not take
        /// text with it that it never described.
        #[test]
        fn a_range_past_the_end_is_skipped() {
            let mut d = Document::from_text("short");
            assert_eq!(d.replace_all(&[0..5, 40..44], "x"), 1);
            assert_eq!(d.text(), "x");
        }

        #[test]
        fn the_cursor_survives_the_document_getting_shorter() {
            let mut d = Document::from_text("leopard leopard");
            d.set_cursor(15);
            let found = hits(&d, "leopard");
            d.replace_all(&found, "cat");
            assert_eq!(d.text(), "cat cat");
            assert_eq!(d.cursor(), 7);
        }
    }

    mod selection {
        use super::*;

        /// Selecting leftwards and rightwards have to describe the same span,
        /// or every consumer of `selection()` needs its own ordering fix.
        #[test]
        fn a_selection_is_ordered_whichever_way_it_was_made() {
            let mut d = Document::from_text("hello");
            d.set_cursor(1);
            d.extend_to(4);
            assert_eq!(d.selection(), Some(1..4));
            d.set_cursor(4);
            d.extend_to(1);
            assert_eq!(d.selection(), Some(1..4));
            assert_eq!(d.selected_text().as_deref(), Some("ell"));
        }

        /// Shift-arrow out and back leaves the anchor sitting on the cursor.
        /// Reporting that as a selection would make the next keystroke replace
        /// an empty span instead of simply typing.
        #[test]
        fn an_anchor_on_the_cursor_is_not_a_selection() {
            let mut d = Document::from_text("hello");
            d.set_cursor(2);
            d.extend_to(3);
            d.extend_to(2);
            assert_eq!(d.selection(), None);
            assert_eq!(d.selected_text(), None);
        }

        #[test]
        fn typing_replaces_the_selection() {
            let mut d = Document::from_text("the quick fox");
            d.select(4..9);
            d.insert("slow");
            assert_eq!(d.text(), "the slow fox");
            assert_eq!(d.cursor(), 8);
            assert_eq!(d.selection(), None, "the selection is spent");
        }

        /// The invariant that made the anchor live in `Document`: a replacement
        /// is a delete and an insert, and it has to undo as the one thing the
        /// writer did.
        #[test]
        fn replacing_a_selection_undoes_in_one_step() {
            let mut d = Document::from_text("the quick fox");
            d.select(4..9);
            d.insert("slow");
            assert_eq!(d.text(), "the slow fox");
            d.undo();
            assert_eq!(d.text(), "the quick fox");
        }

        #[test]
        fn backspace_takes_the_selection_and_nothing_more() {
            let mut d = Document::from_text("abcdef");
            d.select(2..4);
            d.backspace();
            assert_eq!(d.text(), "abef");
            assert_eq!(d.cursor(), 2);
            // And the next backspace is an ordinary one again.
            d.backspace();
            assert_eq!(d.text(), "aef");
        }

        #[test]
        fn delete_takes_the_selection_and_nothing_more() {
            let mut d = Document::from_text("abcdef");
            d.select(2..4);
            d.delete();
            assert_eq!(d.text(), "abef");
        }

        /// What every text field does, and what stepping one character would
        /// get wrong: left means "put me before that".
        #[test]
        fn an_arrow_collapses_the_selection_to_its_near_edge() {
            let mut d = Document::from_text("hello world");
            d.select(6..11);
            d.move_left();
            assert_eq!(d.cursor(), 6);
            assert_eq!(d.selection(), None);

            d.select(6..11);
            d.move_right();
            assert_eq!(d.cursor(), 11);
        }

        #[test]
        fn a_plain_move_clears_the_selection_and_extending_keeps_it() {
            let mut d = Document::from_text("hello");
            d.select(0..3);
            d.set_cursor(4);
            assert_eq!(d.selection(), None);

            d.select(0..3);
            d.extend_right();
            assert_eq!(d.selection(), Some(0..4));
        }

        #[test]
        fn select_all_covers_the_document_including_cjk() {
            let mut d = Document::from_text("你好 world");
            d.select_all();
            assert_eq!(d.selection(), Some(0..8));
            assert_eq!(d.selected_text().as_deref(), Some("你好 world"));
            d.insert("x");
            assert_eq!(d.text(), "x");
        }

        #[test]
        fn undo_drops_the_selection_rather_than_leaving_a_stale_span() {
            let mut d = Document::from_text("abcdef");
            d.select(1..3);
            d.insert("X");
            d.undo();
            assert_eq!(d.text(), "abcdef");
            assert_eq!(d.selection(), None);
        }

        #[test]
        fn a_selection_past_the_end_is_clamped() {
            let mut d = Document::from_text("abc");
            d.select(1..99);
            assert_eq!(d.selection(), Some(1..3));
        }
    }

    mod words {
        use super::*;

        #[test]
        fn the_cursor_moves_a_word_at_a_time_in_both_directions() {
            let mut d = Document::from_text("the quick brown fox");
            d.set_cursor(0);
            d.move_word_right();
            assert_eq!(d.cursor(), 3);
            d.move_word_right();
            assert_eq!(d.cursor(), 9);
            d.move_word_left();
            assert_eq!(d.cursor(), 4);
        }

        #[test]
        fn extending_by_word_selects_it() {
            let mut d = Document::from_text("the quick brown fox");
            d.set_cursor(4);
            d.extend_word_right();
            assert_eq!(d.selected_text().as_deref(), Some("quick"));
        }

        #[test]
        fn deleting_a_word_back_takes_the_whole_word() {
            let mut d = Document::from_text("the quick brown");
            d.set_cursor(9);
            d.delete_word_back();
            assert_eq!(d.text(), "the  brown");
            assert_eq!(d.cursor(), 4);
        }

        #[test]
        fn deleting_a_word_forward_takes_the_whole_word() {
            let mut d = Document::from_text("the quick brown");
            d.set_cursor(4);
            d.delete_word_forward();
            assert_eq!(d.text(), "the  brown");
        }

        /// With something selected, "delete a word" means "delete that" — the
        /// writer is pointing at it.
        #[test]
        fn deleting_a_word_with_a_selection_takes_the_selection() {
            let mut d = Document::from_text("the quick brown");
            d.select(0..3);
            d.delete_word_back();
            assert_eq!(d.text(), " quick brown");
        }

        #[test]
        fn a_word_delete_undoes_in_one_step() {
            let mut d = Document::from_text("the quick brown");
            d.set_cursor(9);
            d.delete_word_back();
            d.undo();
            assert_eq!(d.text(), "the quick brown");
        }

        #[test]
        fn double_tapping_selects_the_word_under_the_finger() {
            let mut d = Document::from_text("the quick brown");
            d.select_word_at(6);
            assert_eq!(d.selected_text().as_deref(), Some("quick"));
        }

        /// The case the whole word module exists for: a Japanese draft has to
        /// break at the okurigana, not at the spaces it does not have.
        #[test]
        fn japanese_selects_by_run_rather_than_by_space() {
            let mut d = Document::from_text("私は書いた");
            d.select_word_at(2);
            assert_eq!(d.selected_text().as_deref(), Some("書"));
            d.set_cursor(2);
            d.extend_word_right();
            assert_eq!(d.selected_text().as_deref(), Some("書"));
            d.extend_word_right();
            assert_eq!(d.selected_text().as_deref(), Some("書いた"));
        }

        #[test]
        fn word_movement_stops_at_the_ends_rather_than_looping() {
            let mut d = Document::from_text("word");
            d.set_cursor(4);
            d.move_word_right();
            assert_eq!(d.cursor(), 4);
            d.set_cursor(0);
            d.move_word_left();
            assert_eq!(d.cursor(), 0);
            // And deleting at an end is a no-op rather than a panic.
            d.delete_word_back();
            assert_eq!(d.text(), "word");
        }
    }
}
