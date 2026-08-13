//! A gap buffer holding `char`s.
//!
//! Prose editing concentrates edits at the cursor, which is exactly the access
//! pattern a gap buffer is good at: moving the gap costs a memcpy proportional
//! to how far the cursor jumped, and typing at it is free.
//!
//! Storing `char` rather than UTF-8 bytes trades memory for the absence of a
//! whole class of boundary bug — every index is a character index, so nothing
//! can land mid-codepoint. Four bytes per character is affordable here: a long
//! Chinese manuscript of 200k characters costs 800 KB against the ~514 MB the
//! device has free, and UTF-8 would have cost 600 KB of that anyway.

/// Minimum gap size, and the amount of headroom left after a grow. Sized so a
/// burst of typing does not reallocate on every keystroke.
const MIN_GAP: usize = 64;

#[derive(Clone)]
pub struct Buffer {
    /// Characters before the gap, then `gap_end - gap_start` unused slots, then
    /// the characters after it. Slots inside the gap hold stale values and are
    /// never read.
    buf: Vec<char>,
    gap_start: usize,
    gap_end: usize,
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            buf: vec!['\0'; MIN_GAP],
            gap_start: 0,
            gap_end: MIN_GAP,
        }
    }

    pub fn from_text(s: &str) -> Self {
        let mut buf: Vec<char> = s.chars().collect();
        let gap_start = buf.len();
        buf.resize(gap_start + MIN_GAP, '\0');
        Self {
            buf,
            gap_start,
            gap_end: gap_start + MIN_GAP,
        }
    }

    pub fn len(&self) -> usize {
        self.buf.len() - (self.gap_end - self.gap_start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The character at `idx`, or `None` past the end.
    pub fn char_at(&self, idx: usize) -> Option<char> {
        if idx >= self.len() {
            return None;
        }
        Some(self.buf[self.raw(idx)])
    }

    /// Map a character index onto its slot, stepping over the gap.
    fn raw(&self, idx: usize) -> usize {
        if idx < self.gap_start {
            idx
        } else {
            idx + (self.gap_end - self.gap_start)
        }
    }

    /// Place the gap at `idx`, so that inserting there is a no-copy write.
    fn move_gap_to(&mut self, idx: usize) {
        debug_assert!(idx <= self.len());
        if idx < self.gap_start {
            // Shift the run [idx, gap_start) up to sit after the gap.
            let n = self.gap_start - idx;
            self.buf.copy_within(idx..self.gap_start, self.gap_end - n);
            self.gap_start -= n;
            self.gap_end -= n;
        } else if idx > self.gap_start {
            // Shift the run after the gap down into it.
            let n = idx - self.gap_start;
            self.buf
                .copy_within(self.gap_end..self.gap_end + n, self.gap_start);
            self.gap_start += n;
            self.gap_end += n;
        }
    }

    /// Ensure the gap can take `n` more characters.
    fn reserve(&mut self, n: usize) {
        let gap = self.gap_end - self.gap_start;
        if gap >= n {
            return;
        }
        let grow = n - gap + MIN_GAP;
        self.buf.splice(
            self.gap_start..self.gap_start,
            std::iter::repeat_n('\0', grow),
        );
        self.gap_end += grow;
    }

    /// Insert `text` at character index `idx`.
    pub fn insert(&mut self, idx: usize, text: &[char]) {
        assert!(idx <= self.len(), "insert past end of buffer");
        if text.is_empty() {
            return;
        }
        self.move_gap_to(idx);
        self.reserve(text.len());
        self.buf[self.gap_start..self.gap_start + text.len()].copy_from_slice(text);
        self.gap_start += text.len();
    }

    /// Remove `len` characters starting at `idx` and return them.
    pub fn remove(&mut self, idx: usize, len: usize) -> Vec<char> {
        assert!(idx + len <= self.len(), "remove past end of buffer");
        if len == 0 {
            return Vec::new();
        }
        self.move_gap_to(idx);
        let taken = self.buf[self.gap_end..self.gap_end + len].to_vec();
        self.gap_end += len;
        taken
    }

    pub fn chars(&self) -> impl Iterator<Item = char> + '_ {
        self.buf[..self.gap_start]
            .iter()
            .chain(&self.buf[self.gap_end..])
            .copied()
    }

    /// Characters in `range`, which must lie within the buffer.
    pub fn slice(&self, range: std::ops::Range<usize>) -> Vec<char> {
        assert!(range.end <= self.len(), "slice past end of buffer");
        (range).map(|i| self.buf[self.raw(i)]).collect()
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.chars()
            .try_for_each(|c| f.write_str(c.encode_utf8(&mut [0u8; 4])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(b: &Buffer) -> String {
        b.chars().collect()
    }

    #[test]
    fn empty_buffer() {
        let b = Buffer::new();
        assert_eq!(b.len(), 0);
        assert!(b.is_empty());
        assert_eq!(b.char_at(0), None);
        assert_eq!(text(&b), "");
    }

    #[test]
    fn insert_at_end_then_middle_then_front() {
        let mut b = Buffer::new();
        b.insert(0, &['h', 'i']);
        assert_eq!(text(&b), "hi");
        b.insert(1, &['e', 'y']);
        assert_eq!(text(&b), "heyi");
        b.insert(0, &['!']);
        assert_eq!(text(&b), "!heyi");
        assert_eq!(b.len(), 5);
    }

    #[test]
    fn remove_returns_what_it_took() {
        let mut b = Buffer::from_text("hello world");
        assert_eq!(b.remove(5, 6), " world".chars().collect::<Vec<_>>());
        assert_eq!(text(&b), "hello");
        assert_eq!(b.remove(0, 5), "hello".chars().collect::<Vec<_>>());
        assert!(b.is_empty());
    }

    #[test]
    fn char_at_is_stable_across_gap_moves() {
        let mut b = Buffer::from_text("abcdef");
        // Force the gap to several positions; indices must not shift.
        for probe in [4usize, 0, 6, 2] {
            b.insert(probe, &[]);
            let seen: String = (0..b.len()).filter_map(|i| b.char_at(i)).collect();
            assert_eq!(seen, "abcdef", "after gap move to {probe}");
        }
    }

    #[test]
    fn cjk_is_one_index_per_character() {
        let mut b = Buffer::from_text("你好");
        assert_eq!(b.len(), 2);
        assert_eq!(b.char_at(0), Some('你'));
        b.insert(1, &['，']);
        assert_eq!(text(&b), "你，好");
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn slice_spans_the_gap() {
        let mut b = Buffer::from_text("abcdef");
        b.insert(3, &['X']);
        assert_eq!(b.slice(0..7).iter().collect::<String>(), "abcXdef");
        assert_eq!(b.slice(2..5).iter().collect::<String>(), "cXd");
    }

    #[test]
    fn growth_past_the_initial_gap() {
        let mut b = Buffer::new();
        let long: Vec<char> = std::iter::repeat_n('x', MIN_GAP * 3).collect();
        b.insert(0, &long);
        assert_eq!(b.len(), MIN_GAP * 3);
        b.insert(MIN_GAP, &['y']);
        assert_eq!(b.char_at(MIN_GAP), Some('y'));
        assert_eq!(b.len(), MIN_GAP * 3 + 1);
    }

    #[test]
    fn interleaved_edits_match_a_plain_string() {
        // Cross-check the gap arithmetic against an obviously-correct model.
        let mut b = Buffer::new();
        let mut model: Vec<char> = Vec::new();
        let script: &[(usize, &str, usize)] = &[
            (0, "hello", 0),
            (5, " 世界", 0),
            (2, "XY", 0),
            (0, "", 3),
            (4, "zz", 2),
        ];
        for &(at, ins, del) in script {
            let ins: Vec<char> = ins.chars().collect();
            b.insert(at, &ins);
            model.splice(at..at, ins.iter().copied());
            b.remove(at, del);
            model.drain(at..at + del);
            assert_eq!(text(&b), model.iter().collect::<String>());
        }
    }
}
