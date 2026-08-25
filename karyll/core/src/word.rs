//! Word boundaries: where one word ends and the next begins, for moving,
//! deleting and selecting by word.

use crate::dict::Dict;
use crate::segment;
use std::ops::Range;

/// A character's class for telling words apart — a third classification
/// beside [`crate::script`] (which face draws it) and `wrap::classify`
/// (where a line may break): 書いた is one run to both and breaks at 書|いた
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Alphanumerics outside the CJK ranges.
    Word,
    /// Han. A run of it is cut into words by [`crate::segment`] over a
    /// [`Dict`], or held whole with `None`.
    Han,
    Hiragana,
    Katakana,
    /// Spaces and tabs.
    Space,
    /// A line break, a kind of its own: a run of [`Kind::Space`] ends at it.
    Newline,
    /// Punctuation and symbols.
    Other,
}

impl Kind {
    /// Whether characters of this kind make up words.
    pub fn is_word(self) -> bool {
        matches!(
            self,
            Self::Word | Self::Han | Self::Hiragana | Self::Katakana
        )
    }
}

/// The kind of a character taken on its own. The CJK arms come before
/// `is_alphanumeric`, which is true of Han and kana.
fn kind_of(c: char) -> Kind {
    match c as u32 {
        0x3040..=0x309F => Kind::Hiragana,
        // The long-vowel mark ー sits in the katakana block; コーヒー is one
        // run.
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

/// The kind of the character at `i`, in the context of its neighbours: an
/// apostrophe between two [`Kind::Word`] characters is [`Kind::Word`], and
/// `don't` is one word. A hyphen is [`Kind::Other`], and `well-known` is two.
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

/// The maximal run of [`Kind::Han`] containing `i`, or `None` at any other
/// kind. Chinese punctuation is [`Kind::Other`] and bounds the run.
fn han_run(chars: &[char], i: usize) -> Option<Range<usize>> {
    if kind_at(chars, i) != Kind::Han {
        return None;
    }
    let mut start = i;
    while start > 0 && kind_at(chars, start - 1) == Kind::Han {
        start -= 1;
    }
    let mut end = i + 1;
    while end < chars.len() && kind_at(chars, end) == Kind::Han {
        end += 1;
    }
    Some(start..end)
}

/// The boundaries of the Han run around `i`, in indices into `chars`.
fn han_cuts(chars: &[char], i: usize, dict: Option<&Dict>) -> Option<(Range<usize>, Vec<usize>)> {
    let dict = dict?;
    let run = han_run(chars, i)?;
    let cuts = segment::cuts(&chars[run.clone()], dict);
    Some((run, cuts))
}

/// Where the word to the right of `idx` ends. Non-word characters are
/// skipped first: from the end of one word, the answer is the end of the
/// next.
pub fn word_end(chars: &[char], idx: usize, dict: Option<&Dict>) -> usize {
    let mut i = idx.min(chars.len());
    while i < chars.len() && !kind_at(chars, i).is_word() {
        i += 1;
    }
    if i == chars.len() {
        return i;
    }
    if let Some((run, cuts)) = han_cuts(chars, i, dict) {
        // `cuts` ends with the run's end: one boundary lies past any `i`
        // inside.
        if let Some(&cut) = cuts.iter().find(|&&cut| run.start + cut > i) {
            return run.start + cut;
        }
    }
    let kind = kind_at(chars, i);
    while i < chars.len() && kind_at(chars, i) == kind {
        i += 1;
    }
    i
}

/// Where the word to the left of `idx` begins.
pub fn word_start(chars: &[char], idx: usize, dict: Option<&Dict>) -> usize {
    let mut i = idx.min(chars.len());
    while i > 0 && !kind_at(chars, i - 1).is_word() {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    // `cuts` opens with the run's start: one boundary lies before any `i`
    // inside.
    if let Some((run, cuts)) = han_cuts(chars, i - 1, dict)
        && let Some(&cut) = cuts.iter().rev().find(|&&cut| run.start + cut < i)
    {
        return run.start + cut;
    }
    let kind = kind_at(chars, i - 1);
    while i > 0 && kind_at(chars, i - 1) == kind {
        i -= 1;
    }
    i
}

/// The run containing `idx` — what a double-tap selects. No skipping: at a
/// space, the run of spaces. An index past the end clamps to the last
/// character.
pub fn word_at(chars: &[char], idx: usize, dict: Option<&Dict>) -> Range<usize> {
    if chars.is_empty() {
        return 0..0;
    }
    let i = idx.min(chars.len() - 1);
    if let Some((run, cuts)) = han_cuts(chars, i, dict) {
        let at = i - run.start;
        if let Some(word) = cuts.windows(2).find(|w| at < w[1]) {
            return run.start + word[0]..run.start + word[1];
        }
    }
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
    use crate::dict::{Layout, fixture};

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    // Wrappers passing `None`; the dictionary tests call the `super::`
    // functions directly.
    fn word_end(chars: &[char], idx: usize) -> usize {
        super::word_end(chars, idx, None)
    }

    fn word_start(chars: &[char], idx: usize) -> usize {
        super::word_start(chars, idx, None)
    }

    fn word_at(chars: &[char], idx: usize) -> Range<usize> {
        super::word_at(chars, idx, None)
    }

    fn dict(words: &[&str]) -> Dict {
        let pairs: Vec<(&str, u16)> = words.iter().map(|w| (*w, 0u16)).collect();
        Dict::parse(fixture::image(&pairs, Layout::Mmseg), Layout::Mmseg).expect("parses")
    }

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
        // 20, the buffer's end, is a fixed point.
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

    /// One run to `script_of`, two words here: 書いた breaks at 書|いた.
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
        // 私 | は | コーヒー | を | 飲 | む — the katakana run holds across ー.
        assert_eq!(word_at(&c, 3), 2..6);
    }

    /// A Han run with no dictionary is one unit.
    #[test]
    fn chinese_moves_a_whole_run_at_a_time_with_no_dictionary() {
        let c = chars("今天天氣很好");
        assert_eq!(word_end(&c, 0), 6);
        assert_eq!(word_start(&c, 6), 0);
        assert_eq!(word_at(&c, 3), 0..6);
    }

    #[test]
    fn a_dictionary_cuts_the_same_run_into_words() {
        let d = dict(&["今天", "天气", "很", "好"]);
        let c = chars("今天天气很好");
        assert_eq!(super::word_at(&c, 0, Some(&d)), 0..2, "今天");
        assert_eq!(super::word_at(&c, 3, Some(&d)), 2..4, "天气");
        assert_eq!(super::word_at(&c, 5, Some(&d)), 5..6, "好");
    }

    #[test]
    fn word_movement_follows_the_dictionary_through_a_run() {
        let d = dict(&["今天", "天气", "很", "好"]);
        let c = chars("今天天气很好");
        let mut at = 0;
        let mut stops = vec![];
        while at < c.len() {
            at = super::word_end(&c, at, Some(&d));
            stops.push(at);
        }
        assert_eq!(stops, vec![2, 4, 5, 6]);

        let mut at = c.len();
        let mut back = vec![];
        while at > 0 {
            at = super::word_start(&c, at, Some(&d));
            back.push(at);
        }
        assert_eq!(back, vec![5, 4, 2, 0], "the mirror of the walk forward");
    }

    /// The comma bounds the runs; each side is segmented apart.
    #[test]
    fn punctuation_ends_the_run_the_dictionary_sees() {
        let d = dict(&["今天", "很好"]);
        let c = chars("今天，很好");
        assert_eq!(super::word_at(&c, 1, Some(&d)), 0..2);
        assert_eq!(
            super::word_at(&c, 2, Some(&d)),
            2..3,
            "the comma is its own run"
        );
        assert_eq!(super::word_at(&c, 3, Some(&d)), 3..5);
        assert_eq!(super::word_end(&c, 0, Some(&d)), 2);
        assert_eq!(super::word_end(&c, 2, Some(&d)), 5, "over the comma");
    }

    /// The dictionary cuts kanji runs; the kana kinds stand apart from it.
    #[test]
    fn japanese_kanji_runs_are_cut_and_kana_is_not() {
        let d = dict(&["東京", "都庁", "書"]);
        let c = chars("東京都庁");
        assert_eq!(super::word_at(&c, 0, Some(&d)), 0..2);
        assert_eq!(super::word_at(&c, 2, Some(&d)), 2..4);

        let c = chars("書いた");
        assert_eq!(super::word_at(&c, 0, Some(&d)), 0..1, "書 alone, as before");
        assert_eq!(
            super::word_at(&c, 1, Some(&d)),
            1..3,
            "いた, untouched by the list"
        );
    }

    /// Characters absent from the word list cut one at a time.
    #[test]
    fn an_unhelpful_dictionary_falls_back_to_one_character_at_a_time() {
        let d = dict(&["東京"]);
        let c = chars("今天天气");
        assert_eq!(super::word_at(&c, 0, Some(&d)), 0..1);
        assert_eq!(super::word_end(&c, 0, Some(&d)), 1);
        assert_eq!(super::word_start(&c, 4, Some(&d)), 3);
    }

    #[test]
    fn the_ends_of_a_han_run_are_fixed_points_with_a_dictionary_too() {
        let d = dict(&["今天", "天气"]);
        let c = chars("今天天气");
        assert_eq!(super::word_end(&c, 4, Some(&d)), 4);
        assert_eq!(super::word_start(&c, 0, Some(&d)), 0);
        assert_eq!(super::word_at(&c, 99, Some(&d)), 2..4);
        assert_eq!(super::word_at(&[], 0, Some(&d)), 0..0);
    }

    #[test]
    fn punctuation_between_scripts_is_skipped_not_entered() {
        let c = chars("你好，world");
        assert_eq!(word_end(&c, 0), 2, "你好");
        assert_eq!(word_end(&c, 2), 8, "over the ， and on to the end of world");
    }

    #[test]
    fn an_apostrophe_inside_a_word_belongs_to_it() {
        let c = chars("don't stop");
        assert_eq!(word_end(&c, 0), 5);
        assert_eq!(word_at(&c, 2), 0..5);
        // The typographic apostrophe too.
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
        // An index past the end clamps.
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

    #[test]
    fn whitespace_at_the_end_terminates_the_walk() {
        let c = chars("word   ");
        assert_eq!(word_end(&c, 4), 7);
        assert_eq!(word_end(&c, 7), 7);
    }
}
