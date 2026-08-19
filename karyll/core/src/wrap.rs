//! Line breaking for mixed Latin and CJK text.
//!
//! Latin breaks at spaces. Chinese breaks between almost any two characters,
//! which is why a wrapper written for English alone produces a ragged right
//! edge on Chinese prose — it can only break where there are spaces, and there
//! are none. The exceptions are punctuation rules: a line may not begin with
//! closing punctuation, and may not end with opening punctuation.
//!
//! Those two rules are kept by *push-out* — 追い出し, the remedy that moves the
//! character before the mark down with it, so the mark never opens a line. It
//! is what a break opportunity refused before a mark amounts to, and it is the
//! default because it costs a slightly shorter line and nothing else: this sets
//! ragged right, so there is no justification to stretch.
//!
//! A mark can also *hang* past the measure instead — ぶら下げ. That happens for
//! two unrelated reasons, and [`wrap_with`] tells them apart: as a style the
//! caller asks for, and as the last resort when a line holds no legal break at
//! all and push-out has nowhere to go.
//!
//! Line breaking is not all of it. Mixed Japanese and Chinese prose also takes
//! a quarter em between a Han character and Latin beside it — 四分アキ, which
//! [`aki`] decides and [`Rules::aki`] gives a width to. It is charged here
//! rather than by the caller because it is space *between* two characters, and
//! space between two characters vanishes when a line breaks there.
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

/// Whether a line may begin with `c`.
///
/// The same two refusals [`can_break_between`] makes — no break before a
/// closing mark, none before a space — read as a property of the character
/// alone, for the one caller that has no pair to ask about: the fallback in
/// [`wrap_with`], which is choosing where to split a run that offers it no
/// legal break at all.
fn may_open_a_line(c: char) -> bool {
    !matches!(classify(c), Class::Space | Class::Close)
}

/// Whether a mark may be set past the measure rather than pushed down.
///
/// **The stops only, and deliberately not [`Class::Close`].** A hung 。 or 、
/// carries its ink in the lower-left of its em box, so almost nothing of it
/// crosses the margin and it reads as intentional. A closing bracket has ink
/// against its right edge and a hung one reads as a mistake, which is why the
/// convention pushes brackets and quotes down and hangs only the stops. `…`
/// and `—` are `Close` for the same reason 」 is — a line may not open on one —
/// and are full-width ink, so neither hangs either.
pub fn hangable(c: char) -> bool {
    matches!(c, '。' | '、' | '．' | '，' | '.' | ',')
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

/// Whether a quarter em belongs between `a` and the `b` that follows it.
///
/// **四分アキ** (JLREQ §3.2). Japanese and Chinese setting puts a quarter of an
/// em between a Han or kana character and Latin letters or digits beside it, in
/// either order. Without it `karyllはRustで書いた` runs the two scripts together
/// at a boundary a reader of either one expects to see marked, and a CJK reader
/// reads its absence as an error rather than as a style.
///
/// **Letters and digits only on the Latin side.** [`Class::Other`] is
/// everything that is not CJK or punctuation, so it holds `*`, `#` and `~` as
/// well as words — and a gap opened between `*` and 世 would push the emphasis
/// marker off the character it marks.
///
/// **Nothing against punctuation on either side.** A mark carries its own
/// sidebearing inside its em box, and a quarter em on top of that reads as two
/// spaces rather than as one.
pub fn aki(a: char, b: char) -> bool {
    let han = |c| classify(c) == Class::Ideograph;
    let latin = |c: char| classify(c) == Class::Other && c.is_alphanumeric();
    han(a) && latin(b) || latin(a) && han(b)
}

/// The mark a word broken across two lines takes.
///
/// Never a character of the document: it is drawn past the end of the line, and
/// [`Line::range`] still names exactly the characters the line holds. That is
/// what keeps a caret, a selection and a hit test able to work in one index
/// space with the buffer.
pub const HYPHEN: char = '-';

/// One visual line: the characters it covers, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub range: Range<usize>,
    /// True when the line ended at a literal newline rather than at a wrap.
    /// The newline itself is not part of `range`.
    pub hard: bool,
    /// True when the line ended inside a word, so a [`HYPHEN`] is drawn after
    /// its last character. Not part of `range`; see that constant.
    pub hyphenated: bool,
}

