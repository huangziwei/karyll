//! Key codes into editing actions, a pure function of (code, mods, layout).
//! `character` decides which character a key position is; [`crate::ime`] takes
//! it from there.

/// Key codes from `linux/input-event-codes.h`.
pub mod code {
    pub const ESC: u16 = 1;
    pub const BACKSPACE: u16 = 14;
    pub const TAB: u16 = 15;
    /// `F10`: half-width Latin in every Japanese IME. `F1`–`F9`, `F11` and
    /// `F12` are unbound.
    pub const F10: u16 = 68;
    pub const ENTER: u16 = 28;
    /// The keypad's own Enter, answered wherever [`ENTER`] is.
    pub const KPENTER: u16 = 96;
    pub const LEFTCTRL: u16 = 29;
    pub const LEFTSHIFT: u16 = 42;
    pub const RIGHTSHIFT: u16 = 54;
    pub const LEFTALT: u16 = 56;
    pub const SPACE: u16 = 57;
    /// Latched by `Mods::track` on the press, unlatched by the next one.
    pub const CAPSLOCK: u16 = 58;
    pub const RIGHTCTRL: u16 = 97;
    pub const RIGHTALT: u16 = 100;
    /// The ⌘ / Super keys, accepted by `Mods::chord` alongside Ctrl.
    pub const LEFTMETA: u16 = 125;
    pub const RIGHTMETA: u16 = 126;
    pub const HOME: u16 = 102;
    pub const UP: u16 = 103;
    pub const PAGEUP: u16 = 104;
    pub const LEFT: u16 = 105;
    pub const RIGHT: u16 = 106;
    pub const END: u16 = 107;
    pub const DOWN: u16 = 108;
    pub const PAGEDOWN: u16 = 109;
    pub const DELETE: u16 = 111;
    /// `KEY_SEARCH`: the magnifier key.
    pub const SEARCH: u16 = 217;
    /// The two sun keys.
    pub const BRIGHTNESSDOWN: u16 = 224;
    pub const BRIGHTNESSUP: u16 = 225;
    /// `Q` decides whether an input device is a keyboard.
    pub const Q: u16 = 16;
}

/// Which modifiers are held.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// `KEY_RIGHTALT`. On `German` this is AltGr and the one way to
    /// `@ € [ ] { } \ | ~`; on `Us` it is the same key as `alt`.
    pub altgr: bool,
    /// ⌘. `chord` takes it with `ctrl`; `movement` reads the two apart.
    pub meta: bool,
    /// Caps Lock, latched by `track` and read by `character` for the letters.
    pub caps: bool,
}

impl Mods {
    /// Update on a press or release. `true` when `code` is a modifier. `caps`
    /// latches on the press; the rest follow the key.
    pub fn track(&mut self, code: u16, pressed: bool) -> bool {
        match code {
            code::LEFTSHIFT | code::RIGHTSHIFT => self.shift = pressed,
            code::LEFTCTRL | code::RIGHTCTRL => self.ctrl = pressed,
            code::LEFTALT => self.alt = pressed,
            code::RIGHTALT => self.altgr = pressed,
            code::LEFTMETA | code::RIGHTMETA => self.meta = pressed,
            code::CAPSLOCK => self.caps ^= pressed,
            _ => return false,
        }
        true
    }

    /// Whether a letter chord is being asked for. `ctrl` and `meta` both
    /// answer `true`, and every letter binding takes either. `movement` reads
    /// the two apart and runs ahead of this.
    pub fn chord(self) -> bool {
        self.ctrl || self.meta
    }
}

/// Which key beside the space bar carries ⌘. `Mac` sends `KEY_LEFTMETA` from
/// it and `KEY_LEFTALT` from the key outside; `Pc` sends the two the other
/// way round.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Convention {
    #[default]
    Mac,
    Pc,
}

impl Convention {
    pub fn name(self) -> &'static str {
        match self {
            Convention::Mac => "Mac",
            Convention::Pc => "PC",
        }
    }

    /// The `Convention` whose `name` this is. Anything else is `Mac`.
    pub fn from_name(name: &str) -> Self {
        match name {
            "PC" => Convention::Pc,
            _ => Convention::Mac,
        }
    }

    /// `mods` with ⌘ in `meta` and ⌥ in `alt`, whichever key sent them.
    /// `altgr` is `KEY_RIGHTALT` under both and holds its place.
    pub fn resolve(self, mods: Mods) -> Mods {
        match self {
            Convention::Mac => mods,
            Convention::Pc => Mods {
                alt: mods.meta,
                meta: mods.alt,
                ..mods
            },
        }
    }
}

