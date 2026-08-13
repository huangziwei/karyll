//! Where one word ends and the next begins.
//!
//! Four things want this and none of them existed before: moving by word,
//! deleting by word, selecting a word by double-tap, and wrapping the word
//! under the cursor in emphasis.
//!
//! **This is the third character classification in this crate, and that is
//! deliberate.** Each answers a different question and merging any two would be
//! wrong:
//!
//! - `script::script_of` asks *which face draws this*, so it puts kana and
//!   kanji together — they come from the same file.
//! - `wrap::classify` asks *may a line break here*, so it puts kana and kanji
//!   together again — both break between characters.
//! - This asks *is this the same word as its neighbour*, and the answer is no:
//!   書いた has to break at 書|いた, which is where the okurigana starts and
//!   very often a real morpheme boundary.
//!
//! **Chinese is coarse, and this is the reason.** A run of Han with no kana in
//! it is one unit, so word movement in Chinese jumps a whole clause. Real
//! segmentation needs a word list *and* an algorithm that consults it, and this
//! crate is dependency-free by design.
//!
//! The device does have word lists — an earlier note here claimed it did not,
//! and blamed memory, and both were wrong. What it has is the wrong shape:
//!
//! - **ICU 65 is on the device but its data is stubbed.** `libicuuc` is 1.5 MB
//!   of real code, and `libicudata.so.65.1` is **4,756 bytes** — ICU's stub
//!   data library. So `BreakIterator`, which is exactly this job and ships a
//!   Chinese/Japanese segmentation dictionary, has no dictionary to open.
//! - **XT9's `zh_CN.ldb` is 1.4 MB of Chinese words**, already resident because
//!   karyll drives that engine for pinyin input. But its API is
//!   pinyin → candidates (`ET9CPBuildSelectionList`, `ET9CPGetPhrase`); nothing
//!   exported asks "where does this word end". Using the list would mean
//!   reading a proprietary format.
//!
//! So the honest position is not "impossible" but "not reachable from here
//! without work nobody has costed". A coarse jump is still far better than
//! moving one character at a time, which is what there was.

use std::ops::Range;

/// What kind of character this is, for the purpose of telling words apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Letters and digits from an alphabet — Latin, and anything else that is
    /// alphanumeric without being CJK.
    Word,
    /// Han. One run is one unit; see the note above about dictionaries.
    Han,
    Hiragana,
    Katakana,
    /// Spaces and tabs.
    Space,
    /// A line break, kept apart from other whitespace so that selecting a run
    /// of blank space cannot swallow a paragraph break.
    Newline,
    /// Punctuation and symbols — skipped over rather than moved through.
    Other,
}

impl Kind {
    /// Whether characters of this kind make up words, as opposed to separating
    /// them.
    pub fn is_word(self) -> bool {
        matches!(
            self,
            Self::Word | Self::Han | Self::Hiragana | Self::Katakana
        )
    }
}

/// The kind of a character taken on its own.
///
/// The CJK ranges are tested before `is_alphanumeric`, which would otherwise
/// claim all of them — Han and kana are alphabetic as far as Unicode is
/// concerned, and this is the one place that has to disagree.
fn kind_of(c: char) -> Kind {
    match c as u32 {
        0x3040..=0x309F => Kind::Hiragana,
        // The long-vowel mark ー lives in the katakana block and is used by
        // both kana, so コーヒー stays one word.
        0x30A0..=0x30FF | 0xFF66..=0xFF9D => Kind::Katakana,
        0x2E80..=0x2FDF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0x20000..=0x3FFFF => Kind::Han,
        _ if c == '\n' || c == '\r' => Kind::Newline,
        _ if c.is_whitespace() => Kind::Space,
        _ if c.is_alphanumeric() => Kind::Word,
        _ => Kind::Other,
    }
}

/// The kind of the character at `i`, in the context of its neighbours.
///
/// Context is needed for exactly one case: an apostrophe between two letters is
/// part of the word. Without it "don't" is three words and reaching the end of
/// it takes two presses, which is not what any editor a writer has used does.
/// A hyphen deliberately does not get the same treatment — "well-known" reads
/// as two words, and German compounds lean on that harder than English does.
pub fn kind_at(chars: &[char], i: usize) -> Kind {
    let Some(&c) = chars.get(i) else {
        return Kind::Space;
    };
    let kind = kind_of(c);
    if kind == Kind::Other
        && matches!(c, '\'' | '\u{2019}')
        && i > 0
        && kind_of(chars[i - 1]) == Kind::Word
        && chars.get(i + 1).copied().map(kind_of) == Some(Kind::Word)
    {
        return Kind::Word;
    }
    kind
}

/// Where the word to the right of `idx` ends.
///
/// Anything that is not part of a word is skipped first, so pressing this at
/// the end of one word lands at the end of the next rather than at the start of
/// the space between them. That is what Word, Pages and a browser text field
/// all do.
pub fn word_end(chars: &[char], idx: usize) -> usize {
    let mut i = idx.min(chars.len());
    while i < chars.len() && !kind_at(chars, i).is_word() {
        i += 1;
    }
    if i == chars.len() {
        return i;
    }
    let kind = kind_at(chars, i);
    while i < chars.len() && kind_at(chars, i) == kind {
        i += 1;
    }
    i
}

