//! Line breaking for mixed Latin and CJK text.
//!
//! Latin breaks at spaces. Chinese breaks between almost any two characters,
//! which is why a wrapper written for English alone produces a ragged right
//! edge on Chinese prose — it can only break where there are spaces, and there
//! are none. The exceptions are punctuation rules: a line may not begin with
//! closing punctuation, and may not end with opening punctuation.
//!
//! This is a deliberately small subset of UAX #14 — the part that matters for
//! prose. Widths come from a caller-supplied measure function so that nothing
//! here depends on a font.

use std::ops::Range;

/// What a character does to the break opportunities around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Breaks after, never before.
    Space,
    /// Opening punctuation. A line may not end on one.
    Open,
    /// Closing punctuation. A line may not begin with one.
    Close,
    /// Ideographs, kana and fullwidth forms — breakable on both sides.
    Ideograph,
    /// Latin letters, digits and everything else: breakable only at spaces.
    Other,
}

pub fn classify(c: char) -> Class {
    match c {
        ' ' | '\t' | '\u{3000}' => Class::Space,

        // Opening brackets and quotes, CJK and ASCII.
        '「' | '『' | '（' | '【' | '〈' | '《' | '〔' | '［' | '｛' | '‘' | '“' | '(' | '['
        | '{' => Class::Open,

        // Closing punctuation: brackets, quotes, and the stops that must not
        // start a line.
        '」' | '』' | '）' | '】' | '〉' | '》' | '〕' | '］' | '｝' | '’' | '”' | ')' | ']'
        | '}' | '。' | '、' | '，' | '．' | '；' | '：' | '？' | '！' | '·' | '…' | '—' | ','
        | '.' | ';' | ':' | '?' | '!' => Class::Close,

        _ if is_ideographic(c) => Class::Ideograph,
        _ => Class::Other,
    }
}

/// Scripts that break between characters rather than between words.
fn is_ideographic(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x2FDF   // radicals and Kangxi
        | 0x3040..=0x30FF // hiragana, katakana
        | 0x3400..=0x4DBF // CJK extension A
        | 0x4E00..=0x9FFF // CJK unified ideographs
        | 0xF900..=0xFAFF // compatibility ideographs
        | 0xFF00..=0xFF60 // fullwidth forms
        | 0x20000..=0x3FFFF // extensions B and beyond
    )
}

/// Whether a line may break between `a` and the `b` that follows it.
pub fn can_break_between(a: char, b: char) -> bool {
    use Class::*;
    match (classify(a), classify(b)) {
        // A space never starts a line; it hangs off the end of the previous one.
        (_, Space) => false,
        (Space, _) => true,
        // Punctuation rules outrank the ideograph rule: 「 must not end a line
        // even though what follows it is breakable, and 。 must not start one.
        (Open, _) => false,
        (_, Close) => false,
        (Ideograph, _) | (_, Ideograph) => true,
        _ => false,
    }
}

/// One visual line: the characters it covers, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub range: Range<usize>,
    /// True when the line ended at a literal newline rather than at a wrap.
    /// The newline itself is not part of `range`.
    pub hard: bool,
}

