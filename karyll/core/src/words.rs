//! Counting words in prose that mixes scripts: Latin prose counts
//! whitespace-separated runs, every Han character and kana counts as one,
//! and the count is over the source, Markdown marks included.

use crate::script::{Script, is_invisible, script_of};

/// How many words `chars` holds.
///
/// A word is a run holding at least one alphanumeric: `---` counts none,
/// `don't` and `well-meaning` count one each.
pub fn count(chars: &[char]) -> usize {
    let mut words = 0;
    let mut inside = false;
    for &ch in chars {
        // Whitespace before invisibility: a newline is both invisible —
        // `is_invisible` is true of control characters — and a word boundary.
        if ch.is_whitespace() {
            inside = false;
            continue;
        }
        if is_invisible(ch) {
            continue;
        }
        // A Han character is one word and ends the run before it: `中文word`
        // is 中, 文 and one Latin word. 。、「」 and the fullwidth comma sit
        // in the same blocks and fail `is_alphanumeric`, counting nothing.
        if script_of(ch) == Script::Han {
            if ch.is_alphanumeric() {
                words += 1;
            }
            inside = false;
        } else if ch.is_alphanumeric() && !inside {
            words += 1;
            inside = true;
        }
    }
    words
}

/// The count as the panel states it: `empty` for zero, `1 word`, a
/// thousands-separated figure past that.
pub fn describe(words: usize) -> String {
    match words {
        0 => "empty".into(),
        1 => "1 word".into(),
        n => format!("{} words", grouped(n)),
    }
}

/// `1284` as `1,284`.
pub fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(text: &str) -> usize {
        count(&text.chars().collect::<Vec<_>>())
    }

    #[test]
    fn latin_prose_is_counted_in_runs() {
        assert_eq!(n(""), 0);
        assert_eq!(n("word"), 1);
        assert_eq!(n("two words"), 2);
        assert_eq!(n("  leading and trailing  "), 3);
        assert_eq!(n("across\nlines\ttoo"), 3);
    }

    /// Punctuation inside a run does not split it.
    #[test]
    fn a_word_survives_its_own_punctuation() {
        assert_eq!(n("don't"), 1);
        assert_eq!(n("well-meaning"), 1);
        assert_eq!(n("e.g. this"), 2);
        assert_eq!(n("(parenthesised)"), 1);
    }

    /// Markdown marks are punctuation, and punctuation alone counts nothing.
    #[test]
    fn punctuation_on_its_own_is_never_a_word() {
        assert_eq!(n("---"), 0);
        assert_eq!(n("* * *"), 0);
        assert_eq!(n("**bold**"), 1);
        assert_eq!(n("# Heading"), 1);
        assert_eq!(n("- a list item"), 3);
        assert_eq!(n("> quoted text"), 2);
    }

    /// The character is the unit for CJK.
    #[test]
    fn every_han_character_counts_as_one() {
        assert_eq!(n("你好"), 2);
        assert_eq!(n("你好，世界"), 4, "the comma is punctuation");
        assert_eq!(n("日本語"), 3);
        assert_eq!(n("ひらがな"), 4, "kana too");
        assert_eq!(n("他说「你好」。"), 4, "the brackets and stop are not");
        assert_eq!(n("你好　世界"), 4, "an ideographic space is a space");
    }

    /// `中文word` is two characters and one word, not one token.
    #[test]
    fn a_han_run_ends_the_latin_word_beside_it() {
        assert_eq!(n("中文word"), 3);
        assert_eq!(n("word中文"), 3);
        assert_eq!(n("他说 hello 世界"), 5, "four characters and one word");
    }

    /// Characters `is_invisible` covers count nothing and split nothing.
    #[test]
    fn invisible_characters_are_not_words() {
        assert_eq!(n("a\u{200B}b"), 1, "a zero-width space does not split");
        assert_eq!(n("\u{200B}"), 0);
    }

    #[test]
    fn the_count_reads_as_a_number_a_writer_recognises() {
        assert_eq!(describe(0), "empty");
        assert_eq!(describe(1), "1 word");
        assert_eq!(describe(12), "12 words");
        assert_eq!(describe(1_284), "1,284 words");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
