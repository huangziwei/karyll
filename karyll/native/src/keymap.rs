//! Turning key codes into editing actions.
//!
//! The editor reads `/dev/input/eventN` directly rather than taking keys
//! through X. Raw key codes are what the CJK engine wants, so routing through X
//! would mean translating twice — and the window never takes focus anyway.
//!
//! That means the layout lives here: an external keyboard sends position codes,
//! not characters, and nothing else on the device is going to turn them into
//! text for us.
//!
//! A layout table is the same kind of thing as `ime::punctuation` — a key goes
//! in, text comes out — one stage earlier. This decides which character a key
//! position *is*; the input method then decides what that character becomes.
//! Keeping the two apart is why the Chinese punctuation table needs no German
//! variant: once a `,` arrives here it no longer matters which key sent it.
//!
//! All of this is a pure function of (code, modifiers, layout), so it is tested
//! without a keyboard.

/// Key codes from `linux/input-event-codes.h`, named where they are referred to
/// by name below.
pub mod code {
    pub const ESC: u16 = 1;
    pub const BACKSPACE: u16 = 14;
    pub const TAB: u16 = 15;
    /// Half-width Latin, in every Japanese IME.
    pub const F10: u16 = 68;
    pub const ENTER: u16 = 28;
    pub const LEFTCTRL: u16 = 29;
    pub const LEFTSHIFT: u16 = 42;
    pub const RIGHTSHIFT: u16 = 54;
    pub const LEFTALT: u16 = 56;
    pub const SPACE: u16 = 57;
    pub const RIGHTCTRL: u16 = 97;
    pub const RIGHTALT: u16 = 100;
    /// The ⌘ / Super keys. Mac-layout keyboards send these where a PC layout
    /// sends Ctrl, which is why they are bound at all — see `Mods::chord`.
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
    /// Used to decide whether an input device is a keyboard at all.
    pub const Q: u16 = 16;
}

/// Which modifiers are held.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// The right Alt key, which on a German keyboard is AltGr and is the only
    /// way to reach `@ € [ ] { } \ | ~`. Tracked apart from `alt` because they
    /// are the same key on a US keyboard and different keys on a German one.
    pub altgr: bool,
    /// ⌘. Tracked apart from `ctrl` for the sake of one exception; every other
    /// binding takes either. See `chord`.
    pub meta: bool,
}

impl Mods {
    /// Update on a press or release, returning whether `code` was a modifier.
    pub fn track(&mut self, code: u16, pressed: bool) -> bool {
        match code {
            code::LEFTSHIFT | code::RIGHTSHIFT => self.shift = pressed,
            code::LEFTCTRL | code::RIGHTCTRL => self.ctrl = pressed,
            code::LEFTALT => self.alt = pressed,
            code::RIGHTALT => self.altgr = pressed,
            code::LEFTMETA | code::RIGHTMETA => self.meta = pressed,
            _ => return false,
        }
        true
    }

    /// Whether a shortcut is being asked for, by either of the two keys that
    /// ask for one.
    ///
    /// **A ⌘ key sends `KEY_LEFTMETA` / `KEY_RIGHTMETA`, not either Ctrl**, and
    /// hands that learned these shortcuts on a Mac reach for ⌘ every time. So
    /// the editor accepts both and the letter chords do not have to know which
    /// arrived: `⌘S`, `⌘Z`, `⌘A`, `⌘Q` all work, and so do their Ctrl forms.
    ///
    /// Accepting both also absorbs a multi-OS keyboard, which sends Meta or
    /// Alt from the same physical key depending on the mode it is switched to.
    /// A letter chord must not depend on which mode that is, and more generally
    /// nothing here may depend on which keyboard is attached: the bindings are
    /// written against evdev codes and both conventions are bound, so any
    /// keyboard that sends standard codes works without being recognised.
    ///
    /// **Movement is the exception**, and it is handled in `movement` before
    /// this is consulted: on a Mac ⌘ with an arrow means the line and ⌥ means
    /// the word, which is a distinction this deliberately flattens. Anything
    /// reading `chord` is asking about the letter chords, where the two really
    /// are the same key.
    pub fn chord(self) -> bool {
        self.ctrl || self.meta
    }
}