/// Where the word to the left of `idx` begins.
pub fn word_start(chars: &[char], idx: usize) -> usize {
    let mut i = idx.min(chars.len());
    while i > 0 && !kind_at(chars, i - 1).is_word() {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    let kind = kind_at(chars, i - 1);
    while i > 0 && kind_at(chars, i - 1) == kind {
        i -= 1;
    }
    i
}

/// The run containing `idx` — what a double-tap selects.
///
/// Unlike the two above this does not skip: tapping a space selects the run of
/// spaces, which is what a double-click does everywhere else. An index past the
/// end selects the last run rather than nothing, because a tap beyond the final
/// character is a tap on that character as far as the writer is concerned.
pub fn word_at(chars: &[char], idx: usize) -> Range<usize> {
    if chars.is_empty() {
        return 0..0;
    }
    let i = idx.min(chars.len() - 1);
    let kind = kind_at(chars, i);
    let mut start = i;
    while start > 0 && kind_at(chars, start - 1) == kind {
        start -= 1;
    }
    let mut end = i + 1;
    while end < chars.len() && kind_at(chars, end) == kind {
        end += 1;
    }
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// Walking right by word across a sentence has to land where a writer
    /// expects at every step, so this checks the whole walk rather than one
    /// jump.
    #[test]
    fn walking_english_prose_by_word() {
        let c = chars("The quick brown fox.");
        let mut at = 0;
        let mut stops = vec![];
        while at < c.len() {
            at = word_end(&c, at);
            stops.push(at);
        }
        assert_eq!(stops, vec![3, 9, 15, 19, 20]);
        // The trailing 20 is the full stop: the walk stops at the end of the
        // buffer rather than looping on a word that is not there.
        assert_eq!(word_end(&c, 20), 20);
    }

    #[test]
    fn walking_back_is_the_mirror_of_walking_forward() {
        let c = chars("The quick brown fox.");
        let mut at = c.len();
        let mut stops = vec![];
        while at > 0 {
            at = word_start(&c, at);
            stops.push(at);
        }
        assert_eq!(stops, vec![16, 10, 4, 0]);
    }

    /// The reason this module exists rather than reusing `script_of`: kanji and
    /// the okurigana after it are one run to a font and two words to a writer.
    #[test]
    fn japanese_breaks_between_kanji_and_kana() {
        let c = chars("書いた");
        assert_eq!(word_end(&c, 0), 1, "書 alone");
        assert_eq!(word_end(&c, 1), 3, "いた");
        assert_eq!(word_start(&c, 3), 1);
        assert_eq!(word_start(&c, 1), 0);
    }

    #[test]
    fn katakana_is_its_own_run_and_keeps_its_long_vowel() {
        let c = chars("私はコーヒーを飲む");
        // 私 | は | コーヒー | を | 飲 | む — the katakana run holds together
        // across ー, which lives in the katakana block for exactly this reason.
        assert_eq!(word_at(&c, 3), 2..6);
    }

    /// Stated coarseness, pinned so that nobody later reads it as a bug: with
    /// no kana to break on, a Chinese clause is one unit.
    #[test]
    fn chinese_moves_a_whole_run_at_a_time() {
        let c = chars("今天天氣很好");
        assert_eq!(word_end(&c, 0), 6);
        assert_eq!(word_start(&c, 6), 0);
    }

    #[test]
    fn punctuation_between_scripts_is_skipped_not_entered() {
        let c = chars("你好，world");
        assert_eq!(word_end(&c, 0), 2, "你好");
        assert_eq!(word_end(&c, 2), 8, "over the ， and on to the end of world");
    }

    /// "don't" is one word in every editor a writer has used, and three runs to
    /// a naive classifier.
    #[test]
    fn an_apostrophe_inside_a_word_belongs_to_it() {
        let c = chars("don't stop");
        assert_eq!(word_end(&c, 0), 5);
        assert_eq!(word_at(&c, 2), 0..5);
        // The typographic apostrophe too, since the Chinese punctuation table
        // is not the only thing that produces one.
        let curly = chars("don\u{2019}t");
        assert_eq!(word_at(&curly, 0), 0..5);
    }

    /// A quote mark is not an apostrophe, and the difference is the neighbours.
    #[test]
    fn an_apostrophe_that_is_a_quote_is_not_part_of_a_word() {
        let c = chars("'quoted'");
        assert_eq!(word_at(&c, 1), 1..7, "just the letters");
        assert_eq!(word_start(&c, 8), 1);
    }

    #[test]
    fn a_double_tap_on_blank_space_stops_at_the_line_break() {
        let c = chars("a  \n  b");
        // The two spaces before the newline, and not the ones after it.
        assert_eq!(word_at(&c, 1), 1..3);
        assert_eq!(word_at(&c, 3), 3..4, "the newline is its own run");
    }

    #[test]
    fn word_movement_crosses_lines_the_way_it_crosses_spaces() {
        let c = chars("one\ntwo");
        assert_eq!(word_end(&c, 3), 7);
        assert_eq!(word_start(&c, 4), 0);
    }

    #[test]
    fn the_ends_of_the_buffer_are_fixed_points() {
        let c = chars("word");
        assert_eq!(word_end(&c, 4), 4);
        assert_eq!(word_start(&c, 0), 0);
        // And an index past the end is clamped rather than panicking, because
        // a tap can land anywhere.
        assert_eq!(word_end(&c, 99), 4);
        assert_eq!(word_start(&c, 99), 0);
        assert_eq!(word_at(&c, 99), 0..4);
    }

    #[test]
    fn an_empty_buffer_has_no_word() {
        assert_eq!(word_at(&[], 0), 0..0);
        assert_eq!(word_end(&[], 0), 0);
        assert_eq!(word_start(&[], 0), 0);
    }

    /// Trailing whitespace has no word after it, and the walk must stop rather
    /// than sit still forever.
    #[test]
    fn whitespace_at_the_end_terminates_the_walk() {
        let c = chars("word   ");
        assert_eq!(word_end(&c, 4), 7);
        assert_eq!(word_end(&c, 7), 7);
    }
}