/// How a line is set, beyond the script rules that always apply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rules {
    /// Whether a stop is set past the measure rather than pushing the character
    /// before it onto the next line. See [`hangable`] for which marks this
    /// covers, and the module note for why it is off by default.
    pub hang: bool,
    /// The width of 四分アキ, in the same unit as `max_width`. Zero sets none.
    ///
    /// A width rather than a flag because a quarter em is a quarter of *this
    /// line's* em: a heading takes a wider one than the body it heads, and
    /// nothing here knows a type size. See [`aki`] for where it is set.
    pub aki: u32,
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
pub fn wrap(chars: &[char], max_width: u32, measure: impl FnMut(usize, char) -> u32) -> Vec<Line> {
    wrap_with(chars, max_width, Rules::default(), measure, |_| Vec::new())
}

/// Break `chars` into lines, with `rules` over the script rules that always
/// apply. [`wrap()`] is this with nothing asked for.
///
/// `hyphenate(word)` is asked where a word may be divided, and answers with
/// character offsets **within** the range it was given. It is consulted only
/// for the one word an overflow lands inside — never for a line that breaks at
/// a space — so a document costs one call per wrapped row rather than one per
/// word. Answering with nothing switches word division off.
pub fn wrap_with(
    chars: &[char],
    max_width: u32,
    rules: Rules,
    mut measure: impl FnMut(usize, char) -> u32,
    mut hyphenate: impl FnMut(Range<usize>) -> Vec<usize>,
) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut width = 0u32;
    // The most recent index a break is allowed at, if any, within this line.
    let mut opportunity: Option<usize> = None;
    // Whether a mark has already been set past the measure on this line, so
    // that the style hangs one and not a run of them.
    let mut hung = false;

    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];

        if c == '\n' {
            lines.push(Line {
                range: start..i,
                hard: true,
                hyphenated: false,
            });
            start = i + 1;
            width = 0;
            opportunity = None;
            hung = false;
            i += 1;
            continue;
        }

        let w = measure(i, c);
        // Space set before this character, and none at the start of a line:
        // 四分アキ sits between two characters, so a break at that boundary
        // takes it away with the boundary.
        let g = if i > start {
            space(chars, rules, start, i)
        } else {
            0
        };
        if width + g + w > max_width && i > start {
            // The last position on this line a break is allowed at, if any.
            // An opportunity is never recorded before a closing mark, so one
            // that exists is always safe to take.
            let legal = opportunity.filter(|&b| b > start);

            // A division inside the word that overflowed, where the dictionary
            // allows one and the mark still fits on the line.
            let soft = soft_break(
                chars,
                rules,
                start,
                i,
                max_width,
                &mut measure,
                &mut hyphenate,
            );

            // **The division wins by being later**, which inside the
            // overflowing word it always is: the last space on the line is
            // before the word began. Where the dictionary offers nothing, or
            // offers only what a space already reaches, the space stands.
            let chosen = match (soft, legal) {
                (Some(at), Some(b)) if at <= b => Some((b, false)),
                (Some(at), _) => Some((at, true)),
                (None, b) => b.map(|b| (b, false)),
            };

            // **Nothing is left to push down.** With no break anywhere on the
            // line — between 「 and what follows it, or inside a word the
            // dictionary will not divide — the fallback below breaks
            // immediately before whatever overflowed. Where that is a mark or
            // a space, it opens the next line with one, which is the thing
            // these rules exist to prevent. It hangs instead, whatever the
            // caller asked for: the alternative is not a worse line, it is a
            // wrong one.
            let stranded = chosen.is_none() && !may_open_a_line(c);

            // The style, and the whole of its visible effect: 世界。 keeps its
            // 界 rather than sending it down for the sake of the stop. One per
            // line, so a run of marks cannot walk out into the margin.
            let styled = rules.hang && !hung && hangable(c);

            if stranded || styled {
                hung = true;
            } else {
                // Fall back to splitting mid-run for an unbroken sequence
                // longer than the line that no dictionary divides.
                let (brk, hyphenated) = chosen.unwrap_or((i, false));
                lines.push(Line {
                    range: start..brk,
                    hard: false,
                    hyphenated,
                });
                start = brk;
                opportunity = None;
                hung = false;
                // Re-measure the tail that moved onto the new line.
                width = (start..i)
                    .map(|j| measure(j, chars[j]) + space(chars, rules, start, j))
                    .sum();
            }
        }

        width += g + w;
        if i + 1 < chars.len() && can_break_between(c, chars[i + 1]) {
            opportunity = Some(i + 1);
        }
        i += 1;
    }

    lines.push(Line {
        range: start..chars.len(),
        hard: false,
        hyphenated: false,
    });
    lines
}

