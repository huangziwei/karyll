//! Which face draws which run.
//!
//! Two things decide it: the script a character belongs to, and what the
//! Markdown around it means. Both are settled here as pure functions so the
//! policy can be tested without any of the faces being present — they live on
//! the device, not on a development machine.
//!
//! **Runs, not strings.** A face is chosen per run of same-script characters.
//! A line of prose here switches script constantly (`他说hello世界`), and Latin
//! and Han come out of different files, so there is no one face for the line.
//! The regional convention is fixed for the document instead of inferred per
//! run, which is what keeps Han unification from setting one paragraph in two
//! conventions.
//!
//! **CJK emphasis is a mark, not a slant.** An oblique Han or Hangul glyph is a
//! synthetic distortion rather than a style the script has, so emphasis is set
//! the way the writing systems themselves set it: a dot against each character
//! — 着重号 under it in Chinese, 圏点 over it in Japanese, 드러냄표 over it in
//! Korean. The face does not change, which is what lets one sentence carry both
//! — [`role_for`] gives Latin a real italic and leaves the CJK scripts upright
//! for [`takes_mark`] to point at.

use crate::markdown::{Block, Style};

/// Which family answers for a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Latin,
    /// Han, kana and the fullwidth forms — everything set from a CJK face.
    Han,
    /// Hangul: the syllables, and the jamo a half-composed syllable shows as.
    ///
    /// **The Korean faces carry no Hanja and the Han faces carry no Hangul.**
    /// 한자 written in Korean prose is a Han run beside a Hangul one, and each
    /// is drawn by the family that has it.
    Hangul,
    /// Anything else. Drawn from the Latin chain, falling back through it.
    Other,
}

pub fn script_of(c: char) -> Script {
    match c as u32 {
        0x2E80..=0x2FDF
        | 0x3000..=0x30FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFF60
        | 0x20000..=0x3FFFF => Script::Han,
        // The syllables, the conjoining jamo they decompose into, and the
        // compatibility jamo a half-composed syllable is shown as.
        0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => Script::Hangul,
        // ASCII and the Latin supplements, which is the bulk of prose here.
        0x0020..=0x024F | 0x2000..=0x206F | 0x2200..=0x22FF => Script::Latin,
        _ => Script::Other,
    }
}

/// A face to draw with, named by the job it does rather than by a filename.
///
/// The renderer maps these onto whatever the device actually has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Body,
    BodyItalic,
    BodyBold,
    BodyBoldItalic,
    /// Han body. Emphasis is set in it too, and marked rather than slanted.
    Han,
    HanBold,
    /// Hangul body. Emphasis is a 드러냄표 against the character.
    Hangul,
    HangulBold,
    /// karyll's own Latin text — a panel label, the action strip, a filename.
    Chrome,
    ChromeBold,
}

impl Role {
    pub fn is_han(self) -> bool {
        matches!(self, Role::Han | Role::HanBold)
    }

    pub fn is_hangul(self) -> bool {
        matches!(self, Role::Hangul | Role::HangulBold)
    }

    /// Whether this role draws karyll's own text rather than the document's.
    pub fn is_chrome(self) -> bool {
        matches!(self, Role::Chrome | Role::ChromeBold)
    }
}

/// The face for karyll's own text, as opposed to the writer's.
///
/// **The app does not restyle itself when the document face changes.** Chrome
/// and prose are two typographic jobs: the panel is a tool that names things,
/// the page is the draft. Setting them from one control meant a settings row
/// redrew in the face it was in the middle of choosing — chips changing width
/// under the finger picking them, and a row that fitted the panel before the tap
/// no longer fitting after it. So Latin chrome is pinned, and [`role_for`] is
/// left to the document alone.
///
/// **CJK chrome is not pinned**, and follows the writer's own family. A label
/// that says 简体 is showing which convention is set as well as naming it, and
/// there is no second Han face to pin it to that would not be one more 10 MB
/// file resident for the sake of four labels. Amazon Ember has no Hangul, so a
/// 한글 label draws from the Korean family.
pub fn chrome_role_for(bold: bool, script: Script) -> Role {
    match (script, bold) {
        (Script::Han, true) => Role::HanBold,
        (Script::Han, false) => Role::Han,
        (Script::Hangul, true) => Role::HangulBold,
        (Script::Hangul, false) => Role::Hangul,
        (_, true) => Role::ChromeBold,
        (_, false) => Role::Chrome,
    }
}

