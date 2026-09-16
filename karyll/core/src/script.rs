//! Which face draws which run. **Runs, not strings**: one face per run of
//! same-script characters, the regional convention fixed per document. **CJK
//! emphasis is a mark, not a slant** — 着重号, 圏点 or 드러냄표, the face unchanged.

use crate::markdown::{Block, Style};

/// Which family answers for a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Latin,
    /// Han, kana and the fullwidth forms — everything set from a CJK face.
    Han,
    /// Hangul: the syllables, and the jamo a half-composed syllable shows as.
    /// **The Korean faces carry no Hanja and the Han faces no Hangul**, so 한자
    /// in Korean prose is a Han run beside a Hangul one.
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

/// The face for karyll's own text. **Latin chrome is pinned** — the app does not
/// restyle itself when the document face changes. **CJK chrome is not**, and
/// follows the writer's family; Amazon Ember has no Hangul.
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

/// Which regional convention the Han faces follow. **Han unification is why
/// this exists**: one code point, three correct glyphs, and only the document
/// can say which is meant, since plain text carries no language tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Region {
    #[default]
    Simplified,
    Traditional,
    Japanese,
}

impl Region {
    /// Which side of a character its emphasis mark sits on. **Japanese sets 圏点
    /// over and Chinese 着重号 under**; the same code point takes both, so the
    /// side comes from the convention rather than the character.
    pub fn mark_above(self) -> bool {
        matches!(self, Region::Japanese)
    }
}

/// Which side of a character an emphasis mark sits on. **Korean sets 드러냄표
/// over**, and carries no unification ambiguity, so Hangul answers from `script`
/// alone while Han asks the document through [`Region::mark_above`].
pub fn mark_above(script: Script, region: Region) -> bool {
    match script {
        Script::Hangul => true,
        _ => region.mark_above(),
    }
}

/// Whether an emphasised character carries a mark of its own. **One per
/// character, and only where a character is what it is against**: a mark under
/// the space between two reads as a mistake. Latin takes a real italic instead.
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

/// The face for a run. Headings are set bold throughout, so emphasis inside one
/// reaches for the bold italic rather than dropping back to the upright.
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

    // Code takes the body face and is distinguished by the renderer instead:
    // the body face is the writer's to choose, and one on offer is monospace.
    match (heading || strong, emphasis) {
        (true, true) => Role::BodyBoldItalic,
        (true, false) => Role::BodyBold,
        (false, true) => Role::BodyItalic,
        (false, false) => Role::Body,
    }
}

/// Split `chars` into maximal runs of one script: half-open ranges, in order,
/// tiling it. A space classifies as Latin, so it ends a Han run and takes the
/// Latin face, which is the right width.
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

/// Code points that carry no glyph and must never reach the rasterizer: a font
/// answers "no glyph" with `.notdef`, which draws as a visible box.
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

    /// **한자 in Korean prose is a Han run**, so the Korean faces are never asked
    /// for a Hanja. Korean punctuation is ASCII, so a full stop is Latin here.
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

    /// **Emphasis leaves the Han face alone**, because the mark carries it: an
    /// emphasised run is the body face with a dot against each character.
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