/// What a keystroke asks the editor to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Insert(char),
    Newline,
    /// A tab, inserted as spaces. Markdown nesting counts columns.
    Indent,
    Backspace,
    Delete,
    /// Delete the word behind the cursor, or the selection.
    DeleteWordBack,
    DeleteWordForward,
    /// ⌘⌫: back to the start of the line in one go.
    DeleteToLineStart,
    DeleteToLineEnd,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    LineStart,
    LineEnd,
    WordLeft,
    WordRight,
    /// Top and bottom of the document: `Ctrl+Home`/`End`, or ⌘↑/⌘↓.
    DocStart,
    DocEnd,
    /// The `Shift`-held forms of the movements above: same destination,
    /// dragging the selection along.
    ExtendLeft,
    ExtendRight,
    ExtendUp,
    ExtendDown,
    ExtendLineStart,
    ExtendLineEnd,
    ExtendWordLeft,
    ExtendWordRight,
    ExtendDocStart,
    ExtendDocEnd,
    SelectAll,
    Cut,
    Copy,
    Paste,
    Undo,
    Redo,
    Save,
    Quit,
    /// Switch Chinese input on or off. `Ctrl` + `Space`.
    CycleLanguage,
    /// Set the page back around the sentence being written. `Ctrl`/`⌘` + `D`.
    ToggleFocus,
    /// Open the find bar, seeded by the selection. `Ctrl`/`⌘` + `F`.
    Find,
    /// Open the find bar with its second field: what to put in place of each
    /// match. `Ctrl`/`⌘` + `Shift` + `F`.
    Replace,
    /// Change the match on screen and step to the next. `Ctrl`/`⌘` + `Enter`.
    /// Inert outside the replace bar.
    Change,
    /// Change every match. `Ctrl`/`⌘` + `Shift` + `Enter`.
    ChangeAll,
    /// The headings of the open document, to jump between.
    /// `Ctrl`/`⌘` + `Shift` + `O`.
    Outline,
    /// Show what the keys and the glass do. `Ctrl`/`⌘` + `H`.
    Help,
    /// Clear the panel: a black frame and then the page again. `Ctrl`/`⌘` + `R`.
    Refresh,
    /// Set the page a size larger or smaller. `Ctrl`/`⌘` + `+` and `-`.
    Resize(bool),
    /// Set the page back to the size it opens at. `Ctrl`/`⌘` + `0`.
    ResetSize,
    /// Take the frontlight one step up or down. The two sun keys.
    Brightness(bool),
    /// Take the next margin along. `Ctrl`/`⌘` + `M`.
    CycleMargins,
    /// The document list. `Ctrl`/`⌘` + `O`.
    Files,
    /// Start a document, skipping the list. `Ctrl`/`⌘` + `N`.
    NewDocument,
    /// Settings. `Ctrl`/`⌘` + `,`.
    Config,
    /// Wrap the selection — or the word under the cursor — in `**` or `*`.
    /// `Ctrl`/`⌘` + `B` and `I`.
    Emphasis(&'static str),
    /// Set the current line to this heading level, or back to prose.
    /// `Ctrl`/`⌘` + `1`…`6`.
    Heading(u8),
    /// Commit what was typed as Latin. `F10`. Inert while CJK input is off.
    CommitTyped,
    /// Abandon the syllable being composed. Inert while writing.
    Escape,
}

/// A keyboard arrangement, named by [`crate::Language::layout`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Us,
    German,
}

impl Layout {
    pub fn name(self) -> &'static str {
        match self {
            Layout::Us => "US",
            Layout::German => "German",
        }
    }

    fn table(self) -> &'static [Key] {
        match self {
            Layout::Us => US,
            Layout::German => GERMAN,
        }
    }
}

/// One printable key: `(code, plain, shifted, altgr)`. `altgr` is `None` where
/// the key has no third legend.
type Key = (u16, char, char, Option<char>);

/// US QWERTY. The printable keys; `action` handles the rest by name.
#[rustfmt::skip]
const US: &[Key] = &[
    (2, '1', '!', None),   (3, '2', '@', None),   (4, '3', '#', None),
    (5, '4', '$', None),   (6, '5', '%', None),   (7, '6', '^', None),
    (8, '7', '&', None),   (9, '8', '*', None),   (10, '9', '(', None),
    (11, '0', ')', None),  (12, '-', '_', None),  (13, '=', '+', None),
    (16, 'q', 'Q', None),  (17, 'w', 'W', None),  (18, 'e', 'E', None),
    (19, 'r', 'R', None),  (20, 't', 'T', None),  (21, 'y', 'Y', None),
    (22, 'u', 'U', None),  (23, 'i', 'I', None),  (24, 'o', 'O', None),
    (25, 'p', 'P', None),  (26, '[', '{', None),  (27, ']', '}', None),
    (30, 'a', 'A', None),  (31, 's', 'S', None),  (32, 'd', 'D', None),
    (33, 'f', 'F', None),  (34, 'g', 'G', None),  (35, 'h', 'H', None),
    (36, 'j', 'J', None),  (37, 'k', 'K', None),  (38, 'l', 'L', None),
    (39, ';', ':', None),  (40, '\'', '"', None), (41, '`', '~', None),
    (43, '\\', '|', None), (44, 'z', 'Z', None),  (45, 'x', 'X', None),
    (46, 'c', 'C', None),  (47, 'v', 'V', None),  (48, 'b', 'B', None),
    (49, 'n', 'N', None),  (50, 'm', 'M', None),  (51, ',', '<', None),
    (52, '.', '>', None),  (53, '/', '?', None),  (86, '\\', '|', None),
];

/// The numeric keypad, one table for every `Layout`. Read ahead of
/// [`Layout::table`], carrying the digits NumLock shows.
#[rustfmt::skip]
const KEYPAD: &[(u16, char)] = &[
    (55, '*'), (71, '7'), (72, '8'), (73, '9'), (74, '-'),
    (75, '4'), (76, '5'), (77, '6'), (78, '+'), (79, '1'),
    (80, '2'), (81, '3'), (82, '0'), (83, '.'), (98, '/'),
];

