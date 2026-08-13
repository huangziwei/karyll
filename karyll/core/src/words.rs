//! Counting words, in prose that mixes scripts.
//!
//! **Two rules, because two writing systems.** Latin prose is counted in
//! whitespace-separated runs; CJK has no spaces between words, so every Han
//! character, kana and fullwidth form counts as one. That is what Word, Pages
//! and iA Writer all do, and a writer who has been told 1,000 characters means
//! something different from one told 1,000 words.
//!
//! It counts the *source*, marks and all, because that is what is on the page:
//! this editor shows Markdown rather than hiding it. The marks themselves are
//! punctuation and are never words — `**bold**` is one word, and a line of
//! `---` is none.

use crate::script::{Script, is_invisible, script_of};

/// How many words `chars` holds.
///
/// A word is a run of characters containing at least one alphanumeric, so
/// punctuation on its own is never one — a bullet, a stray dash or a row of
/// asterisks adds nothing. Punctuation *inside* a run does not break it, which
/// is what keeps `don't` and `well-meaning` to one word each.
pub fn count(chars: &[char]) -> usize {
    let mut words = 0;
    let mut inside = false;
    for &ch in chars {
        // **Whitespace is tested before invisibility**, and the order is the
        // fix rather than an accident: a newline and a tab are control
        // characters, so `is_invisible` is true of them — they carry no glyph
        // and must never reach the rasterizer. They are still the plainest word
        // boundary there is, and skipping them first counted `across\nlines` as
        // one word.
        if ch.is_whitespace() {
            inside = false;
            continue;
        }
        if is_invisible(ch) {
            continue;
        }
        // A Han character is a word, and it also ends whatever ran before it:
        // `中文word` is 中, 文 and one Latin word, not one long token. The test
        // is alphanumeric rather than "is Han", because 。、「」 and the
        // fullwidth comma are in the same blocks and are punctuation.
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

/// The count as the panel says it, with a thousands separator and a word for
/// nothing at all.
///
/// "0 words" is a true answer to a question nobody asked; a document with
/// nothing in it is worth naming as empty, because that is the one fact about
/// it that decides whether to open it.
pub fn describe(words: usize) -> String {
    match words {
        0 => "empty".into(),
        1 => "1 word".into(),
        n => format!("{} words", grouped(n)),
    }
}

/// `1284` as `1,284`. Four figures of prose is a normal morning, so this is
/// reached often enough to be worth reading at a glance.
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

    /// Punctuation inside a word does not split it, which is the difference
    /// between counting words and counting runs of letters.
    #[test]
    fn a_word_survives_its_own_punctuation() {
        assert_eq!(n("don't"), 1);
        assert_eq!(n("well-meaning"), 1);
        assert_eq!(n("e.g. this"), 2);
        assert_eq!(n("(parenthesised)"), 1);
    }

    /// A writing app shows its Markdown, so the marks are on the page — but
    /// they are punctuation, and punctuation alone is not prose.
    #[test]
    fn punctuation_on_its_own_is_never_a_word() {
        assert_eq!(n("---"), 0);
        assert_eq!(n("* * *"), 0);
        assert_eq!(n("**bold**"), 1);
        assert_eq!(n("# Heading"), 1);
        assert_eq!(n("- a list item"), 3);
        assert_eq!(n("> quoted text"), 2);
    }

    /// CJK has no spaces between words, so the character is the unit — which
    /// is what every word counter that handles it at all does.
    #[test]
    fn every_han_character_counts_as_one() {
        assert_eq!(n("你好"), 2);
        assert_eq!(n("你好，世界"), 4, "the comma is punctuation");
        assert_eq!(n("日本語"), 3);
        assert_eq!(n("ひらがな"), 4, "kana too");
        assert_eq!(n("他说「你好」。"), 4, "the brackets and stop are not");
        assert_eq!(n("你好　世界"), 4, "an ideographic space is a space");
    }

    /// The mixed case, which is the whole reason the two rules have to live in
    /// one function: `中文word` is two characters and a word, not one token.
    #[test]
    fn a_han_run_ends_the_latin_word_beside_it() {
        assert_eq!(n("中文word"), 3);
        assert_eq!(n("word中文"), 3);
        assert_eq!(n("他说 hello 世界"), 5, "four characters and one word");
    }

    /// The zero-width characters the renderer inserts are not prose and must
    /// not be counted as characters of it.
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