/// What a keystroke asks the editor to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Insert(char),
    Newline,
    /// A tab. Inserted as spaces, because a literal tab renders at whatever
    /// width the face happens to give it and Markdown nesting counts columns.
    Indent,
    Backspace,
    Delete,
    /// Delete the word behind the cursor, or the selection if there is one.
    DeleteWordBack,
    DeleteWordForward,
    /// ⌘⌫ on a Mac: back to the start of the line in one go.
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
    /// The `Extend*` actions are the `Shift`-held forms of the movements above:
    /// same destination, but dragging the selection along instead of dropping
    /// it.
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
    /// Switch Chinese input on or off. Ctrl+Space, which is what every other
    /// IME on every other platform uses.
    CycleLanguage,
    /// Set the page back around the sentence being written. `Ctrl`/`⌘` + `D`,
    /// which is where iA Writer puts it.
    ToggleFocus,
    /// Open the find bar. `Ctrl`/`⌘` + `F`, as everywhere else. A selection
    /// seeds it, which is the habit every editor has taught.
    Find,
    /// Open the find bar with its second field: what to put in place of each
    /// match. `Ctrl`/`⌘` + `Shift` + `F` — the shifted form of Find, since
    /// `Ctrl`/`⌘` + `H` is Help and `⌥` is bound to nothing here.
    Replace,
    /// Change the match on screen and step to the next. `Ctrl`/`⌘` + `Enter`,
    /// which is "carry out this bar's business" in the applications that have
    /// one. Means nothing outside the replace bar.
    Change,
    /// Change every match. `Ctrl`/`⌘` + `Shift` + `Enter`.
    ChangeAll,
    /// The headings of the open document, to jump between. `Ctrl`/`⌘` +
    /// `Shift` + `O` — the shifted form of Open, and the same key VS Code
    /// gives Go to Symbol.
    Outline,
    /// Show what the keys and the glass do. `Ctrl`/`⌘` + `H`.
    Help,
    /// Clear the panel: a black frame and then the page again. `Ctrl`/`⌘` + `R`,
    /// which is Reload everywhere and is near enough to Refresh.
    Refresh,
    /// Set the page a size larger or smaller. `Ctrl`/`⌘` + `+` and `-`, which
    /// is zoom in every application either writer of this has used.
    Resize(bool),
    /// Take the next margin along. `Ctrl`/`⌘` + `M`, for the setting it moves.
    CycleMargins,
    /// The document list. `Ctrl`/`⌘` + `O`, which is Open everywhere.
    Files,
    /// Start one. `Ctrl`/`⌘` + `N`, likewise — and it skips the list, which is
    /// otherwise two steps to reach a button that is not about any file in it.
    NewDocument,
    /// Settings. `Ctrl`/`⌘` + `,`, which is Preferences everywhere.
    Config,
    /// Wrap the selection — or the word under the cursor — in `**` or `*`.
    /// `Ctrl`/`⌘` + `B` and `I`, as everywhere else.
    Emphasis(&'static str),
    /// Set the current line to this heading level, or back to prose if it is
    /// already at it. `Ctrl`/`⌘` + `1`…`6`.
    Heading(u8),
    /// Commit what was typed as Latin rather than as what it converted into.
    /// `F10`, where every Japanese IME puts it. Does nothing while CJK input is
    /// off, since there is then nothing to convert back.
    CommitTyped,
    /// Abandon whatever is being composed. Bound only because Chinese input
    /// needs a way out of a half-typed syllable; it does nothing while writing.
    Escape,
}

/// A keyboard arrangement.
///
/// **Not a setting.** Nothing selects a layout directly: each language names
/// the one it is written on, so choosing German chooses QWERTZ. A layout on its
/// own would be a second control for a decision the writer only makes once.
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

/// One printable key: `(code, plain, shifted, altgr)`.
///
/// One shape for every layout, so there is one lookup rather than one per
/// keyboard. `altgr` is `None` wherever the key has no third meaning, which on
/// a US keyboard is everywhere.
type Key = (u16, char, char, Option<char>);

/// US QWERTY.
///
/// Only the printable keys. Everything else is handled by name, because a
/// layout table is the wrong place to encode what Enter means.
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
    (52, '.', '>', None),  (53, '/', '?', None),
];