/// The space set before character `at` on a line that began at `start`.
///
/// Zero at the line's own start, so a row that happens to begin at a script
/// boundary is not indented by a quarter em past the flush edge.
fn space(chars: &[char], rules: Rules, start: usize, at: usize) -> u32 {
    if at <= start || rules.aki == 0 {
        return 0;
    }
    match (chars.get(at - 1), chars.get(at)) {
        (Some(&a), Some(&b)) if aki(a, b) => rules.aki,
        _ => 0,
    }
}

/// Whether `c` is a letter of a script that divides words rather than
/// characters.
///
/// `is_alphabetic` alone would claim Han and kana, which are alphabetic as far
/// as Unicode is concerned and divide by nothing a hyphenation dictionary
/// knows; [`classify`] is what separates them.
fn is_word_letter(c: char) -> bool {
    c.is_alphabetic() && classify(c) == Class::Other
}

/// The run of letters `at` falls in, or `None` where it falls outside one.
fn word_around(chars: &[char], at: usize) -> Option<Range<usize>> {
    if !chars.get(at).copied().is_some_and(is_word_letter) {
        return None;
    }
    let mut from = at;
    while from > 0 && is_word_letter(chars[from - 1]) {
        from -= 1;
    }
    let mut to = at + 1;
    while to < chars.len() && is_word_letter(chars[to]) {
        to += 1;
    }
    Some(from..to)
}

