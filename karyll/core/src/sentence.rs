//! Where one sentence ends and the next begins; focus mode is the only caller.
//! **A sentence never crosses a newline**, so a heading, a list item and a
//! fenced line are each one and nothing here knows what a block is.

use std::ops::Range;

/// The sentence containing `idx`, as character indices. A cursor in the gap
/// after a full stop belongs to the sentence that just ended, or typing the `.`
/// would dim the page for one keystroke and light it again on the next.
pub fn sentence_at(chars: &[char], idx: usize) -> Range<usize> {
    let idx = idx.min(chars.len());

    let mut back = idx;
    while back > 0 && is_gap(chars[back - 1]) {
        back -= 1;
    }
    if back > 0 && terminates(chars, back - 1) {
        let end = absorb(chars, back - 1);
        return start_of(chars, back - 1)..end.max(back);
    }

    start_of(chars, idx)..end_of(chars, idx)
}

/// Space that separates two sentences. A newline is excluded because it is a
/// boundary in its own right, not a gap between things on either side of one.
fn is_gap(c: char) -> bool {
    c != '\n' && c.is_whitespace()
}

/// Where the sentence containing `idx` begins.
fn start_of(chars: &[char], idx: usize) -> usize {
    let mut i = idx.min(chars.len());
    while i > 0 {
        let prev = i - 1;
        if chars[prev] == '\n' || terminates(chars, prev) {
            break;
        }
        i -= 1;
    }
    // The space after the previous sentence's full stop belongs to neither, so
    // it is left out rather than made to lead this one.
    while i < idx && is_gap(chars[i]) {
        i += 1;
    }
    i
}

/// Where the sentence containing `idx` ends, one past its last character.
fn end_of(chars: &[char], idx: usize) -> usize {
    let mut i = idx.min(chars.len());
    while i < chars.len() {
        if chars[i] == '\n' {
            return i;
        }
        if terminates(chars, i) {
            return absorb(chars, i);
        }
        i += 1;
    }
    chars.len()
}

/// Take in whatever rides along with the mark at `i` — a second mark in
/// `What?!`, the closing quote in `he said "Go."` — which would otherwise open
/// the next sentence as one solid character in a dimmed paragraph.
fn absorb(chars: &[char], i: usize) -> usize {
    let mut end = i + 1;
    while let Some(&c) = chars.get(end) {
        if is_closing(c) || terminates(chars, end) {
            end += 1;
        } else {
            break;
        }
    }
    end
}

fn is_closing(c: char) -> bool {
    matches!(
        c,
        '"' | '\'' | '\u{2019}' | '\u{201D}' | ')' | ']' | '»' | '」' | '』' | '）' | '】'
    )
}

/// Whether the character at `i` ends a sentence.
fn terminates(chars: &[char], i: usize) -> bool {
    let Some(&c) = chars.get(i) else {
        return false;
    };
    match c {
        '。' | '！' | '？' | '…' | '‼' | '⁉' | '⁈' | '⁇' => true,
        '!' | '?' => true,
        '.' => is_full_stop(chars, i),
        _ => false,
    }
}