/// Which regional convention the Han faces should follow.
///
/// **Han unification is why this exists.** Simplified Chinese, Traditional
/// Chinese and Japanese share code points for characters they draw differently
/// — 骨, 直, 令, 音 and hundreds more differ in stroke count, stroke direction
/// or component shape between the three. One code point, three correct glyphs,
/// and only the document can say which is meant. So the convention is a
/// document-level setting rather than something inferred per character, which
/// is not inferable at all.
///
/// It follows the language being typed. Text already in the document keeps
/// whatever the current setting is, because a paragraph does not carry a
/// language tag — a limitation worth stating plainly rather than papering over,
/// and the same one every plain-text editor has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Region {
    #[default]
    Simplified,
    Traditional,
    Japanese,
}

impl Region {
    /// Which side of a character its emphasis mark sits on, writing across the
    /// page.
    ///
    /// **Japanese sets 圏点 over the character and Chinese sets 着重号 under
    /// it.** The same code point takes both, so the side is read from the
    /// convention rather than from the character — which is the one thing about
    /// emphasis that cannot be settled per run, and the same reason this
    /// setting exists at all.
    pub fn mark_above(self) -> bool {
        matches!(self, Region::Japanese)
    }
}

/// Which side of a character an emphasis mark sits on.
///
/// **Korean sets 드러냄표 over the character**, as Japanese sets 圏点. Hangul
/// carries no unification ambiguity, so [`Region`] holds no Korean entry and
/// [`Script::Hangul`] answers from `script` alone. Han asks the document,
/// through [`Region::mark_above`].
pub fn mark_above(script: Script, region: Region) -> bool {
    match script {
        Script::Hangul => true,
        _ => region.mark_above(),
    }
}

/// Whether an emphasised character carries a mark of its own.
///
/// **A mark per character, and only where a character is what it is against.**
/// Han and Hangul are written on an even em, so a mark against each one reads
/// as a run; a mark under the space or the comma between two of them reads as a
/// mistake. Latin inside the same emphasis is set in a real italic instead and
/// is never marked.
///
/// Korean sets its punctuation in ASCII, which classifies as [`Script::Latin`],
/// so the Hangul blocks hold letters and every one of them takes a mark.
pub fn takes_mark(c: char) -> bool {
    match script_of(c) {
        Script::Hangul => true,
        Script::Han if !c.is_whitespace() => {
            // The CJK punctuation block, and the fullwidth forms of the ASCII
            // marks — the fullwidth *letters* and digits between them are text
            // and take a mark.
            !matches!(c as u32,
                0x3000..=0x303F
                | 0xFF01..=0xFF0F
                | 0xFF1A..=0xFF20
                | 0xFF3B..=0xFF40
                | 0xFF5B..=0xFF65)
        }
        _ => false,
    }
}

/// The face for a run, given what it is and where it sits.
///
/// Headings are set bold throughout, so emphasis inside one has to reach for
/// the bold italic rather than dropping back to the upright.
pub fn role_for(block: Block, style: Style, script: Script) -> Role {
    let heading = matches!(block, Block::Heading(_));
    let emphasis = matches!(style, Style::Emphasis | Style::StrongEmphasis);
    let strong = matches!(style, Style::Strong | Style::StrongEmphasis);

    // CJK emphasis does not appear here at all: it is a mark beside the
    // character, drawn by the renderer, and the face stays where it is.
    if script == Script::Han {
        return if heading || strong {
            Role::HanBold
        } else {
            Role::Han
        };
    }
    if script == Script::Hangul {
        return if heading || strong {
            Role::HangulBold
        } else {
            Role::Hangul
        };
    }

    // Code takes the body face and is distinguished by the renderer instead.
    // Setting it in a face of its own would fix the document's monospace for it,
    // where the body face is the writer's to choose — and one of the faces on
    // offer already is one.
    match (heading || strong, emphasis) {
        (true, true) => Role::BodyBoldItalic,
        (true, false) => Role::BodyBold,
        (false, true) => Role::BodyItalic,
        (false, false) => Role::Body,
    }
}