/// The latest division of the word at `over` that leaves the line, mark and
/// all, no wider than `max_width`.
///
/// `None` where the overflow did not land inside a word, where the word begins
/// past the line's own start, or where the dictionary divides it nowhere the
/// mark still fits. The mark is measured in the role of the character it
/// follows, so a division inside a bold word takes a bold hyphen.
fn soft_break(
    chars: &[char],
    rules: Rules,
    start: usize,
    over: usize,
    max_width: u32,
    measure: &mut impl FnMut(usize, char) -> u32,
    hyphenate: &mut impl FnMut(Range<usize>) -> Vec<usize>,
) -> Option<usize> {
    let word = word_around(chars, over).filter(|w| w.start < over)?;
    let offsets = hyphenate(word.clone());

    let mut found = None;
    // The offsets ascend, so the prefix is measured once across the whole word
    // rather than once per division.
    let mut upto = 0u32;
    let mut cursor = start;
    for at in offsets.into_iter().map(|b| word.start + b) {
        if at <= start {
            continue;
        }
        if at > over {
            break;
        }
        while cursor < at {
            upto += measure(cursor, chars[cursor]) + space(chars, rules, start, cursor);
            cursor += 1;
        }
        if upto + measure(at - 1, HYPHEN) <= max_width {
            found = Some(at);
        }
    }
    found
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
        hung(text, width, Rules::default())
    }

    fn hung(text: &str, width: u32, rules: Rules) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        wrap_with(&chars, width, rules, |_, c| measure(c), |_| Vec::new())
            .iter()
            .map(|l| chars[l.range.clone()].iter().collect())
            .collect()
    }

    const HANG: Rules = Rules { hang: true, aki: 0 };

    /// A stand-in dictionary: the words it knows against the character offsets
    /// it divides them at. Anything else it divides nowhere, which is what a
    /// real one answers for a word it has no patterns for.
    const DICT: [(&str, &[usize]); 3] = [
        ("hyphenation", &[2, 7]),
        ("Silbentrennung", &[3, 6, 10]),
        ("understanding", &[5, 10]),
    ];

    /// Wrapped, with each divided line showing the mark it is drawn with.
    fn divided(text: &str, width: u32) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        wrap_with(
            &chars,
            width,
            Rules::default(),
            |_, c| measure(c),
            |word| {
                let w: String = chars[word].iter().collect();
                DICT.iter()
                    .find(|(name, _)| *name == w)
                    .map(|(_, at)| at.to_vec())
                    .unwrap_or_default()
            },
        )
        .iter()
        .map(|l| {
            let mut s: String = chars[l.range.clone()].iter().collect();
            if l.hyphenated {
                s.push(HYPHEN);
            }
            s
        })
        .collect()
    }

    /// Every wrapped line — the first cannot be helped — begins with something
    /// allowed there.
    fn opens_legally(lines: &[String]) -> bool {
        lines
            .iter()
            .skip(1)
            .all(|l| l.chars().next().is_none_or(|c| classify(c) != Class::Close))
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

    /// The three shapes that leave the line with no legal break at all, where
    /// push-out has nothing to push and the mark has to hang.
    #[test]
    fn a_stranded_mark_hangs_rather_than_opening_a_line() {
        // 「 refuses a break after it and 。 refuses one before: between them
        // the line offers nothing, so 。 sits past the measure.
        let lines = rendered("你好「あ。再见", 4);
        assert_eq!(lines, ["你好", "「あ。", "再见"]);
        assert!(opens_legally(&lines), "{lines:?}");

        // A run of marks too wide to fit on a line of its own.
        let lines = rendered("你好世界。」再见", 4);
        assert_eq!(lines, ["你好", "世", "界。」", "再见"]);
        assert!(opens_legally(&lines), "{lines:?}");

        // Latin: a word chopped mid-letter has no legal break either, and the
        // stop after it would otherwise be a line by itself.
        let lines = rendered("supercalifragilistic.", 5);
        assert_eq!(lines, ["super", "calif", "ragil", "istic."]);
        assert!(opens_legally(&lines), "{lines:?}");
    }

    /// The realistic source: an unbreakable token whose length is a whole
    /// number of columns, so the chop lands exactly on the mark after it. The
    /// column widths are the ones `render::MARGINS` actually produces.
    #[test]
    fn a_token_that_fills_its_lines_keeps_the_mark_after_it() {
        for width in [72u32, 60, 48, 49, 41, 33] {
            for multiple in 1..=3 {
                let token = "a".repeat((width * multiple) as usize);
                let text = format!("Read {token}. Then stop.");
                let lines = rendered(&text, width);
                assert!(opens_legally(&lines), "width {width}: {lines:?}");
            }
        }
    }

    /// A space is no better at the head of a line than a mark is, and the
    /// fallback has to refuse both.
    #[test]
    fn a_line_never_begins_with_a_space_either() {
        assert_eq!(rendered("abc def", 3), ["abc ", "def"]);
        assert_eq!(
            rendered("supercalifragilistic word", 5),
            ["super", "calif", "ragil", "istic ", "word"]
        );
    }

    #[test]
    fn the_style_hangs_a_stop_instead_of_pushing_a_character_down() {
        // Push-out sends 界 down to keep 。 off the head of a line, which costs
        // a whole cell of a narrow column.
        assert_eq!(
            rendered("你好世界。再见", 4),
            ["你好", "世", "界。", "再见"]
        );
        // Hanging keeps 世界 together and sets 。 in the margin.
        assert_eq!(hung("你好世界。再见", 4, HANG), ["你好", "世界。", "再见"]);
    }

    #[test]
    fn only_the_stops_hang() {
        assert!(hangable('。') && hangable('、') && hangable('.') && hangable(','));
        // Full-width ink, and a line may not open on any of them — but they
        // are pushed down rather than hung.
        for c in ['」', '』', '）', '】', '…', '—', '！', '？'] {
            assert!(!hangable(c), "{c} hangs");
        }
        // 」 is not hung by the style: 界 goes down with it as before.
        assert_eq!(
            hung("你好世界」再见", 4, HANG),
            ["你好", "世", "界」", "再见"]
        );
    }

    #[test]
    fn the_style_hangs_one_mark_and_not_a_run() {
        // Two stops together: the first hangs, and the second takes the break
        // the line already had rather than following it into the margin. The
        // line ends up one glyph past the measure, not two.
        let lines = hung("你好世界。、再见", 4, HANG);
        assert_eq!(lines, ["你好", "世", "界。、", "再见"]);
        assert!(opens_legally(&lines), "{lines:?}");

        // The cap is on the style, not on the last resort. A run that cannot
        // be placed legally anywhere still hangs, because the alternative is a
        // line that opens with a mark. Breaking earlier is what keeps it to
        // two glyphs past the measure rather than three.
        let lines = hung("你好世界。。。再见", 4, HANG);
        assert_eq!(lines, ["你好", "世", "界。。。", "再见"]);
        assert!(opens_legally(&lines), "{lines:?}");
    }

    #[test]
    fn a_word_divides_where_the_dictionary_allows_it() {
        assert_eq!(
            divided("the hyphenation of it", 10),
            ["the hy-", "phenation ", "of it"]
        );
        assert_eq!(
            divided("die Silbentrennung hier", 12),
            ["die Silben-", "trennung ", "hier"]
        );
    }

    #[test]
    fn a_division_the_mark_will_not_fit_beside_is_refused() {
        // At six columns "the hy" plus the mark is seven, so that division is
        // passed over for the space, and the word divides on the next line
        // where the mark does fit.
        assert_eq!(
            divided("the hyphenation of it", 6),
            ["the ", "hy-", "phena-", "tion ", "of it"]
        );
    }

    #[test]
    fn a_line_that_fits_is_not_divided_at_all() {
        // The dictionary is not even consulted: nothing overflowed.
        assert_eq!(divided("a hyphenation", 20), ["a hyphenation"]);
    }

    #[test]
    fn a_word_the_dictionary_does_not_hold_is_chopped_without_a_mark() {
        // **No mark on an arbitrary chop.** A hyphen states a division the
        // dictionary vouched for, and there is none here; on the realistic
        // source of an unbreakable run — a URL — a mark would read as part of
        // the address.
        assert_eq!(
            divided("the unhyphenatable word", 10),
            ["the ", "unhyphenat", "able word"]
        );
    }

    #[test]
    fn cjk_is_never_offered_to_the_dictionary() {
        // Han and kana are alphabetic as far as Unicode is concerned, so a
        // word run built on `is_alphabetic` alone would hand a whole clause of
        // Chinese to a set of Liang patterns.
        assert_eq!(divided("你好世界再见", 4), ["你好", "世界", "再见"]);
        assert_eq!(
            word_around(&"你好世界".chars().collect::<Vec<_>>(), 1),
            None
        );
        assert_eq!(word_around(&"a 世 b".chars().collect::<Vec<_>>(), 2), None);
        assert_eq!(
            word_around(&"say 世界".chars().collect::<Vec<_>>(), 1),
            Some(0..3)
        );
    }

    #[test]
    fn division_still_tiles_the_input() {
        let text = "the hyphenation of understanding and Silbentrennung too";
        let chars: Vec<char> = text.chars().collect();
        for width in 4..30 {
            let lines = divided(text, width);
            let rebuilt: String = lines
                .iter()
                .map(|l| l.strip_suffix(HYPHEN).unwrap_or(l))
                .collect::<Vec<_>>()
                .join("");
            assert_eq!(rebuilt.chars().count(), chars.len(), "width {width}");
            assert_eq!(rebuilt, text, "width {width}");
        }
    }

    #[test]
    fn hanging_still_tiles_the_input() {
        let text = "他说「你好，世界」then hello world 再见。それは「あ。」だ。";
        let chars: Vec<char> = text.chars().collect();
        for width in 3..24 {
            let lines = wrap_with(&chars, width, HANG, |_, c| measure(c), |_| Vec::new());
            let mut cursor = 0usize;
            for l in &lines {
                assert_eq!(l.range.start, cursor, "gap or overlap at {l:?}");
                cursor = l.range.end + usize::from(l.hard);
            }
            assert_eq!(cursor, chars.len(), "width {width}");
        }
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

    /// **Both ways round, letters and digits, and nothing else.** The pairs
    /// that must *not* open a gap are the point: markup characters sit against
    /// the Han they mark, and punctuation already has a sidebearing.
    #[test]
    fn a_quarter_em_goes_between_han_and_latin_and_nowhere_else() {
        assert!(aki('は', 'R'));
        assert!(aki('第', '3'));
        assert!(aki('t', '世'));
        assert!(aki('7', '月'));
        assert!(!aki('世', '界'), "two Han characters");
        assert!(!aki('h', 'i'), "two Latin letters");
        assert!(!aki('*', '世'), "an emphasis marker is not a word");
        assert!(!aki('世', '*'));
        assert!(!aki('世', '。'), "a stop carries its own sidebearing");
        assert!(!aki('」', 'a'));
        assert!(!aki('世', ' '), "a space is already a space");
    }

    /// A width set on the line, not on either character: the same eleven
    /// characters fit without it and do not with it.
    #[test]
    fn the_quarter_em_takes_room_on_the_line() {
        let spaced = Rules {
            hang: false,
            aki: 1,
        };
        // `世界 abc` measures 2 + 2 + 3 = 7 units, and one boundary.
        assert_eq!(hung("世界abc", 7, Rules::default()), ["世界abc"]);
        assert_eq!(hung("世界abc", 7, spaced), ["世界", "abc"]);
    }

    /// **The gap goes away with the boundary.** A row that begins at a script
    /// boundary must start flush, or the quarter em becomes an indent on the
    /// one line the reader is most likely to notice it on.
    #[test]
    fn a_row_beginning_at_the_boundary_is_not_indented_by_the_gap() {
        let spaced = Rules {
            hang: false,
            aki: 1,
        };
        // Six units of room: `世界` and then `abcdef`, which exactly fills a
        // line only if the gap it broke at is not charged to it.
        assert_eq!(hung("世界abcdef", 6, spaced), ["世界", "abcdef"]);
    }

    #[test]
    fn spacing_still_tiles_the_input() {
        let text = "karyllはRustで書いた, 第3章まで";
        let chars: Vec<char> = text.chars().collect();
        let rules = Rules { hang: true, aki: 1 };
        let lines = wrap_with(&chars, 9, rules, |_, c| measure(c), |_| Vec::new());
        let mut cursor = 0usize;
        for l in &lines {
            assert_eq!(l.range.start, cursor, "gap or overlap at {l:?}");
            cursor = l.range.end;
        }
        assert_eq!(cursor, chars.len());
    }
}
