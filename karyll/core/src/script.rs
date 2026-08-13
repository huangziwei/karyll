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
//! **Chinese emphasis is a face change, not a slant.** Chinese type marks
//! emphasis by switching family — 宋体 body against 黑体 emphasis — or with
//! emphasis dots. It never slants: an oblique Han glyph is a synthetic
//! distortion, not a style the script has. So [`Role`] gives Latin real italics
//! and gives Han a different family for the same meaning.

use crate::markdown::{Block, Style};

/// Which family answers for a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Latin,
    /// Han, kana and the fullwidth forms — everything set from a CJK face.
    Han,
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
    /// Han body.
    Han,
    /// Han emphasis: a different family, never a slant.
    HanEmphasis,
    HanBold,
}

impl Role {
    pub fn is_han(self) -> bool {
        matches!(self, Role::Han | Role::HanEmphasis | Role::HanBold)
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

/// The face for a run, given what it is and where it sits.
///
/// Headings are set bold throughout, so emphasis inside one has to reach for
/// the bold italic rather than dropping back to the upright.
pub fn role_for(block: Block, style: Style, script: Script) -> Role {
    let heading = matches!(block, Block::Heading(_));
    let emphasis = matches!(style, Style::Emphasis | Style::StrongEmphasis);
    let strong = matches!(style, Style::Strong | Style::StrongEmphasis);

    if script == Script::Han {
        return match (heading || strong, emphasis) {
            (true, _) => Role::HanBold,
            (false, true) => Role::HanEmphasis,
            (false, false) => Role::Han,
        };
    }

    // Code has no monospace face on this device, so it takes the body face and
    // is distinguished by the renderer instead.
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
        assert_eq!(script_of('А'), Script::Other);
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

    #[test]
    fn han_emphasis_is_a_face_change_never_a_slant() {
        let role = role_for(Block::Paragraph, Style::Emphasis, Script::Han);
        assert_eq!(role, Role::HanEmphasis);
        // There is no italic Han role to reach for at all.
        assert!(role.is_han());
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