/// Split `chars` into maximal runs of one script.
///
/// Runs are half-open index ranges into `chars`, in order, and they tile it.
///
/// A space classifies as Latin, so it ends a Han run and is drawn from the
/// Latin face. That is what we want: a Latin space is the right width beside
/// Latin text, and Chinese sets no space between characters in the first place.
pub fn runs(chars: &[char]) -> Vec<(std::ops::Range<usize>, Script)> {
    let mut out: Vec<(std::ops::Range<usize>, Script)> = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        let s = script_of(c);
        match out.last_mut() {
            Some((range, prev)) if *prev == s => range.end = i + 1,
            _ => out.push((i..i + 1, s)),
        }
    }
    out
}

/// Code points that carry no glyph and must never reach the rasterizer.
///
/// A font answers "no glyph" by handing back `.notdef`, so a character that is
/// invisible everywhere else becomes a visible box here.
pub fn is_invisible(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}'                  // soft hyphen
            | '\u{200B}'..='\u{200F}'   // ZWSP, ZWNJ, ZWJ, LRM, RLM
            | '\u{2060}'..='\u{2064}'   // word joiner and invisible operators
            | '\u{FEFF}'                // BOM / zero-width no-break space
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scripts(s: &str) -> Vec<(String, Script)> {
        let cs: Vec<char> = s.chars().collect();
        runs(&cs)
            .into_iter()
            .map(|(r, sc)| (cs[r].iter().collect::<String>(), sc))
            .collect()
    }

    #[test]
    fn classifies_the_scripts_that_matter() {
        assert_eq!(script_of('a'), Script::Latin);
        assert_eq!(script_of(' '), Script::Latin);
        assert_eq!(script_of('世'), Script::Han);
        assert_eq!(script_of('。'), Script::Han);
        assert_eq!(script_of('あ'), Script::Han);
        assert_eq!(script_of('한'), Script::Hangul);
        assert_eq!(script_of('ㄱ'), Script::Hangul);
        assert_eq!(script_of('ᄒ'), Script::Hangul);
        assert_eq!(script_of('А'), Script::Other);
    }

    /// **한자 in Korean prose is a Han run.** It classifies as [`Script::Han`],
    /// so `runs` splits it off for the Han faces to draw and the Korean faces
    /// are never asked for a Hanja. Korean sets its punctuation in ASCII, so a
    /// full stop is Latin here.
    #[test]
    fn korean_prose_splits_into_the_faces_that_have_it() {
        assert_eq!(
            scripts("한자는 漢字, hanja."),
            [
                ("한자는".to_string(), Script::Hangul),
                (" ".to_string(), Script::Latin),
                ("漢字".to_string(), Script::Han),
                (", hanja.".to_string(), Script::Latin),
            ]
        );
    }

    #[test]
    fn a_mixed_line_splits_at_the_script_boundary() {
        assert_eq!(
            scripts("他说hello世界"),
            [
                ("他说".to_string(), Script::Han),
                ("hello".to_string(), Script::Latin),
                ("世界".to_string(), Script::Han),
            ]
        );
    }

    #[test]
    fn a_pure_latin_line_is_one_run() {
        assert_eq!(
            scripts("just words"),
            [("just words".to_string(), Script::Latin)]
        );
    }

    #[test]
    fn han_punctuation_stays_with_its_run() {
        assert_eq!(
            scripts("你好，世界。"),
            [("你好，世界。".to_string(), Script::Han)]
        );
    }

    #[test]
    fn runs_tile_the_input() {
        for src in ["", "abc", "他说hello世界", "a他b说c", "  ", "中English中"] {
            let cs: Vec<char> = src.chars().collect();
            let mut at = 0;
            for (r, _) in runs(&cs) {
                assert_eq!(r.start, at);
                at = r.end;
            }
            assert_eq!(at, cs.len(), "runs did not cover {src:?}");
        }
    }

    #[test]
    fn latin_emphasis_is_an_italic() {
        assert_eq!(
            role_for(Block::Paragraph, Style::Emphasis, Script::Latin),
            Role::BodyItalic
        );
        assert_eq!(
            role_for(Block::Paragraph, Style::Strong, Script::Latin),
            Role::BodyBold
        );
    }

    /// **Emphasis leaves the Han face alone**, because the mark carries it.
    /// There is no italic Han role to reach for and no second family to swap
    /// to — an emphasised run is the body face with a dot against each
    /// character.
    #[test]
    fn han_emphasis_is_a_mark_never_a_slant_or_a_swap() {
        let body = role_for(Block::Paragraph, Style::Text, Script::Han);
        let emphasised = role_for(Block::Paragraph, Style::Emphasis, Script::Han);
        assert_eq!(emphasised, body);
        assert!(emphasised.is_han());
        // Latin in the same sentence still gets a real italic, which is what
        // makes `*これ*は*difficult*そうです` come out in two conventions.
        assert_eq!(
            role_for(Block::Paragraph, Style::Emphasis, Script::Latin),
            Role::BodyItalic
        );
    }

    /// Emphasis leaves the Hangul face where it is: [`role_for`] answers the
    /// body face, and the 드러냄표 carries the emphasis.
    #[test]
    fn hangul_emphasis_is_a_mark_never_a_slant_or_a_swap() {
        let body = role_for(Block::Paragraph, Style::Text, Script::Hangul);
        let emphasised = role_for(Block::Paragraph, Style::Emphasis, Script::Hangul);
        assert_eq!(emphasised, body);
        assert_eq!(body, Role::Hangul);
        assert_eq!(
            role_for(Block::Heading(1), Style::Text, Script::Hangul),
            Role::HangulBold
        );
    }

    /// The mark goes on the characters and not on what sits between them.
    #[test]
    fn what_carries_an_emphasis_mark() {
        for c in ['世', 'あ', 'ア', '漢', 'Ａ', '１', '한', '글', 'ㄱ'] {
            assert!(takes_mark(c), "{c} should carry a mark");
        }
        for c in ['。', '、', '，', '「', ' ', '\u{3000}', 'a', '.', '·'] {
            assert!(!takes_mark(c), "{c} should not carry a mark");
        }
    }

    /// Above for Japanese, below for Chinese — the one part of emphasis that is
    /// the document's to say rather than the character's.
    #[test]
    fn the_mark_sits_where_the_convention_puts_it() {
        assert!(Region::Japanese.mark_above());
        assert!(!Region::Simplified.mark_above());
        assert!(!Region::Traditional.mark_above());
    }

    /// A page mixing 한글 with 简体 marks the Hangul above and the Han below,
    /// in one sentence, at every [`Region`].
    #[test]
    fn hangul_marks_above_whichever_convention_is_set() {
        for region in [Region::Simplified, Region::Traditional, Region::Japanese] {
            assert!(mark_above(Script::Hangul, region), "{region:?}");
            assert_eq!(mark_above(Script::Han, region), region.mark_above());
        }
    }

    #[test]
    fn headings_are_bold_throughout() {
        assert_eq!(
            role_for(Block::Heading(1), Style::Text, Script::Latin),
            Role::BodyBold
        );
        assert_eq!(
            role_for(Block::Heading(1), Style::Text, Script::Han),
            Role::HanBold
        );
        // Emphasis inside a heading stays bold rather than dropping to upright.
        assert_eq!(
            role_for(Block::Heading(2), Style::Emphasis, Script::Latin),
            Role::BodyBoldItalic
        );
    }

    #[test]
    fn syntax_marks_take_the_body_face() {
        // They are drawn quiet by dithering, not by changing face.
        assert_eq!(
            role_for(Block::Paragraph, Style::Syntax, Script::Latin),
            Role::Body
        );
        assert_eq!(
            role_for(Block::Paragraph, Style::Syntax, Script::Han),
            Role::Han
        );
    }

    #[test]
    fn code_falls_back_to_the_body_face() {
        // No monospace text face exists on the device.
        assert_eq!(
            role_for(Block::Paragraph, Style::Code, Script::Latin),
            Role::Body
        );
    }

    #[test]
    fn invisible_characters_are_kept_from_the_rasterizer() {
        assert!(is_invisible('\u{200B}'));
        assert!(is_invisible('\u{FEFF}'));
        assert!(is_invisible('\n'));
        assert!(!is_invisible('a'));
        assert!(!is_invisible('世'));
    }
}