/// Break `chars` into lines no wider than `max_width`.
///
/// `measure(index, char)` returns the advance of a character in whatever unit
/// `max_width` is given in. A character wider than `max_width` on its own still
/// gets its own line rather than looping forever.
///
/// The index is passed because a character's advance is not a property of the
/// character alone: which face draws it depends on the markup around it, so
/// `*世*` measures differently from `世`. It is an index into `chars`, and
/// `measure` may be called more than once for the same one.
pub fn wrap(
    chars: &[char],
    max_width: u32,
    mut measure: impl FnMut(usize, char) -> u32,
) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut width = 0u32;
    // The most recent index a break is allowed at, if any, within this line.
    let mut opportunity: Option<usize> = None;

    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];

        if c == '\n' {
            lines.push(Line {
                range: start..i,
                hard: true,
            });
            start = i + 1;
            width = 0;
            opportunity = None;
            i += 1;
            continue;
        }

        let w = measure(i, c);
        if width + w > max_width && i > start {
            // Prefer a legal break; fall back to splitting mid-run for a word
            // or an unbroken sequence longer than the line.
            let brk = opportunity.filter(|&b| b > start).unwrap_or(i);
            lines.push(Line {
                range: start..brk,
                hard: false,
            });
            start = brk;
            opportunity = None;
            // Re-measure the tail that moved onto the new line.
            width = (start..i).map(|j| measure(j, chars[j])).sum();
        }

        width += w;
        if i + 1 < chars.len() && can_break_between(c, chars[i + 1]) {
            opportunity = Some(i + 1);
        }
        i += 1;
    }

    lines.push(Line {
        range: start..chars.len(),
        hard: false,
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Latin advances one unit, ideographs and fullwidth forms two — the same
    /// proportion a real CJK face gives.
    fn measure(c: char) -> u32 {
        if is_ideographic(c)
            || matches!(classify(c), Class::Open | Class::Close) && c as u32 > 0x2000
        {
            2
        } else {
            1
        }
    }

    fn rendered(text: &str, width: u32) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        wrap(&chars, width, |_, c| measure(c))
            .iter()
            .map(|l| chars[l.range.clone()].iter().collect())
            .collect()
    }

    #[test]
    fn latin_breaks_at_spaces() {
        assert_eq!(
            rendered("the quick brown fox", 10),
            ["the quick ", "brown fox"]
        );
    }

    #[test]
    fn latin_never_breaks_inside_a_word() {
        let input = "alpha beta gamma";
        let lines = rendered(input, 8);
        for word in input.split(' ') {
            assert!(
                lines.iter().any(|l| l.split(' ').any(|w| w == word)),
                "{word:?} was split across lines: {lines:?}"
            );
        }
        assert_eq!(lines, ["alpha ", "beta ", "gamma"]);
    }

    #[test]
    fn an_overlong_word_is_split_rather_than_looping() {
        assert_eq!(
            rendered("supercalifragilistic", 5),
            ["super", "calif", "ragil", "istic"]
        );
    }

    #[test]
    fn chinese_breaks_between_characters() {
        // No spaces anywhere, so a Latin-only wrapper would emit one long line.
        assert_eq!(rendered("你好世界再见", 4), ["你好", "世界", "再见"]);
    }

    #[test]
    fn a_line_never_begins_with_closing_punctuation() {
        // Breaking before 。 would put it at the head of line two.
        let lines = rendered("你好世界。再见", 4);
        assert!(lines.iter().all(|l| !l.starts_with('。')), "{lines:?}");
    }

    #[test]
    fn a_line_never_ends_with_opening_punctuation() {
        let lines = rendered("他说「你好世界」", 4);
        assert!(lines.iter().all(|l| !l.ends_with('「')), "{lines:?}");
    }

    #[test]
    fn newlines_are_hard_breaks() {
        let chars: Vec<char> = "ab\ncd".chars().collect();
        let lines = wrap(&chars, 100, |_, c| measure(c));
        assert_eq!(lines.len(), 2);
        assert!(lines[0].hard);
        assert!(!lines[1].hard);
        assert_eq!(lines[0].range, 0..2);
        assert_eq!(lines[1].range, 3..5);
    }

    #[test]
    fn blank_lines_survive() {
        let chars: Vec<char> = "a\n\nb".chars().collect();
        let lines = wrap(&chars, 100, |_, c| measure(c));
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].range, 2..2);
    }

    #[test]
    fn mixed_scripts_break_at_the_script_boundary() {
        assert_eq!(rendered("hello 世界", 6), ["hello ", "世界"]);
    }

    #[test]
    fn trailing_space_does_not_force_an_early_wrap() {
        // The space hangs past the right edge instead of pushing "b" down.
        assert_eq!(rendered("ab cd", 3), ["ab ", "cd"]);
    }

    #[test]
    fn every_character_appears_exactly_once() {
        let text = "他说「你好，世界」then hello world 再见。";
        let chars: Vec<char> = text.chars().collect();
        let lines = wrap(&chars, 7, |_, c| measure(c));
        // Ranges must tile the input, with only newlines unaccounted for.
        let mut covered = 0usize;
        let mut cursor = 0usize;
        for l in &lines {
            assert_eq!(l.range.start, cursor, "gap or overlap at {l:?}");
            covered += l.range.len();
            cursor = l.range.end + usize::from(l.hard);
        }
        assert_eq!(cursor, chars.len());
        assert_eq!(covered, chars.len());
    }

    #[test]
    fn classification_of_the_characters_the_rules_turn_on() {
        assert_eq!(classify('。'), Class::Close);
        assert_eq!(classify('「'), Class::Open);
        assert_eq!(classify('世'), Class::Ideograph);
        assert_eq!(classify('あ'), Class::Ideograph);
        assert_eq!(classify('a'), Class::Other);
        assert_eq!(classify(' '), Class::Space);
        assert!(!can_break_between('世', '。'));
        assert!(!can_break_between('「', '你'));
        assert!(can_break_between('你', '好'));
        assert!(!can_break_between('h', 'i'));
    }
}