/// German QWERTZ.
///
/// The differences from US are not only Y and Z: every punctuation key moves,
/// the umlauts take the `; ' [` positions, and `@ € [ ] { } \ | ~` are only
/// reachable through AltGr — which is why AltGr is tracked at all.
///
/// Code 86 is the extra key beside the left shift that US boards do not have.
///
/// **Dead keys are not implemented.** `´`, `` ` `` and `^` produce themselves
/// rather than waiting to combine with the next letter, so `´` then `a` gives
/// `´a` rather than `á`. German prose needs accents rarely enough that this is
/// worth knowing about rather than solving; French would need them properly.
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
    let (_, plain, shifted, altgr) = layout.table().iter().find(|(c, ..)| *c == code)?;
    // AltGr wins over shift: on a German keyboard the third legend is what the
    // key is for, and nothing useful sits at AltGr+shift.
    if mods.altgr {
        return *altgr;
    }
    Some(if mods.shift { *shifted } else { *plain })
}

/// How far a movement key travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grain {
    Char,
    Word,
    Line,
}

/// Arrows, `Home`/`End` and the deletions beside them.
///
/// **This is the one place Ctrl and ⌘ are not interchangeable**, so it is
/// handled before `Mods::chord` gets a look in. The keyboard in front of this
/// device is an Apple one and the two conventions disagree about arrows:
///
/// | | macOS | Windows / Linux |
/// |---|---|---|
/// | word | ⌥← ⌥→ | `Ctrl+←` `Ctrl+→` |
/// | line | ⌘← ⌘→ | `Home` `End` |
///
/// Both are bound, because nothing collides: ⌥ and Ctrl mean word, ⌘ and
/// `Home`/`End` mean line. `Shift` extends any of them.
///
/// **AltGr counts as ⌥ here**, and only here. A Mac layout's right Option
/// sends `KEY_RIGHTALT`, which is the same code a German PC layout uses for
/// AltGr — so ignoring it would leave the right-hand Option key dead. It is
/// safe because AltGr's real job is reaching a *third legend* on a printable
/// key, and nothing in this function is printable.
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

        // ⌘↑ and ⌘↓ are the whole document on a Mac. ⌥↑ is by paragraph there
        // and is not bound, so it falls back to a plain line move rather than
        // doing something surprising.
        (code::UP, Grain::Line, false) => Action::DocStart,
        (code::UP, Grain::Line, true) => Action::ExtendDocStart,
        (code::DOWN, Grain::Line, false) => Action::DocEnd,
        (code::DOWN, Grain::Line, true) => Action::ExtendDocEnd,
        (code::UP, _, false) => Action::Up,
        (code::UP, _, true) => Action::ExtendUp,
        (code::DOWN, _, false) => Action::Down,
        (code::DOWN, _, true) => Action::ExtendDown,

        // Bare, `Home` and `End` are the line. With any modifier they are the
        // document — `Ctrl+Home` is that everywhere, and there is no "word"
        // reading of Home for the word grain to claim.
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