/// German QWERTZ. Punctuation moves, the umlauts take the `; ' [` positions,
/// `@ € [ ] { } \ | ~` sit on AltGr, and code 86 is the key beside the left
/// shift. `´`, `` ` `` and `^` produce themselves: `´` then `a` gives `´a`.
#[rustfmt::skip]
const GERMAN: &[Key] = &[
    (2, '1', '!', None),        (3, '2', '"', Some('²')),  (4, '3', '§', Some('³')),
    (5, '4', '$', None),        (6, '5', '%', None),       (7, '6', '&', None),
    (8, '7', '/', Some('{')),   (9, '8', '(', Some('[')),  (10, '9', ')', Some(']')),
    (11, '0', '=', Some('}')),  (12, 'ß', '?', Some('\\')), (13, '´', '`', None),
    (16, 'q', 'Q', Some('@')),  (17, 'w', 'W', None),      (18, 'e', 'E', Some('€')),
    (19, 'r', 'R', None),       (20, 't', 'T', None),      (21, 'z', 'Z', None),
    (22, 'u', 'U', None),       (23, 'i', 'I', None),      (24, 'o', 'O', None),
    (25, 'p', 'P', None),       (26, 'ü', 'Ü', None),      (27, '+', '*', Some('~')),
    (30, 'a', 'A', None),       (31, 's', 'S', None),      (32, 'd', 'D', None),
    (33, 'f', 'F', None),       (34, 'g', 'G', None),      (35, 'h', 'H', None),
    (36, 'j', 'J', None),       (37, 'k', 'K', None),      (38, 'l', 'L', None),
    (39, 'ö', 'Ö', None),       (40, 'ä', 'Ä', None),      (41, '^', '°', None),
    (43, '#', '\'', None),      (44, 'y', 'Y', None),      (45, 'x', 'X', None),
    (46, 'c', 'C', None),       (47, 'v', 'V', None),      (48, 'b', 'B', None),
    (49, 'n', 'N', None),       (50, 'm', 'M', Some('µ')), (51, ',', ';', None),
    (52, '.', ':', None),       (53, '-', '_', None),      (86, '<', '>', Some('|')),
];

/// The character a printable key produces, if it is one.
pub fn character(code: u16, mods: Mods, layout: Layout) -> Option<char> {
    if code == code::SPACE {
        return Some(' ');
    }
    if let Some((_, key)) = KEYPAD.iter().find(|(c, _)| *c == code) {
        return Some(*key);
    }
    let (_, plain, shifted, altgr) = layout.table().iter().find(|(c, ..)| *c == code)?;
    // AltGr outranks shift. Nothing sits at AltGr+shift.
    if mods.altgr {
        return *altgr;
    }
    // `caps` reaches a key whose upper register is its own capital: `1` stays
    // `1` and `ß` stays `ß`.
    let caps = mods.caps && plain.to_uppercase().eq(std::iter::once(*shifted));
    Some(if mods.shift != caps { *shifted } else { *plain })
}

/// How far a movement key travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grain {
    Char,
    Word,
    Line,
}

/// Arrows, `Home`/`End` and the deletions beside them. `meta` is the line
/// grain, `ctrl`/`alt`/`altgr` the word grain, bare the character, and `shift`
/// extends any of them. No key answered here is printable.
fn movement(code: u16, mods: Mods) -> Option<Action> {
    let grain = if mods.meta {
        Grain::Line
    } else if mods.ctrl || mods.alt || mods.altgr {
        Grain::Word
    } else {
        Grain::Char
    };
    let shift = mods.shift;

    Some(match (code, grain, shift) {
        (code::LEFT, Grain::Char, false) => Action::Left,
        (code::LEFT, Grain::Char, true) => Action::ExtendLeft,
        (code::LEFT, Grain::Word, false) => Action::WordLeft,
        (code::LEFT, Grain::Word, true) => Action::ExtendWordLeft,
        (code::LEFT, Grain::Line, false) => Action::LineStart,
        (code::LEFT, Grain::Line, true) => Action::ExtendLineStart,

        (code::RIGHT, Grain::Char, false) => Action::Right,
        (code::RIGHT, Grain::Char, true) => Action::ExtendRight,
        (code::RIGHT, Grain::Word, false) => Action::WordRight,
        (code::RIGHT, Grain::Word, true) => Action::ExtendWordRight,
        (code::RIGHT, Grain::Line, false) => Action::LineEnd,
        (code::RIGHT, Grain::Line, true) => Action::ExtendLineEnd,

        // ⌘↑ and ⌘↓ are the whole document. ⌥↑ falls through to a line move.
        (code::UP, Grain::Line, false) => Action::DocStart,
        (code::UP, Grain::Line, true) => Action::ExtendDocStart,
        (code::DOWN, Grain::Line, false) => Action::DocEnd,
        (code::DOWN, Grain::Line, true) => Action::ExtendDocEnd,
        (code::UP, _, false) => Action::Up,
        (code::UP, _, true) => Action::ExtendUp,
        (code::DOWN, _, false) => Action::Down,
        (code::DOWN, _, true) => Action::ExtendDown,

        // Bare, `Home` and `End` are the line. Under any modifier, the document.
        (code::HOME, Grain::Char, false) => Action::LineStart,
        (code::HOME, Grain::Char, true) => Action::ExtendLineStart,
        (code::HOME, _, false) => Action::DocStart,
        (code::HOME, _, true) => Action::ExtendDocStart,
        (code::END, Grain::Char, false) => Action::LineEnd,
        (code::END, Grain::Char, true) => Action::ExtendLineEnd,
        (code::END, _, false) => Action::DocEnd,
        (code::END, _, true) => Action::ExtendDocEnd,

        (code::BACKSPACE, Grain::Char, _) => Action::Backspace,
        (code::BACKSPACE, Grain::Word, _) => Action::DeleteWordBack,
        (code::BACKSPACE, Grain::Line, _) => Action::DeleteToLineStart,
        (code::DELETE, Grain::Char, _) => Action::Delete,
        (code::DELETE, Grain::Word, _) => Action::DeleteWordForward,
        (code::DELETE, Grain::Line, _) => Action::DeleteToLineEnd,

        _ => return None,
    })
}

