//! Finding a phrase in the document.
//!
//! **Over characters, never over bytes.** The buffer is a `Vec<char>` and every
//! index in this editor — the cursor, a selection, a wrap point, a focused
//! sentence — counts characters. A search that worked on a UTF-8 string and
//! handed back byte offsets would land three glyphs early on the first Han
//! character before the match, and the further into a Chinese paragraph the
//! worse it would get.
//!
//! **Every match at once, on every keystroke.** A document long enough for this
//! to matter is a few tens of thousands of characters, which is microseconds to
//! scan, and knowing how many there are is half of what a search bar is for —
//! "3 of 12" tells a writer whether the word they think they overuse is a
//! problem, and one match at a time never can.

use std::ops::Range;

/// Case-folded for comparison.
///
/// **Per character, and that is a constraint rather than a shortcut.** A match
/// comes back as a range of *the haystack's* indices, so a fold that changed
/// the length would put the highlight on the wrong glyph — the one thing this
/// module must not do. `char::to_lowercase` yields more than one character for
/// a handful of code points (İ is the common one), and taking the first keeps
/// the indices honest.
///
/// It also means German `ß` does not match `SS`, which is the same answer every
/// editor's plain find gives.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Every place `needle` occurs in `haystack`, in order and not overlapping.
///
/// Not overlapping, so `aa` in `aaa` is one match and not two, which is what
/// stepping through hits with Enter has to mean if the step is to terminate.
///
/// An empty needle matches nothing rather than everything: the search bar is
/// empty before a word is typed into it, and highlighting every position in the
/// document at that moment would be a strange greeting.
pub fn matches(haystack: &[char], needle: &[char]) -> Vec<Range<usize>> {
    let needle: Vec<char> = needle.iter().copied().map(fold).collect();
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut at = 0;
    while at + needle.len() <= haystack.len() {
        let hit = haystack[at..at + needle.len()]
            .iter()
            .map(|c| fold(*c))
            .eq(needle.iter().copied());
        if hit {
            out.push(at..at + needle.len());
            at += needle.len();
        } else {
            at += 1;
        }
    }
    out
}

/// The first match at or after `cursor` — where a fresh search lands.
///
/// **At**, not after: the writer has not moved yet, so a match starting exactly
/// where they are is the nearest one and skipping it would look like the search
/// had missed it.
pub fn from(matches: &[Range<usize>], cursor: usize) -> Option<usize> {
    (!matches.is_empty()).then(|| {
        matches
            .iter()
            .position(|m| m.start >= cursor)
            // Past the last one, so round the end of the document. A search
            // that stops at the bottom makes the writer guess whether there is
            // nothing more or nothing at all.
            .unwrap_or(0)
    })
}

/// The next match after `cursor`, wrapping. What Enter does.
///
/// **Strictly after**, which is the difference from [`from`]: arriving at a
/// match leaves the cursor inside it, and Enter has to move on rather than
/// find the same one again.
pub fn next(matches: &[Range<usize>], cursor: usize) -> Option<usize> {
    (!matches.is_empty()).then(|| matches.iter().position(|m| m.start > cursor).unwrap_or(0))
}

/// The previous match before `cursor`, wrapping. What Shift+Enter does.
pub fn previous(matches: &[Range<usize>], cursor: usize) -> Option<usize> {
    (!matches.is_empty()).then(|| {
        matches
            .iter()
            .rposition(|m| m.start < cursor)
            .unwrap_or(matches.len() - 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(haystack: &str, needle: &str) -> Vec<Range<usize>> {
        let h: Vec<char> = haystack.chars().collect();
        let n: Vec<char> = needle.chars().collect();
        matches(&h, &n)
    }

    #[test]
    fn a_phrase_is_found_wherever_it_occurs() {
        assert_eq!(find("the cat sat on the mat", "the"), vec![0..3, 15..18]);
        assert_eq!(find("aaa", "b"), vec![]);
        assert_eq!(find("", "a"), vec![]);
    }

    /// Or stepping through hits with Enter would never terminate on a run.
    #[test]
    fn matches_do_not_overlap() {
        assert_eq!(find("aaa", "aa"), vec![0..2]);
        assert_eq!(find("aaaa", "aa"), vec![0..2, 2..4]);
    }

    #[test]
    fn an_empty_search_matches_nothing_rather_than_everywhere() {
        assert_eq!(find("some prose", ""), vec![]);
    }

    #[test]
    fn a_needle_longer_than_the_haystack_is_not_in_it() {
        assert_eq!(find("ab", "abc"), vec![]);
    }

    #[test]
    fn case_is_ignored() {
        assert_eq!(find("The THE the", "the"), vec![0..3, 4..7, 8..11]);
        assert_eq!(find("the", "THE"), vec![0..3]);
        // German, which this writer uses: the umlauts fold like anything else.
        assert_eq!(find("Ärger und ärger", "ärger"), vec![0..5, 10..15]);
    }

    /// The reason this counts characters. A byte-oriented search would report
    /// 6 for the second hit here, and the highlight would land in the middle of
    /// a glyph three characters early.
    #[test]
    fn indices_are_characters_so_a_han_match_lands_on_its_own_glyph() {
        assert_eq!(find("你好世界你好", "你好"), vec![0..2, 4..6]);
        assert_eq!(find("中文と日本語", "日本語"), vec![3..6]);
        // Mixed, which is the case the whole editor is built around.
        assert_eq!(find("他说hello世界hello", "hello"), vec![2..7, 9..14]);
    }

    #[test]
    fn a_fresh_search_lands_on_the_match_the_cursor_is_already_at() {
        let hits = vec![0..3, 15..18];
        assert_eq!(from(&hits, 0), Some(0), "at the cursor counts");
        assert_eq!(from(&hits, 1), Some(1));
        assert_eq!(from(&hits, 15), Some(1));
        assert_eq!(from(&hits, 16), Some(0), "past the last, round the end");
        assert_eq!(from(&[], 0), None);
    }

    /// Enter moves on. Arriving at a match leaves the cursor in it, so "at the
    /// cursor" would find the same one for ever.
    #[test]
    fn enter_steps_past_the_match_it_is_on_and_wraps() {
        let hits = vec![0..3, 15..18, 40..43];
        assert_eq!(next(&hits, 0), Some(1));
        assert_eq!(next(&hits, 15), Some(2));
        assert_eq!(next(&hits, 40), Some(0), "wraps to the top");
        assert_eq!(next(&[], 7), None);
    }

    #[test]
    fn shift_enter_steps_back_and_wraps_the_other_way() {
        let hits = vec![0..3, 15..18, 40..43];
        assert_eq!(previous(&hits, 40), Some(1));
        assert_eq!(previous(&hits, 15), Some(0));
        assert_eq!(previous(&hits, 0), Some(2), "wraps to the bottom");
        assert_eq!(previous(&[], 7), None);
    }

    /// Enter and Shift+Enter from the same place are inverses, which is what
    /// makes overshooting recoverable rather than a reason to start again.
    #[test]
    fn the_two_directions_undo_each_other() {
        let hits = vec![0..3, 15..18, 40..43];
        for at in 0..hits.len() {
            let forward = next(&hits, hits[at].start).expect("a match");
            assert_eq!(previous(&hits, hits[forward].start), Some(at));
        }
    }
}