/// Whether a `.` ends a sentence: not between digits (`3.5`), not after a lone
/// letter (`e.g.`, `z.B.`), not before a lowercase word (`etc. and so on`).
/// **Known miss: `Mr. Smith`**, which would need a list of titles per language.
fn is_full_stop(chars: &[char], i: usize) -> bool {
    let before = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
    let after = chars.get(i + 1).copied();

    if before.is_some_and(|c| c.is_ascii_digit()) && after.is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    if before.is_some_and(char::is_alphabetic) && i >= 2 && !chars[i - 2].is_alphabetic() {
        return false;
    }
    match chars[i + 1..].iter().find(|c| !is_gap(**c)) {
        Some(&c) => !c.is_lowercase(),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, idx: usize) -> String {
        let chars: Vec<char> = text.chars().collect();
        chars[sentence_at(&chars, idx)].iter().collect()
    }

    #[test]
    fn a_document_of_one_sentence_is_that_sentence() {
        assert_eq!(at("Hello there.", 3), "Hello there.");
    }

    #[test]
    fn the_cursor_picks_out_which_of_several_it_is_in() {
        let text = "One here. Two there. Three last.";
        assert_eq!(at(text, 2), "One here.");
        assert_eq!(at(text, 13), "Two there.");
        assert_eq!(at(text, 25), "Three last.");
    }

    #[test]
    fn a_cursor_in_the_gap_belongs_to_the_sentence_that_just_ended() {
        // Typing the space after a full stop must not dim the page for one
        // keystroke and light it again on the next.
        assert_eq!(at("Done. ", 6), "Done.");
        assert_eq!(at("Done. Next one.", 6), "Done.");
    }

    #[test]
    fn a_cursor_at_the_very_end_after_a_full_stop_keeps_the_last_sentence() {
        assert_eq!(at("All finished.", 13), "All finished.");
    }

    #[test]
    fn a_newline_ends_a_sentence_with_or_without_a_mark() {
        let text = "# A title\nAnd the body.";
        assert_eq!(at(text, 4), "# A title");
        assert_eq!(at(text, 14), "And the body.");
    }

    #[test]
    fn every_list_item_is_its_own_sentence() {
        let text = "- first\n- second\n- third";
        assert_eq!(at(text, 3), "- first");
        assert_eq!(at(text, 11), "- second");
        assert_eq!(at(text, 20), "- third");
    }

    #[test]
    fn a_full_width_stop_ends_a_chinese_sentence() {
        let text = "今天天气很好。我们去公园吧。";
        assert_eq!(at(text, 2), "今天天气很好。");
        assert_eq!(at(text, 9), "我们去公园吧。");
    }

    #[test]
    fn a_japanese_question_mark_ends_one_too() {
        assert_eq!(at("元気ですか？はい。", 2), "元気ですか？");
    }

    #[test]
    fn a_mixed_line_breaks_where_the_mark_is_not_where_the_script_changes() {
        let text = "This is 中文 mixed in. 下一句在这里。";
        assert_eq!(at(text, 5), "This is 中文 mixed in.");
        assert_eq!(at(text, 24), "下一句在这里。");
    }

    #[test]
    fn a_decimal_point_is_not_the_end_of_anything() {
        assert_eq!(
            at("It weighs 3.5 kilos exactly.", 12),
            "It weighs 3.5 kilos exactly."
        );
    }

    #[test]
    fn an_abbreviation_does_not_end_a_sentence() {
        assert_eq!(at("Use e.g. this one.", 6), "Use e.g. this one.");
        assert_eq!(at("Nimm z.B. das da.", 7), "Nimm z.B. das da.");
    }

    #[test]
    fn a_lowercase_word_after_a_dot_continues_the_sentence() {
        assert_eq!(
            at("Cats, dogs etc. and so on.", 12),
            "Cats, dogs etc. and so on."
        );
    }

    #[test]
    fn a_closing_quote_stays_with_the_sentence_it_closes() {
        let text = "He said \"Go.\" She left.";
        assert_eq!(at(text, 3), "He said \"Go.\"");
    }

    #[test]
    fn a_doubled_mark_is_one_ending() {
        assert_eq!(at("Really?! I think so.", 3), "Really?!");
    }

    #[test]
    fn an_empty_document_has_an_empty_sentence() {
        assert_eq!(sentence_at(&[], 0), 0..0);
    }

    #[test]
    fn an_index_past_the_end_is_clamped_rather_than_a_panic() {
        let chars: Vec<char> = "Short.".chars().collect();
        assert_eq!(sentence_at(&chars, 999), 0..6);
    }

    #[test]
    fn a_blank_line_between_paragraphs_is_its_own_empty_sentence() {
        let text = "First para.\n\nSecond para.";
        assert_eq!(at(text, 12), "");
    }
}