/// What a key press means. `None` for a key with no binding. `movement`
/// resolves first, then `chord`, then the named keys, then `character`.
/// `alt` alone binds nothing.
pub fn action(code: u16, mods: Mods, layout: Layout) -> Option<Action> {
    // Ahead of `chord()`, which flattens `ctrl` and `meta` together.
    if let Some(action) = movement(code, mods) {
        return Some(action);
    }
    if mods.chord() {
        if code == code::SPACE {
            // The one binding `meta` does not take. ⌘Space types no space.
            return mods.ctrl.then_some(Action::CycleLanguage);
        }
        // The replace bar's two commands, ahead of `character`.
        if code == code::ENTER || code == code::KPENTER {
            return Some(if mods.shift {
                Action::ChangeAll
            } else {
                Action::Change
            });
        }
        // Every arm below is written unshifted and unlatched.
        return match character(
            code,
            Mods {
                shift: false,
                caps: false,
                ..mods
            },
            layout,
        ) {
            Some('s') => Some(Action::Save),
            Some('z') if mods.shift => Some(Action::Redo),
            Some('z') => Some(Action::Undo),
            Some('y') => Some(Action::Redo),
            Some('q') => Some(Action::Quit),
            // `Home`/`End` carry line-start and line-end.
            Some('a') => Some(Action::SelectAll),
            Some('x') => Some(Action::Cut),
            Some('c') => Some(Action::Copy),
            Some('v') => Some(Action::Paste),
            Some('d') => Some(Action::ToggleFocus),
            Some('f') if mods.shift => Some(Action::Replace),
            Some('f') => Some(Action::Find),
            // The shifted arm comes first.
            Some('h') if mods.shift => Some(Action::Emphasis("==")),
            Some('h') => Some(Action::Help),
            // Every button on the strip has a key here.
            Some('r') => Some(Action::Refresh),
            // Matched on the character: `-` is code 12 on QWERTY and 53 on
            // QWERTZ, and the key that enlarges is `=` on one, `+` on the other.
            Some('-') => Some(Action::Resize(false)),
            Some('=' | '+') => Some(Action::Resize(true)),
            // The digit beside them, and the one the heading levels leave.
            Some('0') => Some(Action::ResetSize),
            // Three margins, cycled from one key.
            Some('m') => Some(Action::CycleMargins),
            // Open a document, and — shifted — a place inside the open one.
            Some('o') if mods.shift => Some(Action::Outline),
            Some('o') => Some(Action::Files),
            Some('n') => Some(Action::NewDocument),
            Some(',') => Some(Action::Config),
            Some('b') => Some(Action::Emphasis("**")),
            Some('i') => Some(Action::Emphasis("*")),
            // Digits are heading levels here. `compose` abandons a composition
            // on a chord, so candidate selection never reaches this arm.
            Some(c @ '1'..='6') => Some(Action::Heading(c as u8 - b'0')),
            _ => None,
        };
    }

    match code {
        code::ESC => Some(Action::Escape),
        code::F10 => Some(Action::CommitTyped),
        code::SEARCH => Some(Action::Find),
        code::BRIGHTNESSDOWN => Some(Action::Brightness(false)),
        code::BRIGHTNESSUP => Some(Action::Brightness(true)),
        // `Enter` commits the composition as it stands; `Shift`+`Enter` commits
        // the letters struck. Outside a composition, a line break.
        code::ENTER | code::KPENTER if mods.shift => Some(Action::CommitTyped),
        code::ENTER | code::KPENTER => Some(Action::Newline),
        code::PAGEUP => Some(Action::PageUp),
        code::PAGEDOWN => Some(Action::PageDown),
        code::TAB => Some(Action::Indent),
        _ => character(code, mods, layout).map(Action::Insert),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Mods {
        Mods::default()
    }

    fn shift() -> Mods {
        Mods {
            shift: true,
            ..Mods::default()
        }
    }

    fn ctrl() -> Mods {
        Mods {
            ctrl: true,
            ..Mods::default()
        }
    }

    fn meta() -> Mods {
        Mods {
            meta: true,
            ..Mods::default()
        }
    }

    #[test]
    fn hello_decodes_from_the_codes_the_device_reported() {
        // The key codes captured on the Scribe when "hello" was typed.
        let typed: String = [35u16, 18, 38, 38, 24]
            .iter()
            .filter_map(|c| character(*c, plain(), Layout::Us))
            .collect();
        assert_eq!(typed, "hello");
    }

    #[test]
    fn shift_selects_the_upper_register() {
        assert_eq!(character(30, plain(), Layout::Us), Some('a'));
        assert_eq!(character(30, shift(), Layout::Us), Some('A'));
        assert_eq!(character(2, plain(), Layout::Us), Some('1'));
        assert_eq!(character(2, shift(), Layout::Us), Some('!'));
    }

    /// Every character the Markdown parser treats as markup is typeable on
    /// every layout. On `German` most of them are AltGr legends.
    #[test]
    fn the_markdown_characters_are_reachable_on_every_layout() {
        let wanted = "#*_`[]()->.";
        for layout in [Layout::Us, Layout::German] {
            for want in wanted.chars() {
                let found = layout.table().iter().any(|(_, plain, shifted, altgr)| {
                    *plain == want || *shifted == want || *altgr == Some(want)
                });
                assert!(found, "{} has no key for {want:?}", layout.name());
            }
        }
    }

    #[test]
    fn space_is_not_in_the_layout_table_but_still_types() {
        assert_eq!(character(code::SPACE, plain(), Layout::Us), Some(' '));
        assert_eq!(character(code::SPACE, shift(), Layout::Us), Some(' '));
    }

    #[test]
    fn named_keys_map_to_actions_not_characters() {
        assert_eq!(
            action(code::ENTER, plain(), Layout::Us),
            Some(Action::Newline)
        );
        assert_eq!(
            action(code::BACKSPACE, plain(), Layout::Us),
            Some(Action::Backspace)
        );
        assert_eq!(action(code::LEFT, plain(), Layout::Us), Some(Action::Left));
        assert_eq!(
            action(code::HOME, plain(), Layout::Us),
            Some(Action::LineStart)
        );
        assert_eq!(character(code::ENTER, plain(), Layout::Us), None);
    }

    #[test]
    fn a_chord_beats_the_character_it_would_otherwise_type() {
        assert_eq!(action(31, ctrl(), Layout::Us), Some(Action::Save));
        assert_eq!(action(31, plain(), Layout::Us), Some(Action::Insert('s')));
        assert_eq!(action(44, ctrl(), Layout::Us), Some(Action::Undo));
        assert_eq!(
            action(
                44,
                Mods {
                    ctrl: true,
                    shift: true,
                    ..Mods::default()
                },
                Layout::Us
            ),
            Some(Action::Redo)
        );
    }

    #[test]
    fn an_unbound_chord_does_nothing_rather_than_typing() {
        // Code 17 carries `w`, and `Ctrl+W` binds nothing.
        assert_eq!(action(17, ctrl(), Layout::Us), None);
    }

    /// The shifted forms of Find and Open, on both layouts.
    #[test]
    fn shift_deepens_find_and_open_rather_than_repeating_them() {
        for layout in [Layout::Us, Layout::German] {
            for mods in [ctrl(), meta()] {
                let shifted = Mods {
                    shift: true,
                    ..mods
                };
                assert_eq!(action(33, mods, layout), Some(Action::Find));
                assert_eq!(action(33, shifted, layout), Some(Action::Replace));
                assert_eq!(action(24, mods, layout), Some(Action::Files));
                assert_eq!(action(24, shifted, layout), Some(Action::Outline));
            }
        }
    }

    /// `Enter` under a chord is the replace bar's own command. With no chord,
    /// a line break; with `Shift`, the letters as typed.
    #[test]
    fn enter_only_changes_things_when_a_chord_is_held() {
        let shift = Mods {
            shift: true,
            ..Mods::default()
        };
        assert_eq!(
            action(code::ENTER, plain(), Layout::Us),
            Some(Action::Newline)
        );
        assert_eq!(
            action(code::ENTER, shift, Layout::Us),
            Some(Action::CommitTyped)
        );
        for mods in [ctrl(), meta()] {
            assert_eq!(action(code::ENTER, mods, Layout::Us), Some(Action::Change));
            assert_eq!(
                action(
                    code::ENTER,
                    Mods {
                        shift: true,
                        ..mods
                    },
                    Layout::Us
                ),
                Some(Action::ChangeAll)
            );
        }
    }

    /// Every button on the strip has a key, on both layouts. The chord arm
    /// resolves the code with shift forced off, and a binding needing `Shift`
    /// on QWERTZ is dead there.
    #[test]
    fn every_panel_has_a_key_and_it_works_on_both_layouts() {
        let bindings = [
            (35, Action::Help),        // H
            (24, Action::Files),       // O
            (49, Action::NewDocument), // N
            (51, Action::Config),      // comma
            (19, Action::Refresh),     // R
        ];
        for (code, expected) in bindings {
            for layout in [Layout::Us, Layout::German] {
                for mods in [ctrl(), meta()] {
                    assert_eq!(
                        action(code, mods, layout),
                        Some(expected),
                        "code {code} on {}",
                        layout.name()
                    );
                }
            }
            // And types its letter with no chord held.
            assert!(matches!(
                action(code, plain(), Layout::Us),
                Some(Action::Insert(_))
            ));
        }
    }

    /// `M` is unshifted and in one place on either layout.
    #[test]
    fn the_margins_cycle_from_either_chord_on_either_layout() {
        for layout in [Layout::Us, Layout::German] {
            for mods in [ctrl(), meta()] {
                assert_eq!(action(50, mods, layout), Some(Action::CycleMargins));
            }
            // And types its letter with no chord held.
            assert!(matches!(
                action(50, plain(), layout),
                Some(Action::Insert('m'))
            ));
        }
    }

    /// `ctrl` and `meta` resolve to one action for every letter code, which
    /// covers a binding added later without naming it.
    #[test]
    fn command_does_everything_control_does() {
        // Save, undo, quit, select all — and `w`, unbound under both.
        for code in [31, 44, 16, 30, 17] {
            assert_eq!(
                action(code, meta(), Layout::Us),
                action(code, ctrl(), Layout::Us),
                "code {code} means different things under Ctrl and ⌘"
            );
        }
        assert_eq!(action(31, meta(), Layout::Us), Some(Action::Save));
    }

    mod movement {
        use super::*;

        fn with(mods: Mods, shift: bool) -> Mods {
            Mods { shift, ..mods }
        }

        fn alt() -> Mods {
            Mods {
                alt: true,
                ..Mods::default()
            }
        }

        fn altgr() -> Mods {
            Mods {
                altgr: true,
                ..Mods::default()
            }
        }

        /// Both conventions at once: `alt` and `ctrl` are one grain.
        #[test]
        fn option_and_control_both_move_by_word() {
            for mods in [ctrl(), alt()] {
                assert_eq!(
                    action(code::RIGHT, mods, Layout::Us),
                    Some(Action::WordRight)
                );
                assert_eq!(action(code::LEFT, mods, Layout::Us), Some(Action::WordLeft));
            }
        }

        /// The right-hand ⌥ key sends the code `German` uses for AltGr.
        #[test]
        fn the_right_option_key_moves_by_word_too() {
            assert_eq!(
                action(code::RIGHT, altgr(), Layout::Us),
                Some(Action::WordRight)
            );
        }

        /// `altgr` reaches a third legend on printable keys; no movement key
        /// is one.
        #[test]
        fn treating_altgr_as_option_does_not_cost_german_its_third_legend() {
            assert_eq!(
                action(16, altgr(), Layout::German),
                Some(Action::Insert('@'))
            );
            assert_eq!(
                action(9, altgr(), Layout::German),
                Some(Action::Insert('['))
            );
        }

        /// `meta` is the one modifier `movement` reads apart from `ctrl`.
        #[test]
        fn command_moves_by_line_where_control_moves_by_word() {
            assert_eq!(
                action(code::LEFT, meta(), Layout::Us),
                Some(Action::LineStart)
            );
            assert_eq!(
                action(code::RIGHT, meta(), Layout::Us),
                Some(Action::LineEnd)
            );
            assert_ne!(
                action(code::LEFT, meta(), Layout::Us),
                action(code::LEFT, ctrl(), Layout::Us)
            );
        }

        #[test]
        fn shift_extends_every_grain_of_movement() {
            for (mods, want) in [
                (plain(), Action::ExtendLeft),
                (ctrl(), Action::ExtendWordLeft),
                (alt(), Action::ExtendWordLeft),
                (meta(), Action::ExtendLineStart),
            ] {
                assert_eq!(
                    action(code::LEFT, with(mods, true), Layout::Us),
                    Some(want),
                    "{mods:?}"
                );
            }
            assert_eq!(
                action(code::HOME, with(plain(), true), Layout::Us),
                Some(Action::ExtendLineStart)
            );
            assert_eq!(
                action(code::UP, with(plain(), true), Layout::Us),
                Some(Action::ExtendUp)
            );
        }

        #[test]
        fn backspace_deletes_a_character_a_word_or_a_line() {
            assert_eq!(
                action(code::BACKSPACE, plain(), Layout::Us),
                Some(Action::Backspace)
            );
            assert_eq!(
                action(code::BACKSPACE, alt(), Layout::Us),
                Some(Action::DeleteWordBack)
            );
            assert_eq!(
                action(code::BACKSPACE, meta(), Layout::Us),
                Some(Action::DeleteToLineStart)
            );
            assert_eq!(
                action(code::DELETE, ctrl(), Layout::Us),
                Some(Action::DeleteWordForward)
            );
        }

        /// `Ctrl+A` selects all; the line start is `Home`.
        #[test]
        fn control_a_selects_all_and_home_still_goes_to_the_line_start() {
            assert_eq!(action(30, ctrl(), Layout::Us), Some(Action::SelectAll));
            assert_eq!(action(30, meta(), Layout::Us), Some(Action::SelectAll));
            assert_eq!(
                action(code::HOME, plain(), Layout::Us),
                Some(Action::LineStart)
            );
            // `Ctrl+E` is unbound.
            assert_eq!(action(18, ctrl(), Layout::Us), None);
        }

        /// `Home` and `End` under any modifier reach the whole document.
        #[test]
        fn any_modifier_on_home_or_end_means_the_whole_document() {
            for mods in [ctrl(), alt(), meta()] {
                assert_eq!(action(code::HOME, mods, Layout::Us), Some(Action::DocStart));
                assert_eq!(action(code::END, mods, Layout::Us), Some(Action::DocEnd));
            }
            // ⌘↑ and ⌘↓ spell the same pair.
            assert_eq!(action(code::UP, meta(), Layout::Us), Some(Action::DocStart));
            assert_eq!(action(code::DOWN, meta(), Layout::Us), Some(Action::DocEnd));
            // Bare, they are the line; ⌥↑ is a line up.
            assert_eq!(
                action(code::HOME, plain(), Layout::Us),
                Some(Action::LineStart)
            );
            assert_eq!(action(code::UP, alt(), Layout::Us), Some(Action::Up));
        }

        /// The clipboard chords, under both modifiers and both layouts. X, C
        /// and V hold their positions on `German`; the lookup runs the table.
        #[test]
        fn cut_copy_and_paste_are_bound_on_both_modifiers_and_layouts() {
            for layout in [Layout::Us, Layout::German] {
                for mods in [ctrl(), meta()] {
                    assert_eq!(action(45, mods, layout), Some(Action::Cut));
                    assert_eq!(action(46, mods, layout), Some(Action::Copy));
                    assert_eq!(action(47, mods, layout), Some(Action::Paste));
                }
                // And unmodified they type.
                assert_eq!(action(46, plain(), layout), Some(Action::Insert('c')));
            }
        }
    }

    /// ⌘Space puts no space in the draft.
    #[test]
    fn command_space_is_not_the_language_switch() {
        assert_eq!(
            action(code::SPACE, ctrl(), Layout::Us),
            Some(Action::CycleLanguage)
        );
        assert_eq!(action(code::SPACE, meta(), Layout::Us), None);
        assert_eq!(
            action(code::SPACE, plain(), Layout::Us),
            Some(Action::Insert(' '))
        );
    }

    #[test]
    fn modifiers_are_tracked_on_press_and_release() {
        let mut mods = Mods::default();
        assert!(mods.track(code::LEFTSHIFT, true));
        assert!(mods.shift);
        assert!(mods.track(code::LEFTSHIFT, false));
        assert!(!mods.shift);
        // Both sides of the keyboard.
        assert!(mods.track(code::RIGHTCTRL, true));
        assert!(mods.ctrl);
        // ⌘ tracks as a modifier, not as an ordinary key.
        assert!(mods.track(code::LEFTMETA, true));
        assert!(mods.meta);
        assert!(mods.track(code::RIGHTMETA, false));
        assert!(!mods.meta);
        // A normal key is not a modifier.
        assert!(!mods.track(30, true));
    }

    #[test]
    fn alt_alone_types_the_plain_character() {
        // `alt` binds nothing and swallows no keystroke.
        let alt = Mods {
            alt: true,
            ..Mods::default()
        };
        assert_eq!(action(30, alt, Layout::Us), Some(Action::Insert('a')));
    }

    /// `caps` latches on the press and the release leaves it.
    #[test]
    fn caps_lock_latches_and_the_next_press_lets_it_go() {
        let mut mods = Mods::default();
        assert!(mods.track(code::CAPSLOCK, true));
        assert!(mods.caps);
        assert!(mods.track(code::CAPSLOCK, false));
        assert!(mods.caps);
        assert!(mods.track(code::CAPSLOCK, true));
        assert!(!mods.caps);
    }

    #[test]
    fn caps_lock_reaches_the_letters_and_leaves_the_rest_alone() {
        let caps = Mods {
            caps: true,
            ..Mods::default()
        };
        let both = Mods {
            shift: true,
            ..caps
        };
        for layout in [Layout::Us, Layout::German] {
            assert_eq!(character(30, caps, layout), Some('A'));
            // `shift` and `caps` together cancel.
            assert_eq!(character(30, both, layout), Some('a'));
            // A digit under Caps Lock is the digit.
            assert_eq!(character(2, caps, layout), Some('1'));
        }
        // `ß` capitalises to two letters, and stays itself.
        assert_eq!(character(12, caps, Layout::German), Some('ß'));
        assert_eq!(character(26, caps, Layout::German), Some('Ü'));
    }

    /// `caps` left on does not shift a chord.
    #[test]
    fn a_chord_under_caps_lock_is_the_chord_it_is_under_neither() {
        let caps = Mods {
            caps: true,
            ctrl: true,
            ..Mods::default()
        };
        assert_eq!(action(31, caps, Layout::Us), Some(Action::Save));
        assert_eq!(action(24, caps, Layout::Us), Some(Action::Files));
    }

    /// `KEYPAD` answers the same on every `Layout`.
    #[test]
    fn the_numeric_keypad_types_its_own_legends() {
        for layout in [Layout::Us, Layout::German] {
            for (code, key) in [
                (55u16, '*'),
                (71, '7'),
                (74, '-'),
                (78, '+'),
                (82, '0'),
                (83, '.'),
                (98, '/'),
            ] {
                assert_eq!(
                    action(code, plain(), layout),
                    Some(Action::Insert(key)),
                    "code {code} on {}",
                    layout.name()
                );
            }
            // `shift` and `caps` leave `KEYPAD` alone.
            assert_eq!(action(79, shift(), layout), Some(Action::Insert('1')));
        }
    }

    #[test]
    fn the_keypads_enter_is_enter() {
        assert_eq!(
            action(code::KPENTER, plain(), Layout::Us),
            Some(Action::Newline)
        );
        assert_eq!(
            action(code::KPENTER, shift(), Layout::Us),
            Some(Action::CommitTyped)
        );
        assert_eq!(
            action(code::KPENTER, ctrl(), Layout::Us),
            Some(Action::Change)
        );
    }

    #[test]
    fn the_magnifier_opens_find() {
        assert_eq!(
            action(code::SEARCH, plain(), Layout::Us),
            Some(Action::Find)
        );
    }

    /// The digit `Heading` leaves, beside the two that step the size.
    #[test]
    fn the_zero_key_sets_the_type_back() {
        for layout in [Layout::Us, Layout::German] {
            for mods in [ctrl(), meta()] {
                assert_eq!(action(11, mods, layout), Some(Action::ResetSize));
            }
            assert_eq!(action(11, plain(), layout), Some(Action::Insert('0')));
        }
    }

    /// `Mac` and `Pc` reach the same three bindings from the key against the
    /// space bar.
    #[test]
    fn the_key_beside_the_space_bar_reaches_the_same_bindings_either_way() {
        let alt = Mods {
            alt: true,
            ..Mods::default()
        };
        for (beside, outside, convention) in [
            (meta(), alt, Convention::Mac),
            (alt, meta(), Convention::Pc),
        ] {
            let beside = convention.resolve(beside);
            let outside = convention.resolve(outside);
            let at = convention.name();
            // Save, off the key against the space bar.
            assert_eq!(action(31, beside, Layout::Us), Some(Action::Save), "{at}");
            // The line grain there, the word grain outside it.
            assert_eq!(
                action(code::LEFT, beside, Layout::Us),
                Some(Action::LineStart),
                "{at}"
            );
            assert_eq!(
                action(code::LEFT, outside, Layout::Us),
                Some(Action::WordLeft),
                "{at}"
            );
        }
    }

    /// `resolve` leaves `altgr` where it arrived under both.
    #[test]
    fn the_third_legend_survives_the_swap() {
        let altgr = Mods {
            altgr: true,
            ..Mods::default()
        };
        for convention in [Convention::Mac, Convention::Pc] {
            let mods = convention.resolve(altgr);
            assert!(mods.altgr, "{}", convention.name());
            assert!(!mods.alt && !mods.meta, "{}", convention.name());
            assert_eq!(
                action(16, mods, Layout::German),
                Some(Action::Insert('@')),
                "{}",
                convention.name()
            );
        }
    }

    /// `resolve` leaves `ctrl` alone under both.
    #[test]
    fn control_reaches_config_under_either_convention() {
        for convention in [Convention::Mac, Convention::Pc] {
            let mods = convention.resolve(ctrl());
            assert_eq!(
                action(51, mods, Layout::Us),
                Some(Action::Config),
                "{}",
                convention.name()
            );
        }
    }

    #[test]
    fn a_convention_survives_the_round_trip_through_its_name() {
        for convention in [Convention::Mac, Convention::Pc] {
            assert_eq!(Convention::from_name(convention.name()), convention);
        }
        assert_eq!(Convention::from_name(""), Convention::Mac);
    }

    #[test]
    fn every_layout_code_is_listed_once() {
        for layout in [Layout::Us, Layout::German] {
            let mut codes: Vec<u16> = layout.table().iter().map(|(c, ..)| *c).collect();
            codes.sort_unstable();
            let before = codes.len();
            codes.dedup();
            assert_eq!(codes.len(), before, "{} binds a code twice", layout.name());
        }
    }

    mod german {
        use super::*;

        /// Codes 21 and 44 carry `y` and `z` in opposite order per layout.
        #[test]
        fn y_and_z_trade_places() {
            assert_eq!(character(21, plain(), Layout::Us), Some('y'));
            assert_eq!(character(21, plain(), Layout::German), Some('z'));
            assert_eq!(character(44, plain(), Layout::Us), Some('z'));
            assert_eq!(character(44, plain(), Layout::German), Some('y'));
        }

        #[test]
        fn the_umlauts_and_eszett_are_where_the_keycaps_say() {
            for (code, want) in [(39, 'ö'), (40, 'ä'), (26, 'ü'), (12, 'ß')] {
                assert_eq!(character(code, plain(), Layout::German), Some(want));
            }
            assert_eq!(character(39, shift(), Layout::German), Some('Ö'));
        }

        /// `altgr` carries the characters Markdown is written with on `German`.
        #[test]
        fn altgr_reaches_the_third_legend() {
            let altgr = Mods {
                altgr: true,
                ..Mods::default()
            };
            for (code, want) in [
                (9, '['),
                (10, ']'),
                (8, '{'),
                (11, '}'),
                (16, '@'),
                (18, '€'),
            ] {
                assert_eq!(character(code, altgr, Layout::German), Some(want));
            }
        }

        /// A key with no third legend produces nothing under `altgr`.
        #[test]
        fn altgr_on_a_key_without_one_types_nothing() {
            let altgr = Mods {
                altgr: true,
                ..Mods::default()
            };
            assert_eq!(character(30, altgr, Layout::German), None);
        }

        /// Code 86, the key beside the left shift, carries `<`, `>` and `|` on
        /// `German`. An ISO board reading `Us` finds `\` and `|` there.
        #[test]
        fn the_key_us_boards_lack_is_bound() {
            assert_eq!(character(86, plain(), Layout::German), Some('<'));
            assert_eq!(character(86, shift(), Layout::German), Some('>'));
            assert_eq!(character(86, plain(), Layout::Us), Some('\\'));
            assert_eq!(character(86, shift(), Layout::Us), Some('|'));
        }

        /// Punctuation moves position and arrives as the same character, which
        /// is what `ime::punctuation` keys on.
        #[test]
        fn punctuation_moves_but_still_arrives_as_itself() {
            assert_eq!(character(51, plain(), Layout::German), Some(','));
            assert_eq!(character(52, plain(), Layout::German), Some('.'));
            // Semicolon and colon are shifted on `German`, unshifted on `Us`.
            assert_eq!(character(51, shift(), Layout::German), Some(';'));
            assert_eq!(character(52, shift(), Layout::German), Some(':'));
            // The question mark sits on the ß key.
            assert_eq!(character(12, shift(), Layout::German), Some('?'));
        }
    }
}