/// What a key press means. `None` for keys the editor does not bind.
///
/// Chords are checked before characters, so Ctrl+S saves rather than typing an
/// `s`, and Ctrl here means either Ctrl or ⌘. Alt is not bound to anything: the
/// keyboards in use put it where a thumb lands, and a stray Alt should do
/// nothing rather than something surprising.
pub fn action(code: u16, mods: Mods, layout: Layout) -> Option<Action> {
    // First, because movement is where the modifiers stop being
    // interchangeable and `chord()` would flatten the distinction.
    if let Some(action) = movement(code, mods) {
        return Some(action);
    }
    if mods.chord() {
        if code == code::SPACE {
            // The one binding that does not take ⌘. On macOS, Ctrl+Space is
            // already the input-source shortcut and ⌘Space is Spotlight, so
            // the habit this serves is the Ctrl one. ⌘Space does nothing —
            // which above all means it does not type a space into the draft.
            return mods.ctrl.then_some(Action::CycleLanguage);
        }
        // The replace bar's two commands. Before `character`, which has nothing
        // to say about Enter — the same reason Space is answered above.
        if code == code::ENTER {
            return Some(if mods.shift {
                Action::ChangeAll
            } else {
                Action::Change
            });
        }
        return match character(
            code,
            Mods {
                shift: false,
                ..mods
            },
            layout,
        ) {
            Some('s') => Some(Action::Save),
            Some('z') if mods.shift => Some(Action::Redo),
            Some('z') => Some(Action::Undo),
            Some('y') => Some(Action::Redo),
            Some('q') => Some(Action::Quit),
            // Select all, the consumer reading. `Ctrl+A` for line-start and
            // `Ctrl+E` for line-end were emacs habits from an editor this
            // writer does not use, and `Home`/`End` already do that job.
            Some('a') => Some(Action::SelectAll),
            Some('x') => Some(Action::Cut),
            Some('c') => Some(Action::Copy),
            Some('v') => Some(Action::Paste),
            Some('d') => Some(Action::ToggleFocus),
            Some('f') if mods.shift => Some(Action::Replace),
            Some('f') => Some(Action::Find),
            // **`H`, and not the `/` that ⌘⇧/ has taught.** On QWERTZ `/` is
            // `Shift+7`, and this arm resolves the code with shift forced off —
            // so a German writer would press it and reach `7`, which is bound
            // to nothing. `h` is unshifted and in the same place on both
            // layouts. `F1` is out for the reason `F10` was: a Bluetooth
            // keyboard's function row may be media-first.
            // Shifted first, or the plain arm would swallow it. `H` for
            // highlight, beside Help rather than instead of it — the same
            // shift-variant idiom `O` and `F` already use.
            Some('h') if mods.shift => Some(Action::Emphasis("==")),
            Some('h') => Some(Action::Help),
            // **Every button on the strip has a key.** A strip that hides while
            // you write, reachable only by putting a hand on the glass, is a
            // reason to take a hand off the keyboard four times an hour. `O` is
            // Open and `,` is Preferences in every application either writer of
            // this has used; both are unshifted and in the same place on QWERTZ.
            Some('r') => Some(Action::Refresh),
            // Zoom, matched on the character so both layouts are covered by
            // one line: `-` is code 12 on QWERTY and 53 on QWERTZ, and the key
            // that makes bigger is `=` on one and an unshifted `+` on the
            // other. `+` is also accepted on QWERTY, where it takes Shift —
            // nobody looks at the legend before pressing it.
            Some('-') => Some(Action::Resize(false)),
            Some('=' | '+') => Some(Action::Resize(true)),
            // The page's other half. Zoom's own keys are a two-ended step and
            // there are three margins, so this cycles instead — the letter the
            // setting is named for, unshifted and in the same place on both
            // layouts.
            Some('m') => Some(Action::CycleMargins),
            // Open a document, and — shifted — a place inside the open one.
            Some('o') if mods.shift => Some(Action::Outline),
            Some('o') => Some(Action::Files),
            Some('n') => Some(Action::NewDocument),
            Some(',') => Some(Action::Config),
            Some('b') => Some(Action::Emphasis("**")),
            Some('i') => Some(Action::Emphasis("*")),
            // The digits are heading levels here. They pick IME candidates
            // only when a word is being composed, and a chord is never that —
            // `compose` abandons the composition on one.
            Some(c @ '1'..='6') => Some(Action::Heading(c as u8 - b'0')),
            _ => None,
        };
    }

    match code {
        code::ESC => Some(Action::Escape),
        code::F10 => Some(Action::CommitTyped),
        // Enter commits the composition as it stands; Shift+Enter commits the
        // letters that were struck instead. The same key, one modifier, and an
        // adjacent meaning — and unlike `F10` it exists on every keyboard,
        // which a function row that might be media-first does not.
        //
        // Outside a composition this is an ordinary line break, so nothing is
        // taken away from a writer who never turns CJK input on.
        code::ENTER if mods.shift => Some(Action::CommitTyped),
        code::ENTER => Some(Action::Newline),
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

    /// Everything the Markdown parser treats as markup has to be typeable — on
    /// *every* layout, or the editor would be less capable on one keyboard than
    /// another. On German most of these are AltGr legends, which is what makes
    /// AltGr load-bearing rather than a nicety.
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
        // Ctrl+W would otherwise insert a `w`.
        assert_eq!(action(17, ctrl(), Layout::Us), None);
    }

    /// The shifted forms of Find and Open, on both layouts. Both are the
    /// deeper version of the key beside them, and neither may take the
    /// unshifted one's meaning.
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

    /// Enter under a chord is the replace bar's own command. Without a chord it
    /// stays a line break and — with Shift — the letters as typed, which is
    /// what a Japanese writer presses several times a paragraph.
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

    /// **Every button on the strip has a key, on both layouts.** A strip that
    /// hides while you write and is reachable only by putting a hand on the
    /// glass is a reason to take a hand off the keyboard four times an hour.
    ///
    /// German is asserted beside US because that is the trap these four sit in:
    /// this arm resolves the code with shift forced off, so a binding that
    /// needs Shift on QWERTZ is dead there. `Ctrl+/` was the first choice for
    /// Help — ⌘⇧/ is what macOS teaches — and `/` is `Shift+7` on QWERTZ, so a
    /// German writer would have reached `7` and hit nothing.
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
            // And still types its letter with no chord held, or the binding
            // would have cost a character.
            assert!(matches!(
                action(code, plain(), Layout::Us),
                Some(Action::Insert(_))
            ));
        }
    }

    /// The page has two settings and both are on the keyboard. `M` is
    /// unshifted and in the same place on either layout, which is what this arm
    /// needs — it resolves the character with shift forced off.
    #[test]
    fn the_margins_cycle_from_either_chord_on_either_layout() {
        for layout in [Layout::Us, Layout::German] {
            for mods in [ctrl(), meta()] {
                assert_eq!(action(50, mods, layout), Some(Action::CycleMargins));
            }
            // And still types its letter with no chord held.
            assert!(matches!(
                action(50, plain(), layout),
                Some(Action::Insert('m'))
            ));
        }
    }

    /// **The keyboard this app is used with is an Apple one, and ⌘ was inert.**
    /// `Mods::track` matched neither meta code, so every shortcut quietly did
    /// nothing under the key the writer's hands actually reach for. Asserting
    /// the two are interchangeable is stronger than listing the bindings again,
    /// because a binding added later is covered without anyone remembering to.
    #[test]
    fn command_does_everything_control_does() {
        // Save, undo, quit, line start — and `w`, which is bound to nothing and
        // must be equally unbound both ways.
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

        /// Both conventions at once: ⌥ is the habit on the Apple keyboard in
        /// front of this device, `Ctrl` is the habit everywhere else, and there
        /// is no reason to make anyone choose.
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

        /// The right-hand Option key sends the same code a German keyboard uses
        /// for AltGr, so ignoring it would leave half of ⌥ dead.
        #[test]
        fn the_right_option_key_moves_by_word_too() {
            assert_eq!(
                action(code::RIGHT, altgr(), Layout::Us),
                Some(Action::WordRight)
            );
        }

        /// And the reason that is safe: AltGr's real job is a third legend on a
        /// *printable* key, which no movement key is. This is the assertion
        /// that would fail if the two ever started fighting.
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

        /// ⌘ is the one modifier that means something different from Ctrl.
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

        /// Consumer conventions, not editor ones: `Ctrl+A` selects all, and
        /// the line start is `Home`.
        #[test]
        fn control_a_selects_all_and_home_still_goes_to_the_line_start() {
            assert_eq!(action(30, ctrl(), Layout::Us), Some(Action::SelectAll));
            assert_eq!(action(30, meta(), Layout::Us), Some(Action::SelectAll));
            assert_eq!(
                action(code::HOME, plain(), Layout::Us),
                Some(Action::LineStart)
            );
            // Ctrl+E went with it; it must not linger as a half-binding.
            assert_eq!(action(18, ctrl(), Layout::Us), None);
        }

        /// Both conventions again, and the one that unblocks reopening a draft
        /// at its end: without a quick way back to the top, restoring the
        /// cursor just trades one trek for another.
        #[test]
        fn any_modifier_on_home_or_end_means_the_whole_document() {
            for mods in [ctrl(), alt(), meta()] {
                assert_eq!(action(code::HOME, mods, Layout::Us), Some(Action::DocStart));
                assert_eq!(action(code::END, mods, Layout::Us), Some(Action::DocEnd));
            }
            // ⌘↑ and ⌘↓, the Mac spelling of the same thing.
            assert_eq!(action(code::UP, meta(), Layout::Us), Some(Action::DocStart));
            assert_eq!(action(code::DOWN, meta(), Layout::Us), Some(Action::DocEnd));
            // Bare, they are still the line — and ⌥↑ has no paragraph move to
            // give, so it stays an ordinary line up rather than surprising.
            assert_eq!(
                action(code::HOME, plain(), Layout::Us),
                Some(Action::LineStart)
            );
            assert_eq!(action(code::UP, alt(), Layout::Us), Some(Action::Up));
        }

        /// The clipboard chords, under both keys — and on a German keyboard,
        /// where X, C and V happen not to move but the lookup goes through the
        /// layout table all the same.
        #[test]
        fn cut_copy_and_paste_are_bound_on_both_modifiers_and_layouts() {
            for layout in [Layout::Us, Layout::German] {
                for mods in [ctrl(), meta()] {
                    assert_eq!(action(45, mods, layout), Some(Action::Cut));
                    assert_eq!(action(46, mods, layout), Some(Action::Copy));
                    assert_eq!(action(47, mods, layout), Some(Action::Paste));
                }
                // And unmodified they still type.
                assert_eq!(action(46, plain(), layout), Some(Action::Insert('c')));
            }
        }
    }

    /// The one deliberate exception, and the failure it prevents is not the
    /// obvious one: a writer reaching for Spotlight must not have a space
    /// appear in the draft.
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
        // Both sides of the keyboard work.
        assert!(mods.track(code::RIGHTCTRL, true));
        assert!(mods.ctrl);
        // Including ⌘, which must not fall through here as an ordinary key.
        assert!(mods.track(code::LEFTMETA, true));
        assert!(mods.meta);
        assert!(mods.track(code::RIGHTMETA, false));
        assert!(!mods.meta);
        // A normal key is not a modifier.
        assert!(!mods.track(30, true));
    }

    #[test]
    fn alt_alone_types_the_plain_character() {
        // Alt is unbound, so it must not swallow the keystroke.
        let alt = Mods {
            alt: true,
            ..Mods::default()
        };
        assert_eq!(action(30, alt, Layout::Us), Some(Action::Insert('a')));
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

        /// The swap that would have been obvious the moment any German was
        /// typed, and the reason this exists at all.
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

        /// Without AltGr a German keyboard cannot reach the characters Markdown
        /// is written with, so this is not a convenience.
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

        /// A key with no third legend produces nothing under AltGr rather than
        /// falling back to its plain character, which would type letters into
        /// the document whenever AltGr was held down.
        #[test]
        fn altgr_on_a_key_without_one_types_nothing() {
            let altgr = Mods {
                altgr: true,
                ..Mods::default()
            };
            assert_eq!(character(30, altgr, Layout::German), None);
        }

        /// The extra key beside the left shift, which US boards do not have and
        /// which is where German keeps `<`, `>` and the Markdown table pipe.
        #[test]
        fn the_key_us_boards_lack_is_bound() {
            assert_eq!(character(86, plain(), Layout::German), Some('<'));
            assert_eq!(character(86, shift(), Layout::German), Some('>'));
            assert_eq!(character(86, plain(), Layout::Us), None);
        }

        /// Punctuation moves, and the Chinese punctuation table keys on the
        /// character rather than the code — so a German comma still becomes ，
        /// with no German-specific mapping anywhere.
        #[test]
        fn punctuation_moves_but_still_arrives_as_itself() {
            assert_eq!(character(51, plain(), Layout::German), Some(','));
            assert_eq!(character(52, plain(), Layout::German), Some('.'));
            // Semicolon and colon are shifted here, unshifted on US.
            assert_eq!(character(51, shift(), Layout::German), Some(';'));
            assert_eq!(character(52, shift(), Layout::German), Some(':'));
            // And the question mark moves onto the ß key.
            assert_eq!(character(12, shift(), Layout::German), Some('?'));
        }
    }
}
