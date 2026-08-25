//! CJK input: two languages through the device's own predictor plugins, and
//! one karyll composes itself.
//!
//! [`Korean`] is the composed one — a Hangul syllable is arithmetic on a code
//! point, so it is three tables and a state machine, all under `cargo test`.
//! The rest of this file is a binding.
//!
//! The device ships an IME per locale under
//! `/usr/share/keyboard/<locale>/libpredictor.so.1` — engine, dictionaries
//! and keyboard databases. Two are driven: **Chinese** is `zh_CN`, XT9 over
//! `libxt9a`; **Japanese** is `ja`, Omron iWnn over `libwlf` with ICU doing
//! romaji to kana from `hiragana_rules.txt`. The plugins share one ABI —
//! `libkb`'s — so [`Plugin`] is written once.
//!
//! `load(host)` performs the entire engine initialisation — for Chinese
//! `ET9CPSysInit`, `ET9CPLdbInit`, `ET9CPSetInputMode`,
//! `ET9CPSetFullSentence`, `ET9CPUdbActivate`, the `ET9KDB_*` setup and the
//! `mmap` of both databases; for Japanese `wlf_init`, `wlf_set_state`,
//! `wlf_load_lang` on `JA.conf` and `wlf_set_active_lang` — and returns a
//! 48-byte block of function pointers, a *session* API:
//!
//! | slot | signature |
//! |---|---|
//! | `+0x00` | `prv_unload(userData) -> int` |
//! | `+0x04` | `prv_open(flags, userData) -> int` — begin a session |
//! | `+0x08` | `prv_close(userData) -> int` — ends it, writes the user dictionary |
//! | `+0x0c` | `prv_set_surround(str, position, userData) -> int` |
//! | `+0x10` | `(out: *mut c_char, capacity: usize)` — the composition so far |
//! | `+0x14` | `prv_key_handler(key: u32, userData) -> int` |
//! | `+0x18` | commit: `(index: u32, userData) -> int` |
//! | `+0x1c` | `prv_get_candidate_list(out: *mut *mut c_char, count: *mut u32, userData)` |
//!
//! The calling convention:
//!
//! * **No `self`/context argument.** Every slot resolves its own context
//!   PC-relative from the plugin's `.bss`; the pointer `load()` stores at
//!   `+0x2c` is its own bookkeeping.
//! * **`userData` is the last argument**, and the plugin only logs it.
//!   [`USER_DATA`] is what karyll sends.
//! * **`+0x00` and `+0x08` are the teardown pair.** Called first, they close
//!   and unload the engine `load()` built, and every call after them runs on
//!   freed memory.
//!
//! **The order is the ABI. The addresses are not**: the same function sits
//! somewhere different in every firmware, and two builds of one language can
//! agree on all their code and still differ in `.bss`. No address is written
//! down here — the plugin is found in `/proc/self/maps` from a pointer it
//! produced itself (see [`Mapping`]), and engine state is found by asking
//! the running engine: the phonetic context by the magic it stamps on
//! itself, the pending keys by typing at it and watching which words move.
//!
//! All slots return `int`, 0 = ok — **except `+0x1c`, which returns
//! nothing**: its exit path never sets `r0`, and the out-count is the
//! answer. It fills the caller's array with pointers **borrowed** from the
//! plugin's own fixed-stride candidate table, valid until the next call, and
//! overwrites `*count` with how many it produced. Commit takes an index into
//! that same table, so the caller holds the text it committed; the host
//! callback reporting it is confirmation, not delivery.
//!
//! **A commit need not consume the whole reading.** Chinese's commit slot
//! re-feeds every key past the chosen phrase through `prv_key_handler` and
//! returns composing the remainder; Japanese's ends by zeroing its
//! composition buffer. Either way the caller asks what is left — a remainder
//! left inside the engine joins the front of the next word typed.
//!
//! The key is a **Unicode codepoint** — ASCII for pinyin and romaji, not a
//! keycode and not an index. Each engine handles a couple of keys itself.

use std::ffi::{c_char, c_void};
use std::ops::Range;

/// Which language's rules apply — the input method, and the punctuation that
/// goes with it.
///
/// Not the same thing as the plugin: Simplified and Traditional Chinese are one
/// engine and one set of rules, differing only in what the candidates are
/// converted to on the way out. Korean names no plugin at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Chinese,
    Japanese,
    /// Korean, composed by [`Korean`] in this process.
    Korean,
}

/// What the editor needs from an input method.
///
/// Small on purpose, and a trait so the contract can be tested against a stub:
/// the real engine is a device file that cannot be redistributed, so anything
/// only exercisable through it would be exercised once, on hardware, by hand.
pub trait Ime {
    /// Feed one key and return the candidates now available, best first.
    fn key(&mut self, key: char) -> Vec<String>;

    /// Accept candidate `index`, and hand back whatever reading it left
    /// unconverted: a candidate can cover the front of the reading, and the
    /// word then carries on with the rest. `None` is a finished word. The
    /// committed text is the candidate the caller selected, and is not
    /// returned.
    fn commit(&mut self, index: usize) -> Option<Rest>;

    /// Abandon whatever is being composed.
    fn clear(&mut self);

    /// Hand back Traditional characters. The device's one Chinese keyboard
    /// database, `zh_CN.ldb`, is Simplified pinyin; Traditional is the same
    /// candidates converted.
    fn set_traditional(&mut self, traditional: bool);

    /// The engine's own reading of what has been typed, when it keeps one:
    /// romaji has become kana — `nihon` shows にほん — and only the engine
    /// holds the transliteration. `None` means "show what was typed".
    fn preedit(&self) -> Option<String> {
        None
    }
}

/// What an engine is still composing after a candidate covered only the front
/// of the reading.
#[derive(Debug, PartialEq, Eq)]
pub struct Rest {
    /// The part that was not converted: pinyin in Chinese, kana in Japanese.
    pub reading: String,
    /// What that reading offers now, best first.
    pub candidates: Vec<String>,
}

/// How many candidates are on screen at once. Ten fits a bar across the page
/// and matches the number row, which is how they are chosen.
pub const WANTED: usize = 10;

/// How many to keep behind them, for paging. **Chinese clamps its answer to
/// the count it is asked for**, and each candidate past that count costs an
/// `ET9CPGetPhrase` on every keystroke.
pub const KEPT: usize = 5 * WANTED;

/// CJK punctuation for the ASCII key that produces it.
///
/// Neither engine supplies any: `hiragana_rules.txt` maps letters and
/// nothing else, so a `.` typed in Japanese mode reaches the preedit as a
/// full stop and stays one. Every CJK mark comes from here.
///
/// **macOS's CJK inputs are the reference** where a mapping is arguable.
/// Not exhaustive: `-`, `/`, `=`, `+`, `%`, `#`, `&`, `*` and `$` stay
/// ASCII — their CJK forms are rare in prose.
fn punctuation(script: Script, key: char) -> Option<Punct> {
    let fixed = |s| Some(Punct::Fixed(s));
    // Shared by both: sentence marks differing only in the comma, and the
    // fullwidth parentheses.
    let common = || match key {
        '.' => fixed("。"),
        '?' => fixed("？"),
        '!' => fixed("！"),
        ':' => fixed("："),
        ';' => fixed("；"),
        '(' => fixed("（"),
        ')' => fixed("）"),
        '"' => Some(Punct::Paired("“", "”")),
        '\'' => Some(Punct::Paired("‘", "’")),
        _ => None,
    };
    match script {
        // **Korean sets its punctuation in ASCII**: the full stop, comma and
        // quotation marks Korean prose takes are the ones on the key.
        Script::Korean => None,
        Script::Chinese => match key {
            ',' => fixed("，"),
            // The enumeration comma, which Chinese uses between list items
            // where the ordinary comma separates clauses.
            '\\' => fixed("、"),
            // Both are doubled in Chinese typography: one ellipsis is three
            // dots of a six-dot mark, and a lone dash is a hyphen.
            '^' => fixed("……"),
            '_' => fixed("——"),
            '<' => fixed("《"),
            '>' => fixed("》"),
            '[' => fixed("【"),
            ']' => fixed("】"),
            // Shift+bracket gives the corner brackets, as macOS's Simplified
            // Chinese input does. They are not interchangeable with 【】: 「」
            // quote speech and titles, 【】 mark editorial insertions.
            '{' => fixed("「"),
            '}' => fixed("」"),
            _ => common(),
        },
        Script::Japanese => match key {
            // 読点, not the fullwidth comma. Japanese sets ， only in
            // horizontal technical writing; prose takes 、.
            ',' => fixed("、"),
            // The corner brackets are Japanese's primary quotation marks and
            // sit on the unshifted bracket keys, where a JIS keyboard has them
            // and where macOS puts them. 『』 quote inside a quotation, and
            // title works.
            '[' => fixed("「"),
            ']' => fixed("」"),
            '{' => fixed("『"),
            '}' => fixed("』"),
            '<' => fixed("〈"),
            '>' => fixed("〉"),
            '~' => fixed("〜"),
            // Japanese doubles neither the dash nor the ellipsis.
            _ => common(),
        },
    }
}

/// A mark that is always the same, or one that alternates open and closed.
enum Punct {
    Fixed(&'static str),
    Paired(&'static str, &'static str),
}

/// The quote-pairing state, per document. Chinese quotation marks are
/// directional with one key per pair, so the key alternates.
#[derive(Default)]
pub struct Punctuation {
    double_open: bool,
    single_open: bool,
}

impl Punctuation {
    /// The CJK text for an ASCII punctuation key, advancing the pairing state
    /// where the mark is directional.
    pub fn resolve(&mut self, script: Script, key: char) -> Option<&'static str> {
        match punctuation(script, key)? {
            Punct::Fixed(s) => Some(s),
            Punct::Paired(open, close) => {
                let state = if key == '"' {
                    &mut self.double_open
                } else {
                    &mut self.single_open
                };
                *state = !*state;
                Some(if *state { open } else { close })
            }
        }
    }
}

/// What a keystroke means while CJK input is switched on.
#[derive(Debug, PartialEq, Eq)]
pub enum Compose {
    /// Send this to the engine.
    Feed(char),
    /// Compose this key into the Hangul syllable under way, through
    /// [`Korean::key`]. [`Script::Korean`] only: that answers with committed
    /// text where an engine answers with candidates.
    Jamo(char),
    /// Take one jamo back off the syllable — 앉 → 안 → 아. [`Script::Korean`]
    /// only, through [`Korean::backspace`].
    Decompose,
    /// **The composition is finished text.** Commit it and let the editor have
    /// the keystroke as usual. [`Script::Korean`] only: a half-typed Hangul
    /// syllable is correct Korean, so every key that is not a jamo ends it and
    /// then means what it always means.
    Finish,
    /// Take candidate `n` of the ten on screen, counting from zero.
    Select(usize),
    /// Show the ten candidates after the ones on screen, or the ten before.
    NextPage,
    PreviousPage,
    /// Insert the Chinese form of this ASCII punctuation key.
    Punctuate(char),
    /// Insert the pinyin exactly as typed and stop composing; Enter's case.
    CommitRaw,
    /// Finish the word under way and add this Latin character to the document
    /// directly, without the engine seeing it.
    Latin(char),
    /// Insert the letters as they were struck. Pinyin's preedit *is* the
    /// letters; romaji has become kana, and the letters survive only in
    /// `typed`.
    CommitTyped,
    /// Abandon the composition. The keystroke is consumed.
    Cancel,
    /// Nothing to do with input — the editor should handle it as usual.
    Pass,
}

/// The prolonged sound mark, which lengthens the preceding kana — ラーメン.
/// Fed to the engine as this code point: `prv_key_handler` tests for it, and
/// the romaji rules map no hyphen onto it.
const CHOONPU: char = 'ー';

/// Decide what a keystroke means while CJK input is on.
///
/// `composing` is whether anything has been typed towards a word yet, and it
/// changes almost every rule: a digit is a candidate number mid-word and a
/// digit otherwise, space converts mid-word and is a space otherwise.
///
/// Pure: tested without an engine, a window or a keyboard.
pub fn compose(action: &crate::keymap::Action, composing: bool, script: Script) -> Compose {
    use crate::keymap::Action;

    // **A Korean keyboard types Korean.** Every letter is a jamo, capitals
    // included — the tense consonants — and Latin is reached by switching
    // source (`Ctrl + Space`). Every other key finishes the syllable and
    // goes on to mean what it means; no candidate list, no pages.
    if script == Script::Korean {
        return match action {
            Action::Insert(c) if jamo_for(*c).is_some() => Compose::Jamo(*c),
            Action::Backspace if composing => Compose::Decompose,
            _ if composing => Compose::Finish,
            _ => Compose::Pass,
        };
    }

    match action {
        // **A capital is never CJK input** — pinyin and romaji are written
        // in lower case, and a capital is Latin for the page. Before the
        // letter rule, and unconditional on `composing`.
        Action::Insert(c) if c.is_ascii_uppercase() => Compose::Latin(*c),

        // Letters always start or continue a syllable, in both languages: the
        // engines take lowercase ASCII and do pinyin or romaji themselves.
        Action::Insert(c) if c.is_ascii_alphabetic() => Compose::Feed(c.to_ascii_lowercase()),

        // The apostrophe is pinyin's own syllable separator — xi'an against
        // xian — so mid-word it is that, and only otherwise a quotation mark.
        // Japanese romaji has no such separator, so there it stays punctuation.
        Action::Insert('\'') if composing && script == Script::Chinese => Compose::Feed('\''),

        // A hyphen mid-word is the chōonpu, which is not optional in Japanese —
        // カレー and コーヒー need it, and it is the one non-letter that is part
        // of ordinary romaji input rather than punctuation.
        Action::Insert('-') if composing && script == Script::Japanese => Compose::Feed(CHOONPU),

        // Punctuation is CJK whenever the mode is on, composing or not.
        Action::Insert(c) if punctuation(script, *c).is_some() => Compose::Punctuate(*c),

        _ if !composing => Compose::Pass,

        // The number row picks a candidate; 0 is the tenth.
        Action::Insert(c @ '1'..='9') => Compose::Select(*c as usize - '1' as usize),
        Action::Insert('0') => Compose::Select(9),

        // **Space is the one rule the two languages disagree on.** Pinyin
        // predicts as it goes and space accepts the best candidate; Japanese
        // converts on space — `prv_key_handler` routes 0x20 to start and
        // advance the selection.
        Action::Insert(' ') => match script {
            Script::Chinese => Compose::Select(0),
            Script::Japanese => Compose::Feed(' '),
            Script::Korean => Compose::Finish,
        },

        // Backspace goes to the engine: Chinese routes 0x08 to
        // `ET9ClearOneSymb`, Japanese truncates its preedit one character
        // back, and either re-predicts from what is left.
        Action::Backspace => Compose::Feed('\u{8}'),

        // **The arrows page the candidates rather than the document.** The
        // bar is one row, so both axes page, and so do the page keys.
        Action::Right | Action::Down | Action::PageDown => Compose::NextPage,
        Action::Left | Action::Up | Action::PageUp => Compose::PreviousPage,

        Action::Newline => Compose::CommitRaw,
        Action::Escape => Compose::Cancel,

        // **The way out of kana for a Latin word**: `F10` converts the
        // reading to half-width Latin, in both languages.
        Action::CommitTyped => Compose::CommitTyped,

        // Anything else — a Ctrl chord, the ends of a line, a jump — abandons
        // the composition and is consumed.
        _ => Compose::Cancel,
    }
}

/// The 19 초성, in the order the composition formula counts them.
const INITIALS: [char; 19] = [
    'ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ',
    'ㅌ', 'ㅍ', 'ㅎ',
];

/// The 21 중성.
const MEDIALS: [char; 21] = [
    'ㅏ', 'ㅐ', 'ㅑ', 'ㅒ', 'ㅓ', 'ㅔ', 'ㅕ', 'ㅖ', 'ㅗ', 'ㅘ', 'ㅙ', 'ㅚ', 'ㅛ', 'ㅜ', 'ㅝ', 'ㅞ',
    'ㅟ', 'ㅠ', 'ㅡ', 'ㅢ', 'ㅣ',
];

/// The 27 종성 a syllable can end in, counted from one: index zero of the
/// formula means no 받침 at all.
///
/// ㄸ, ㅃ and ㅉ are absent: Korean never writes them there, so [`Korean::key`]
/// opens a new syllable on one.
const CODAS: [char; 27] = [
    'ㄱ', 'ㄲ', 'ㄳ', 'ㄴ', 'ㄵ', 'ㄶ', 'ㄷ', 'ㄹ', 'ㄺ', 'ㄻ', 'ㄼ', 'ㄽ', 'ㄾ', 'ㄿ', 'ㅀ', 'ㅁ',
    'ㅂ', 'ㅄ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅊ', 'ㅋ', 'ㅌ', 'ㅍ', 'ㅎ',
];

/// Where the precomposed syllables start: 가.
const FIRST_SYLLABLE: u32 = 0xAC00;

/// The vowels two keys make, and the pair each is made of.
///
/// 두벌식 has one key per simple vowel, so ㅘ is typed ㅗ then ㅏ, and
/// [`Korean::backspace`] takes it back to the pair.
const COMPOUND_MEDIALS: [(char, char, char); 7] = [
    ('ㅗ', 'ㅏ', 'ㅘ'),
    ('ㅗ', 'ㅐ', 'ㅙ'),
    ('ㅗ', 'ㅣ', 'ㅚ'),
    ('ㅜ', 'ㅓ', 'ㅝ'),
    ('ㅜ', 'ㅔ', 'ㅞ'),
    ('ㅜ', 'ㅣ', 'ㅟ'),
    ('ㅡ', 'ㅣ', 'ㅢ'),
];

/// The 받침 two consonants make.
///
/// **Every one of these splits under a vowel**, and only its tail moves: 앉
/// followed by ㅏ is 안자. The tense finals ㄲ and ㅆ are absent — they are
/// single keys on the shifted row of [`jamo_for`].
const COMPOUND_CODAS: [(char, char, char); 11] = [
    ('ㄱ', 'ㅅ', 'ㄳ'),
    ('ㄴ', 'ㅈ', 'ㄵ'),
    ('ㄴ', 'ㅎ', 'ㄶ'),
    ('ㄹ', 'ㄱ', 'ㄺ'),
    ('ㄹ', 'ㅁ', 'ㄻ'),
    ('ㄹ', 'ㅂ', 'ㄼ'),
    ('ㄹ', 'ㅅ', 'ㄽ'),
    ('ㄹ', 'ㅌ', 'ㄾ'),
    ('ㄹ', 'ㅍ', 'ㄿ'),
    ('ㄹ', 'ㅎ', 'ㅀ'),
    ('ㅂ', 'ㅅ', 'ㅄ'),
];

/// The jamo a key writes under 두벌식, or `None` for a key that is not one.
///
/// **두벌식 is the standard Korean arrangement**, defined against QWERTY:
/// consonants under the left hand, vowels under the right.
///
/// ```text
/// q ㅂ  w ㅈ  e ㄷ  r ㄱ  t ㅅ  y ㅛ  u ㅕ  i ㅑ  o ㅐ  p ㅔ
/// a ㅁ  s ㄴ  d ㅇ  f ㄹ  g ㅎ  h ㅗ  j ㅓ  k ㅏ  l ㅣ
/// z ㅋ  x ㅌ  c ㅊ  v ㅍ  b ㅠ  n ㅜ  m ㅡ
/// ```
///
/// `key` is the character [`crate::keymap::Layout`] resolved, not a scan code,
/// so this holds for whatever keyboard is attached.
///
/// **Nothing but the letters.** Korean sets its punctuation in ASCII, so every
/// other key writes what it says and `None` leaves it to the editor.
fn jamo_for(key: char) -> Option<char> {
    Some(match key {
        'q' => 'ㅂ',
        'w' => 'ㅈ',
        'e' => 'ㄷ',
        'r' => 'ㄱ',
        't' => 'ㅅ',
        'y' => 'ㅛ',
        'u' => 'ㅕ',
        'i' => 'ㅑ',
        'o' => 'ㅐ',
        'p' => 'ㅔ',
        'a' => 'ㅁ',
        's' => 'ㄴ',
        'd' => 'ㅇ',
        'f' => 'ㄹ',
        'g' => 'ㅎ',
        'h' => 'ㅗ',
        'j' => 'ㅓ',
        'k' => 'ㅏ',
        'l' => 'ㅣ',
        'z' => 'ㅋ',
        'x' => 'ㅌ',
        'c' => 'ㅊ',
        'v' => 'ㅍ',
        'b' => 'ㅠ',
        'n' => 'ㅜ',
        'm' => 'ㅡ',
        // Shift carries the five tense consonants and the two iotised vowels,
        // which is the whole of the shifted row.
        'Q' => 'ㅃ',
        'W' => 'ㅉ',
        'E' => 'ㄸ',
        'R' => 'ㄲ',
        'T' => 'ㅆ',
        'O' => 'ㅒ',
        'P' => 'ㅖ',
        // 두벌식 leaves the rest of the shifted row unassigned, and every
        // capital on it writes the jamo under the finger.
        c if c.is_ascii_uppercase() => return jamo_for(c.to_ascii_lowercase()),
        _ => return None,
    })
}

/// The Korean input method: 두벌식 in, Hangul out.
///
/// **No dictionary, no candidate list and nothing that can fail to load.** A
/// syllable is three slots, and the code point is the formula:
///
/// ```text
/// syllable = 0xAC00 + (초성 × 21 + 중성) × 28 + 종성
/// ```
///
/// It does not implement [`Ime`]: that trait is a session with an engine that
/// offers candidates and is told which one to take. [`Korean::key`] offers
/// none, and commits without being asked.
///
/// **The 받침 migrates.** A final belongs to the syllable it was typed into
/// until a vowel arrives, and then to the next one — ㅎㅏㄴ is 한 and ㅎㅏㄴㅏ is
/// 하나. So [`Korean::key`] hands back two things: text that is finished with,
/// and the syllable under construction. The finished half is empty for most
/// keystrokes.
///
/// The three tables hold **compatibility jamo**, which is what a lone jamo is
/// written as. The conjoining jamo of U+1100 appear nowhere here: a
/// half-composed syllable shows as ㄱ.
///
/// A syllable holds a lone consonant (ㄱ), a lone vowel (ㅏ), or all three
/// slots — and a 받침 only once the other two are filled, which keeps
/// [`Korean::preedit`] a single code point wherever one exists. Empty is not
/// composing.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Korean {
    initial: Option<char>,
    medial: Option<char>,
    coda: Option<char>,
}

impl Korean {
    /// Compose one key, and hand back the text this keystroke finished.
    ///
    /// Empty for the keystrokes that go on building the same syllable, which
    /// is most of them; what they build is [`Korean::preedit`]. It carries a
    /// syllable when the one in hand is full and a new one starts, and in the
    /// 받침 case, where a vowel takes the final away.
    ///
    /// `key` is what the keyboard resolved. One that [`jamo_for`] does not
    /// know leaves the composition alone and hands back the empty string;
    /// [`Compose::Finish`] is that key's case.
    pub fn key(&mut self, key: char) -> String {
        let Some(jamo) = jamo_for(key) else {
            return String::new();
        };
        if MEDIALS.contains(&jamo) {
            self.vowel(jamo)
        } else {
            self.consonant(jamo)
        }
    }

    /// Take one jamo back off the syllable: 앉 → 안 → 아 → ㅇ → nothing.
    ///
    /// **A Korean backspace decomposes.** A compound falls back to its head,
    /// and each slot empties in turn. `false` says the syllable was empty and
    /// the keystroke belongs to the document.
    pub fn backspace(&mut self) -> bool {
        if let Some(coda) = self.coda {
            self.coda = split(&COMPOUND_CODAS, coda).map(|(head, _)| head);
        } else if let Some(medial) = self.medial {
            self.medial = split(&COMPOUND_MEDIALS, medial).map(|(head, _)| head);
        } else if self.initial.is_some() {
            self.initial = None;
        } else {
            return false;
        }
        true
    }

    /// The syllable under construction, as it appears on the page.
    ///
    /// A complete syllable is one precomposed code point. One missing a slot
    /// is the jamo typed so far — ㄱ, ㅏ — and both are correct Korean text.
    pub fn preedit(&self) -> String {
        if let (Some(initial), Some(medial)) = (self.initial, self.medial)
            && let Some(syllable) = compose_syllable(initial, medial, self.coda)
        {
            return syllable.to_string();
        }
        [self.initial, self.medial, self.coda]
            .into_iter()
            .flatten()
            .collect()
    }

    /// Finish the syllable in hand and hand back its text.
    pub fn take(&mut self) -> String {
        let text = self.preedit();
        self.clear();
        text
    }

    /// Abandon the syllable.
    pub fn clear(&mut self) {
        *self = Korean::default();
    }

    /// A consonant is the 받침 of the syllable in hand where one fits, and the
    /// initial of a new syllable everywhere else.
    fn consonant(&mut self, c: char) -> String {
        if self.initial.is_some() && self.medial.is_some() {
            match self.coda {
                None if CODAS.contains(&c) => {
                    self.coda = Some(c);
                    return String::new();
                }
                Some(held) => {
                    if let Some(joined) = join(&COMPOUND_CODAS, held, c) {
                        self.coda = Some(joined);
                        return String::new();
                    }
                }
                None => {}
            }
        }
        let done = self.take();
        self.initial = Some(c);
        done
    }

    /// A vowel fills the empty 중성, compounds with the one there, or takes the
    /// 받침 away into a syllable of its own.
    fn vowel(&mut self, v: char) -> String {
        // **The 받침 belongs to the vowel's syllable.** It becomes that
        // syllable's initial, and a compound sends only its tail.
        if let Some(coda) = self.coda {
            let (stays, moves) = match split(&COMPOUND_CODAS, coda) {
                Some((head, tail)) => (Some(head), tail),
                None => (None, coda),
            };
            self.coda = stays;
            let done = self.take();
            self.initial = Some(moves);
            self.medial = Some(v);
            return done;
        }
        let Some(held) = self.medial else {
            self.medial = Some(v);
            return String::new();
        };
        if let Some(joined) = join(&COMPOUND_MEDIALS, held, v) {
            self.medial = Some(joined);
            return String::new();
        }
        // Two vowels that do not compound are two syllables, and the second
        // opens with an empty 초성.
        let done = self.take();
        self.medial = Some(v);
        done
    }
}

/// The code point for three slots, or `None` for a combination that is not a
/// syllable.
fn compose_syllable(initial: char, medial: char, coda: Option<char>) -> Option<char> {
    let initial = INITIALS.iter().position(|c| *c == initial)?;
    let medial = MEDIALS.iter().position(|c| *c == medial)?;
    let coda = match coda {
        Some(coda) => CODAS.iter().position(|c| *c == coda)? + 1,
        None => 0,
    };
    let index = (initial * MEDIALS.len() + medial) * (CODAS.len() + 1) + coda;
    char::from_u32(FIRST_SYLLABLE + index as u32)
}

/// The jamo `a` and `b` make together, if they make one.
fn join(pairs: &[(char, char, char)], a: char, b: char) -> Option<char> {
    pairs
        .iter()
        .find(|(head, tail, _)| *head == a && *tail == b)
        .map(|(_, _, joined)| *joined)
}

/// The pair a compound jamo is made of, or `None` if it is simple.
fn split(pairs: &[(char, char, char)], jamo: char) -> Option<(char, char)> {
    pairs
        .iter()
        .find(|(_, _, joined)| *joined == jamo)
        .map(|(head, tail, _)| (*head, *tail))
}

/// The Chinese plugin: XT9 pinyin over `libxt9a`, one dictionary, Simplified.
const PLUGIN_ZH: &str = "/usr/share/keyboard/zh_CN/libpredictor.so.1";

/// The Japanese plugin: Omron iWnn over `libwlf`, with ICU doing romaji to
/// kana. It names nineteen shared libraries against Chinese's four, every
/// one of them on the device.
const PLUGIN_JA: &str = "/usr/share/keyboard/ja/libpredictor.so.1";

/// How many candidate pointers the plugin is given room to write.
///
/// **Sized to the plugins' own tables, not to [`WANTED`]: Japanese ignores
/// the count asked for and writes as many as it produced.** The Japanese
/// table holds 250 entries at a 50-byte stride; Chinese preallocates 500
/// buffers of 41 bytes. Shrinking this is a memory-safety change.
const MAX_CANDIDATES: usize = 500;

/// Opaque host cookie, passed last to every slot and only ever logged.
const USER_DATA: u32 = 0;

/// The word the ET9 engine stamps into its Chinese-phonetic context, and
/// where in the context it puts it: `ET9CPSimplifiedToTraditional` refuses a
/// context whose word at `+0x88` is not this, and the mark is what
/// [`Chinese::find_converter`] scans for.
const CP_MAGIC_OFFSET: usize = 0x88;
const CP_MAGIC: u32 = 0x1428_1428;

/// 国 and 國 — one character whose Traditional form differs from its
/// Simplified one, which is enough to tell a real ET9 context from a word that
/// merely happens to read [`CP_MAGIC`].
const SIMPLIFIED: u16 = 0x56fd;
const TRADITIONAL: u16 = 0x570b;

/// Two letters to type at a plugin while watching what it writes: ordinary
/// pinyin and ordinary romaji, distinct so the second says something the
/// first did not.
const PROBE_KEYS: [char; 2] = ['a', 'b'];

/// The key handler stops recording at 512, so a longer count is a record that
/// is not this one.
const ZH_PENDING_MAX: usize = 512;

/// The composition buffer is 52 bytes and a candidate 50, so this is past any
/// valid end of either.
const PREEDIT_CAPACITY: usize = 256;

/// The vtable is eight function pointers. `load()` returns a 48-byte block and
/// the words past these are its own bookkeeping rather than entry points.
const SLOTS: usize = 8;

const SLOT_UNLOAD: usize = 0x00;
const SLOT_OPEN: usize = 0x04;
const SLOT_CLOSE: usize = 0x08;
const SLOT_PREEDIT: usize = 0x10;
const SLOT_KEY: usize = 0x14;
const SLOT_COMMIT: usize = 0x18;
const SLOT_CANDIDATES: usize = 0x1c;

type CStr = *const u8;

unsafe extern "C" {
    fn dlopen(path: CStr, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, sym: CStr) -> *mut c_void;
    fn calloc(n: usize, size: usize) -> *mut c_void;
}

/// `RTLD_NOW | RTLD_GLOBAL` — resolve everything up front, so a missing symbol
/// is an error here rather than a crash mid-sentence.
const RTLD_NOW_GLOBAL: i32 = 2 | 0x100;

fn cstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

/// `ET9CPSimplifiedToTraditional(ctx, buf, count)`, from `libxt9a`.
///
/// Converts a run of ET9 symbols **in place**. Returns 0 on success, 2 for a
/// context that fails the magic check, `0x1b` for a null buffer.
type ToTraditional = unsafe extern "C" fn(*mut c_void, *mut u16, u16) -> i32;

/// Where the kernel says a mapped file is.
const MAPS: &str = "/proc/self/maps";

/// Where the dynamic linker put a plugin, and what of it karyll may read.
///
/// Every address inside a plugin moves between builds, so every pointer
/// derived from one is checked against the kernel's map before it is read; a
/// pointer that fails the check is a message, not a read.
///
/// The object is found from a pointer the plugin itself produced:
/// `libpredictor.so.1` is a symlink to `libpredictor.so.1.0`,
/// `/proc/self/maps` names the file it resolves to, and the path handed to
/// `dlopen` never appears there at all.
struct Mapping {
    /// What the kernel calls the object. Reported, never matched against.
    path: String,
    /// The object's lowest address. Only used to report a discovered address as
    /// the offset a disassembly would show, which is what makes a device log
    /// comparable with one.
    base: usize,
    /// The executable ranges. A vtable slot outside them is not a function of
    /// this plugin.
    code: Vec<Range<usize>>,
    /// The writable ranges, which is where `.bss` is and therefore where every
    /// piece of engine state karyll looks for lives.
    data: Vec<Range<usize>>,
}

impl Mapping {
    /// Whether `addr` is code belonging to this object.
    fn holds_code(&self, addr: usize) -> bool {
        self.code.iter().any(|r| r.contains(&addr))
    }

    /// How many words can be read starting at `addr` before the end of the
    /// range holding it — zero if it is not in writable memory at all.
    fn words_from(&self, addr: usize) -> usize {
        self.data
            .iter()
            .find(|r| r.contains(&addr))
            .map_or(0, |r| (r.end - addr) / 4)
    }

    /// How many words [`Mapping::read`] returns.
    fn words(&self) -> usize {
        self.data.iter().map(|r| (r.end - r.start) / 4).sum()
    }

    /// Where the `index`th word of a [`Mapping::read`] came from.
    fn address(&self, index: usize) -> Option<usize> {
        let mut index = index;
        for r in &self.data {
            let words = (r.end - r.start) / 4;
            if index < words {
                return Some(r.start + index * 4);
            }
            index -= words;
        }
        None
    }

    /// Whether word `index` and the one after it are neighbours in memory,
    /// rather than the last word of one range and the first of the next.
    fn adjacent(&self, index: usize) -> bool {
        matches!(
            (self.address(index), self.address(index + 1)),
            (Some(a), Some(b)) if b == a + 4
        )
    }

    /// Every writable word of the object, in address order.
    ///
    /// Volatile: the plugin writes this memory from the other side of an FFI
    /// call, and two reads taken either side of a keystroke differ.
    ///
    /// # Safety
    /// The ranges must be this process's own, which they are for any `Mapping`
    /// that came from [`locate`] on [`MAPS`]. The parsing is kept apart from
    /// the reading so that it can be tested on text.
    unsafe fn read(&self) -> Vec<u32> {
        let mut words = Vec::with_capacity(self.words());
        for r in &self.data {
            let mut at = r.start;
            while at + 4 <= r.end {
                words.push(unsafe { std::ptr::read_volatile(at as *const u32) });
                at += 4;
            }
        }
        words
    }

    /// The address of every writable word that reads `value`.
    ///
    /// # Safety
    /// As [`Mapping::read`].
    unsafe fn scan(&self, value: u32) -> Vec<usize> {
        unsafe { self.read() }
            .iter()
            .enumerate()
            .filter(|(_, word)| **word == value)
            .filter_map(|(i, _)| self.address(i))
            .collect()
    }
}

/// One line of `/proc/self/maps`: an address range, its permissions, and the
/// file it came from if it came from one.
struct Row {
    range: Range<usize>,
    exec: bool,
    write: bool,
    /// Empty for anonymous memory. It is the last column and may hold
    /// spaces; runs of them collapse here — a path is only compared against
    /// another row of the same file, and printed.
    path: String,
}

fn row(line: &str) -> Option<Row> {
    let mut fields = line.split_whitespace();
    let (from, to) = fields.next()?.split_once('-')?;
    let perms = fields.next()?;
    Some(Row {
        range: usize::from_str_radix(from, 16).ok()?..usize::from_str_radix(to, 16).ok()?,
        exec: perms.contains('x'),
        write: perms.contains('w'),
        path: fields.skip(3).collect::<Vec<&str>>().join(" "),
    })
}

/// Describe the object holding `addr`, given the text of `/proc/self/maps`.
/// Pure: the parsing is tested against captured map text.
fn locate(maps: &str, addr: usize) -> Option<Mapping> {
    let rows: Vec<Row> = maps.lines().filter_map(row).collect();
    let path = rows
        .iter()
        .find(|r| r.range.contains(&addr))
        .filter(|r| !r.path.is_empty())?
        .path
        .clone();
    let base = rows
        .iter()
        .filter(|r| r.path == path)
        .map(|r| r.range.start)
        .min()?;
    let code = rows
        .iter()
        .filter(|r| r.path == path && r.exec)
        .map(|r| r.range.clone())
        .collect();
    let mut data: Vec<Range<usize>> = Vec::new();
    for r in rows.iter().filter(|r| r.write) {
        if r.path == path {
            data.push(r.range.clone());
        } else if r.path.is_empty() {
            // `.bss` runs past the end of the file's last page and the loader
            // maps the remainder anonymously, so writable anonymous memory
            // butted against the object's own data is the rest of that `.bss`.
            // It is most of it here: the Chinese plugin's is 228 KB against a
            // file of 18.
            match data.last_mut() {
                Some(last) if last.end == r.range.start => last.end = r.range.end,
                _ => {}
            }
        }
    }
    Some(Mapping {
        path,
        base,
        code,
        data,
    })
}

/// One of the device's predictor plugins, loaded and ready.
///
/// The ABI is `libkb`'s, one for every language. What differs lives in
/// [`Chinese`] and [`Japanese`]: which file to open, whether the session has
/// to be opened explicitly, and what to do with the candidates.
struct Plugin {
    table: *const usize,
    handle: *mut c_void,
    /// Where this plugin is, and the bounds every pointer derived from it is
    /// checked against.
    mapping: Mapping,
}

impl Plugin {
    /// `dlopen` the plugin and call `load()`, which brings its engine up.
    ///
    /// Nothing is preloaded alongside it: each plugin's own `DT_NEEDED`
    /// names the engine it was built against, and neither `kb` nor
    /// `libkb.so` links any `libxt9*`. The three XT9 engines are one build
    /// with different embedded data, every entry point at an identical
    /// address — a preloaded wrong one interposes silently.
    fn open(path: &str) -> Result<Plugin, String> {
        // The host block is what the plugin calls back into — committing a
        // candidate calls it — and a zeroed slot is a call through null, so
        // every slot points at a no-op returning 0.
        let host = unsafe { calloc(1, HOST_BLOCK) };
        if host.is_null() {
            return Err("cannot allocate the host block".into());
        }
        unsafe { install_host_table(host) };

        let handle = unsafe { dlopen(cstr(path).as_ptr(), RTLD_NOW_GLOBAL) };
        if handle.is_null() {
            return Err(format!("dlopen({path}) failed"));
        }
        let load = unsafe { dlsym(handle, cstr("load").as_ptr()) };
        if load.is_null() {
            return Err(format!("{path} has no `load` symbol"));
        }
        let f: unsafe extern "C" fn(*mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(load) };
        let table = unsafe { f(host) };
        if table.is_null() {
            return Err(format!("{path}: load() returned null"));
        }
        let table = table as *const usize;

        // `load()`'s return value cannot be checked before it is read.
        // Everything derived from it is checked against the mapping the
        // first slot leads to — the whole table: the slot order is `libkb`'s
        // ABI, and a table failing the check is an error, not a call. The
        // Thumb bit comes off first: every address in a Thumb-2 build is odd.
        let slots: Vec<usize> = (0..SLOTS).map(|i| unsafe { *table.add(i) } & !1).collect();
        let maps = std::fs::read_to_string(MAPS)
            .map_err(|e| format!("{path}: cannot read {MAPS}: {e}"))?;
        let open_at = slots[SLOT_OPEN / 4];
        let mapping = locate(&maps, open_at).ok_or_else(|| {
            format!(
                "{path}: load() returned a table whose open slot ({open_at:#x}) \
                 is in no mapped file"
            )
        })?;
        if let Some((slot, at)) = slots
            .iter()
            .enumerate()
            .find(|(_, at)| !mapping.holds_code(**at))
        {
            return Err(format!(
                "{path}: vtable slot +{:#04x} is {at:#x}, which is not code in {} — the plugin's \
                 table is not the one karyll was written against",
                slot * 4,
                mapping.path
            ));
        }
        eprintln!("ime: {} mapped at {:#x}", mapping.path, mapping.base);
        Ok(Plugin {
            table,
            handle,
            mapping,
        })
    }

    fn slot(&self, byte_offset: usize) -> usize {
        unsafe { *self.table.add(byte_offset / 4) }
    }

    /// A discovered address as the offset a disassembly of the plugin shows
    /// it at.
    fn offset_of(&self, addr: usize) -> usize {
        addr.saturating_sub(self.mapping.base)
    }

    fn symbol(&self, name: &str) -> *mut c_void {
        unsafe { dlsym(self.handle, cstr(name).as_ptr()) }
    }

    /// `+0x04` `prv_open` — begin a session.
    fn call_open(&self) -> i32 {
        let f: unsafe extern "C" fn(u32, u32) -> i32 =
            unsafe { std::mem::transmute(self.slot(SLOT_OPEN)) };
        unsafe { f(0, USER_DATA) }
    }

    /// `+0x14` `prv_key_handler` — one key, as a Unicode code point.
    fn call_key(&self, key: char) {
        let f: unsafe extern "C" fn(u32, u32) -> i32 =
            unsafe { std::mem::transmute(self.slot(SLOT_KEY)) };
        unsafe { f(key as u32, USER_DATA) };
    }

    /// `+0x1c` `prv_get_candidate_list` — whatever the engine now offers.
    ///
    /// **The count out is a report, not an answer to the count in.** Chinese
    /// clamps to what was asked for; Japanese overwrites it with however many
    /// it produced and fills that many array slots — so the array is sized to
    /// the plugins' whole preallocation, and the result is cut to [`KEPT`]
    /// here.
    ///
    /// The strings are borrowed from the plugin's own fixed-stride table,
    /// valid until the next call, and are copied here.
    fn call_candidates(&self) -> Vec<String> {
        let mut slots: Vec<*mut c_char> = vec![std::ptr::null_mut(); MAX_CANDIDATES];
        let mut count: u32 = KEPT as u32;
        let f: unsafe extern "C" fn(*mut *mut c_char, *mut u32, u32) =
            unsafe { std::mem::transmute(self.slot(SLOT_CANDIDATES)) };
        unsafe { f(slots.as_mut_ptr(), &mut count, USER_DATA) };

        let produced = (count as usize).min(MAX_CANDIDATES);
        slots[..produced]
            .iter()
            .filter_map(|p| unsafe { c_string(*p) })
            .take(KEPT)
            .collect()
    }

    /// `+0x18` — accept the candidate at `index`.
    fn call_commit(&self, index: usize) {
        let f: unsafe extern "C" fn(u32, u32) -> i32 =
            unsafe { std::mem::transmute(self.slot(SLOT_COMMIT)) };
        unsafe { f(index as u32, USER_DATA) };
    }

    /// `+0x10` — copy out the composition as the engine understands it.
    /// Empty is `None`.
    fn call_preedit(&self) -> Option<String> {
        let mut buf = [0u8; PREEDIT_CAPACITY];
        let f: unsafe extern "C" fn(*mut c_char, usize) =
            unsafe { std::mem::transmute(self.slot(SLOT_PREEDIT)) };
        unsafe { f(buf.as_mut_ptr().cast(), buf.len()) };
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        (end > 0).then(|| String::from_utf8_lossy(&buf[..end]).into_owned())
    }
}

impl Drop for Plugin {
    /// Close and unload, in that order — the plugin's own lifecycle. Closing
    /// writes the user dictionary back to disk (Amazon's `xt9-zh.*` for
    /// Chinese, the iWnn learning data for Japanese), so a session's learned
    /// phrases survive. `panic = "abort"` skips `Drop` and loses that write.
    fn drop(&mut self) {
        let close: unsafe extern "C" fn(u32) -> i32 =
            unsafe { std::mem::transmute(self.slot(SLOT_CLOSE)) };
        let unload: unsafe extern "C" fn(u32) -> i32 =
            unsafe { std::mem::transmute(self.slot(SLOT_UNLOAD)) };
        unsafe {
            close(USER_DATA);
            unload(USER_DATA);
        }
    }
}

/// Chinese pinyin, with Traditional as a conversion of the same candidates.
pub struct Chinese {
    plugin: Plugin,
    /// Set when the engine's own Simplified-to-Traditional converter was found
    /// *and* its context passed the magic check. `None` leaves Traditional
    /// unavailable rather than silently returning Simplified.
    converter: Option<(ToTraditional, *mut c_void)>,
    /// Whether to convert what the engine offers.
    want_traditional: bool,
    /// The plugin's record of the keys it is still holding, once the plugin has
    /// shown karyll where it keeps it. `None` costs partial commits, not
    /// Chinese input.
    pending: Option<Pending>,
}

/// The two places the Chinese plugin keeps what it has not converted yet:
/// how many keys it is holding, and the keys themselves. Two pointers: the
/// pair sits side by side in some builds and 57 KB apart in others.
struct Pending {
    count: *const u32,
    keys: *const u32,
    /// How many keys can be read before the end of the mapping holding them, so
    /// that a count karyll has misread cannot walk off it.
    room: usize,
}

impl Chinese {
    pub fn open() -> Result<Chinese, String> {
        let plugin = Plugin::open(PLUGIN_ZH).map_err(|e| format!("{e} — no Chinese input"))?;
        let mut zh = Chinese {
            plugin,
            converter: None,
            want_traditional: false,
            pending: None,
        };
        // Begin a session — the documented lifecycle, and both searches
        // below want an engine holding no keys.
        let st = zh.plugin.call_open();
        if st != 0 {
            eprintln!("ime: zh prv_open returned {st}, continuing");
        }
        zh.find_converter();
        zh.find_pending();
        Ok(zh)
    }

    /// Find where the plugin keeps the keys it has not converted yet, by
    /// watching it keep some: an open session holds no keys, a key makes it
    /// hold that key, a second makes it two, reopening empties it. The
    /// writable memory is copied at each of those four moments; the count is
    /// the word that read 0, 1, 2, 0, and the keys are the pair that became
    /// `a`, then `a` and `b`.
    ///
    /// The halves are searched for independently — they are not always
    /// neighbours — and a counter immediately in front of the array is
    /// preferred: that one is the count *of that array*. The search costs
    /// four copies and three calls, and leaves the session as it found it.
    fn find_pending(&mut self) {
        let quiet = unsafe { self.plugin.mapping.read() };
        self.plugin.call_key(PROBE_KEYS[0]);
        let one = unsafe { self.plugin.mapping.read() };
        self.plugin.call_key(PROBE_KEYS[1]);
        let two = unsafe { self.plugin.mapping.read() };
        self.plugin.call_open();
        let emptied = unsafe { self.plugin.mapping.read() };

        let map = &self.plugin.mapping;
        let (first, second) = (PROBE_KEYS[0] as u32, PROBE_KEYS[1] as u32);
        let counts: Vec<usize> = (0..quiet.len())
            .filter(|&i| (quiet[i], one[i], two[i], emptied[i]) == (0, 1, 2, 0))
            .collect();
        let keys: Vec<usize> = (0..quiet.len().saturating_sub(1))
            .filter(|&i| (quiet[i], quiet[i + 1]) == (0, 0))
            .filter(|&i| (one[i], two[i], two[i + 1]) == (first, first, second))
            .filter(|&i| map.adjacent(i))
            .collect();

        let found = keys.first().and_then(|&keys_at| {
            let count_at = counts
                .iter()
                .find(|&&i| i + 1 == keys_at && map.adjacent(i))
                .or(counts.first())?;
            Some((map.address(*count_at)?, map.address(keys_at)?))
        });
        let Some((count, keys_at)) = found else {
            eprintln!(
                "ime: the zh plugin's pending-key record did not show itself \
                 ({} counters, {} arrays) — a candidate covering part of a word will end it",
                counts.len(),
                keys.len()
            );
            return;
        };
        eprintln!(
            "ime: the zh plugin holds its pending keys at +{:#x}, counted at +{:#x}",
            self.plugin.offset_of(keys_at),
            self.plugin.offset_of(count)
        );
        self.pending = Some(Pending {
            count: count as *const u32,
            keys: keys_at as *const u32,
            room: self.plugin.mapping.words_from(keys_at),
        });
    }

    /// The reading the plugin is still holding, as the letters it was sent.
    ///
    /// `Some("")` is a word the engine has finished with and `None` is not
    /// knowing, which are different things to the caller: one ends a word, the
    /// other means the engine has to be told to let go of it. Anything that is
    /// not pinyin says the record is not the one this was written against, and
    /// is `None` for the same reason.
    fn pending(&self) -> Option<String> {
        let at = self.pending.as_ref()?;
        let count = unsafe { std::ptr::read_volatile(at.count) } as usize;
        if count > ZH_PENDING_MAX || count > at.room {
            return None;
        }
        (0..count)
            .map(|i| {
                char::from_u32(unsafe { std::ptr::read_volatile(at.keys.add(i)) })
                    .filter(|c| c.is_ascii_alphabetic() || *c == '\'')
            })
            .collect()
    }

    /// What the engine offers, in the script asked for — converted here, so
    /// the bar shows what will be inserted.
    fn candidates(&self) -> Vec<String> {
        let candidates = self.plugin.call_candidates();
        if self.want_traditional {
            candidates.iter().map(|c| self.to_traditional(c)).collect()
        } else {
            candidates
        }
    }

    /// Find the engine's own Simplified-to-Traditional converter, and the
    /// context it needs. The context is a static that moves with every build
    /// and announces itself: the engine stamps [`CP_MAGIC`] into it — the
    /// check `ET9CPSimplifiedToTraditional` itself performs — so the magic
    /// is scanned for in the plugin's writable memory, and [`converts`]
    /// confirms a hit.
    fn find_converter(&mut self) {
        let convert = self.plugin.symbol("ET9CPSimplifiedToTraditional");
        if convert.is_null() {
            eprintln!("ime: no ET9CPSimplifiedToTraditional — Traditional unavailable");
            return;
        }
        let convert = unsafe { std::mem::transmute::<*mut c_void, ToTraditional>(convert) };
        let marked = unsafe { self.plugin.mapping.scan(CP_MAGIC) };
        for at in &marked {
            let Some(ctx) = at.checked_sub(CP_MAGIC_OFFSET) else {
                continue;
            };
            let ctx = ctx as *mut c_void;
            if !converts(convert, ctx) {
                continue;
            }
            eprintln!(
                "ime: Traditional conversion available, the ET9 context at +{:#x}",
                self.plugin.offset_of(ctx as usize)
            );
            self.converter = Some((convert, ctx));
            return;
        }
        eprintln!(
            "ime: no ET9 phonetic context in the zh plugin ({} words carry the mark, none \
             converts) — Traditional unavailable",
            marked.len()
        );
    }

    /// Convert one candidate to Traditional, in the engine's own terms.
    ///
    /// The engine works in 16-bit symbols and converts in place, so the
    /// string makes a round trip through UTF-16. Anything outside the basic
    /// plane is left alone: `encode_utf16` splits it into a surrogate pair,
    /// two characters to the converter.
    fn to_traditional(&self, text: &str) -> String {
        let Some((convert, ctx)) = self.converter else {
            return text.to_string();
        };
        if text.chars().any(|c| c as u32 > 0xffff) {
            return text.to_string();
        }
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        let Ok(count) = u16::try_from(buf.len()) else {
            return text.to_string();
        };
        let st = unsafe { convert(ctx, buf.as_mut_ptr(), count) };
        if st != 0 {
            return text.to_string();
        }
        String::from_utf16_lossy(&buf)
    }
}

/// Whether a candidate context really is the engine's own, asked by converting
/// a character that differs between the scripts.
///
/// The magic is the engine's own check on the pointer; this is karyll's. 国
/// comes back as 國 only from a context that is what it claims to be.
fn converts(convert: ToTraditional, ctx: *mut c_void) -> bool {
    let mut buf = [SIMPLIFIED];
    let st = unsafe { convert(ctx, buf.as_mut_ptr(), 1) };
    st == 0 && buf[0] == TRADITIONAL
}

impl Ime for Chinese {
    fn key(&mut self, key: char) -> Vec<String> {
        self.plugin.call_key(key);
        self.candidates()
    }

    fn set_traditional(&mut self, traditional: bool) {
        self.want_traditional = traditional && self.converter.is_some();
    }

    /// The commit slot re-feeds the keys the phrase did not cover, so the
    /// engine returns from a commit composing the remainder; this notices,
    /// and restarts nothing. Without the pending record a finished word and
    /// a half-converted one look the same, and the engine is cleared — at
    /// the cost of the context the commit set — so no stranded reading lands
    /// on the front of the next word typed.
    fn commit(&mut self, index: usize) -> Option<Rest> {
        self.plugin.call_commit(index);
        match self.pending() {
            Some(reading) if reading.is_empty() => None,
            Some(reading) => Some(Rest {
                reading,
                candidates: self.candidates(),
            }),
            None => {
                self.plugin.call_open();
                None
            }
        }
    }

    fn clear(&mut self) {
        // Reopening the session clears the context and the symbols
        // (`prv_key_handler` routes 0x20 to the same pair).
        self.plugin.call_open();
    }
}

/// Japanese: romaji in, kana and kanji out, through iWnn.
pub struct Japanese {
    plugin: Plugin,
}

impl Japanese {
    pub fn open() -> Result<Japanese, String> {
        let plugin = Plugin::open(PLUGIN_JA).map_err(|e| format!("{e} — no Japanese input"))?;

        // `prv_open` sets the plugin's input mode; before it runs the mode
        // is zeroed `.bss`, on which the key handler takes a different
        // branch and the composition getter returns nothing.
        let st = plugin.call_open();
        if st != 0 {
            eprintln!("ime: ja prv_open returned {st}, continuing");
        }

        // **`load()` returns a complete-looking table whether or not the
        // engine came up**: its error paths log and fall through to the same
        // return. So type at it — an engine that is up answers a letter with
        // kana, with conversions, usually both; one that is not answers
        // neither. No address involved, so the check holds across firmwares.
        plugin.call_key(PROBE_KEYS[0]);
        let candidates = plugin.call_candidates().len();
        let composed = plugin.call_preedit();
        plugin.call_open();
        if candidates == 0 && composed.is_none() {
            return Err(
                "the ja plugin loaded but its engine did not come up — it composed nothing from a \
                 key and offered nothing to convert it to; check that \
                 /usr/share/keyboard/ja/JA.conf and the iWnn dictionaries are present"
                    .into(),
            );
        }
        Ok(Japanese { plugin })
    }
}

impl Ime for Japanese {
    fn key(&mut self, key: char) -> Vec<String> {
        self.plugin.call_key(key);
        self.plugin.call_candidates()
    }

    /// Japanese has one dictionary and one script convention; there is nothing
    /// for this to switch.
    fn set_traditional(&mut self, _traditional: bool) {}

    /// The commit slot ends by zeroing the composition buffer, so a partial
    /// candidate takes the rest of the reading with it; `call_preedit` asks
    /// rather than assumes.
    fn commit(&mut self, index: usize) -> Option<Rest> {
        self.plugin.call_commit(index);
        let reading = self.plugin.call_preedit()?;
        Some(Rest {
            reading,
            candidates: self.plugin.call_candidates(),
        })
    }

    fn clear(&mut self) {
        // Reopening resets the composition, the selection and the input mode
        // together.
        self.plugin.call_open();
    }

    /// The kana, not the romaji: the engine holds the ICU transliteration.
    fn preedit(&self) -> Option<String> {
        self.plugin.call_preedit()
    }
}

/// Copy a NUL-terminated UTF-8 string out of the plugin's buffers. They are 41
/// bytes each, so 64 is past any valid end.
///
/// # Safety
/// `p` must be null or point at a NUL-terminated string.
unsafe fn c_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let b: *const u8 = p.cast();
    let mut bytes = Vec::new();
    for i in 0..64 {
        let c = unsafe { *b.add(i) };
        if c == 0 {
            break;
        }
        bytes.push(c);
    }
    if bytes.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Room for the host callback table: opaque to karyll, and larger than the
/// sixteen slots filled below.
const HOST_BLOCK: usize = 0x400;

/// A host callback that does nothing. The plugin calls into the host block
/// to report a commit and to ask about surrounding text; karyll uses
/// neither, and the pointers have to be callable.
unsafe extern "C" fn host_noop(_a0: usize, _a1: usize, _a2: usize, _a3: usize) -> u32 {
    0
}

/// Fill a host block with callable no-ops.
///
/// # Safety
/// `host` must point to at least [`HOST_BLOCK`] writable bytes.
unsafe fn install_host_table(host: *mut c_void) {
    let f: unsafe extern "C" fn(usize, usize, usize, usize) -> u32 = host_noop;
    for i in 0..16 {
        unsafe { *(host as *mut usize).add(i) = f as usize };
    }
}

/// A canned pinyin table standing in for the engine.
///
/// Not an IME, and not shipped: the real engine is a device file that cannot be
/// redistributed, so this is how the [`Ime`] contract itself gets tested —
/// including the parts the editor relies on, like backspace and space being the
/// engine's job rather than the editor's.
#[cfg(test)]
pub struct Stub {
    typed: String,
}

#[cfg(test)]
impl Stub {
    pub fn new() -> Stub {
        Stub {
            typed: String::new(),
        }
    }

    /// A handful of real syllables, enough to exercise selection, a growing
    /// preedit, and the empty-candidate case.
    fn lookup(&self) -> Vec<String> {
        let canned: &[(&str, &[&str])] = &[
            ("n", &["你", "年", "呢", "能", "那"]),
            ("ni", &["你", "拟", "泥", "腻", "逆"]),
            ("nih", &["你好", "你会", "你还"]),
            ("niha", &["你哈"]),
            ("nihao", &["你好"]),
            ("h", &["和", "很", "会"]),
            ("ha", &["哈", "还"]),
            ("hao", &["好", "号", "毫"]),
            ("shijie", &["世界", "时节", "使节", "十"]),
            ("jie", &["界", "节", "结", "接"]),
        ];
        canned
            .iter()
            .find(|(k, _)| *k == self.typed)
            .map(|(_, v)| v.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    }

    /// What a candidate leaves unconverted, for the ones that cover only the
    /// front of a reading. The engines do it whenever a shorter phrase is the
    /// better guess, and it is the case the editor has to carry.
    fn rest(&self, candidate: &str) -> Option<&'static str> {
        let partial: &[(&str, &str, &str)] = &[("shijie", "十", "jie")];
        partial
            .iter()
            .find(|(reading, taken, _)| *reading == self.typed && *taken == candidate)
            .map(|(_, _, rest)| *rest)
    }
}

#[cfg(test)]
impl Ime for Stub {
    fn key(&mut self, key: char) -> Vec<String> {
        match key {
            // The engine handles these itself; the stub agrees.
            '\u{8}' => {
                self.typed.pop();
            }
            ' ' => self.typed.clear(),
            c => self.typed.push(c),
        }
        self.lookup()
    }

    fn commit(&mut self, index: usize) -> Option<Rest> {
        let offered = self.lookup();
        let rest = offered
            .get(index)
            .and_then(|taken| self.rest(taken))
            .unwrap_or_default();
        self.typed = rest.to_string();
        (!rest.is_empty()).then(|| Rest {
            reading: rest.to_string(),
            candidates: self.lookup(),
        })
    }

    fn clear(&mut self) {
        self.typed.clear();
    }

    /// The stub has no converter; Traditional stays unavailable.
    fn set_traditional(&mut self, _traditional: bool) {}
}

#[cfg(test)]
impl Default for Stub {
    fn default() -> Self {
        Self::new()
    }
}

/// An engine that composes something other than the keys it was given: the
/// keys are `nihon` and the composition is にほん — [`Ime::preedit`]'s case.
#[cfg(test)]
pub struct KanaStub {
    typed: String,
}

#[cfg(test)]
impl KanaStub {
    pub fn new() -> KanaStub {
        KanaStub {
            typed: String::new(),
        }
    }

    /// Straight two-letter syllables and a bare `n`, which is all this needs.
    fn kana(&self) -> String {
        let table = [
            ("ni", "に"),
            ("ho", "ほ"),
            ("n", "ん"),
            ("ka", "か"),
            ("na", "な"),
        ];
        let mut out = String::new();
        let mut rest = self.typed.as_str();
        'outer: while !rest.is_empty() {
            for (romaji, k) in table {
                if rest.starts_with(romaji) && !(romaji == "n" && rest.len() > 1) {
                    out.push_str(k);
                    rest = &rest[romaji.len()..];
                    continue 'outer;
                }
            }
            // A partial syllable stays as the letters typed — `nih` composes
            // にh.
            out.push_str(&rest[..1]);
            rest = &rest[1..];
        }
        out
    }
}

#[cfg(test)]
impl Ime for KanaStub {
    fn key(&mut self, key: char) -> Vec<String> {
        match key {
            '\u{8}' => {
                self.typed.pop();
            }
            // Space is the conversion request, not a commit.
            ' ' => {}
            c => self.typed.push(c),
        }
        if self.typed == "nihon" {
            vec!["日本".into(), "にほん".into(), "ニホン".into()]
        } else {
            Vec::new()
        }
    }

    /// iWnn's plugin drops whatever a candidate did not cover, so a commit
    /// always ends the word.
    fn commit(&mut self, _index: usize) -> Option<Rest> {
        self.typed.clear();
        None
    }

    fn clear(&mut self) {
        self.typed.clear();
    }

    fn set_traditional(&mut self, _traditional: bool) {}

    fn preedit(&self) -> Option<String> {
        let kana = self.kana();
        (!kana.is_empty()).then_some(kana)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type `keys` as they are struck on a 두벌식 keyboard, and hand back what
    /// is on the page: everything committed, with [`Korean::preedit`] after it.
    fn typed(keys: &str) -> String {
        let mut korean = Korean::default();
        let mut page: String = keys.chars().map(|key| korean.key(key)).collect();
        page.push_str(&korean.preedit());
        page
    }

    #[test]
    fn a_syllable_is_its_three_slots() {
        assert_eq!(compose_syllable('ㄱ', 'ㅏ', None), Some('가'));
        assert_eq!(compose_syllable('ㅎ', 'ㅏ', Some('ㄴ')), Some('한'));
        assert_eq!(compose_syllable('ㅇ', 'ㅏ', Some('ㄵ')), Some('앉'));
        // The last syllable of the block.
        assert_eq!(compose_syllable('ㅎ', 'ㅣ', Some('ㅎ')), Some('힣'));
    }

    /// The 받침 belongs to the syllable it was typed into until a vowel
    /// arrives, and to the next one after that.
    #[test]
    fn a_vowel_takes_the_final_away() {
        assert_eq!(typed("gks"), "한");
        assert_eq!(typed("gksk"), "하나");
        assert_eq!(typed("dksw"), "앉");
        assert_eq!(typed("dkswk"), "안자");
    }

    /// A compound sends its tail and keeps its head as the 받침.
    #[test]
    fn a_compound_final_splits_rather_than_migrating_whole() {
        assert_eq!(typed("dlfg"), "잃");
        assert_eq!(typed("dlfgj"), "일허");
        assert_eq!(typed("dhkfrh"), "왈고");
    }

    #[test]
    fn two_keys_make_one_vowel() {
        assert_eq!(typed("rhk"), "과");
        assert_eq!(typed("rho"), "괘");
        assert_eq!(typed("rhl"), "괴");
        assert_eq!(typed("rnj"), "궈");
        assert_eq!(typed("rnp"), "궤");
        assert_eq!(typed("rnl"), "귀");
        assert_eq!(typed("rml"), "긔");
        // A compound vowel composes with an empty 초성 in front of it.
        assert_eq!(typed("hk"), "ㅘ");
    }

    #[test]
    fn two_keys_make_one_final() {
        assert_eq!(typed("ahrt"), "몫");
        assert_eq!(typed("djqt"), "없");
        assert_eq!(typed("ekfr"), "닭");
    }

    /// The consonants absent from [`CODAS`] open a syllable of their own.
    #[test]
    fn a_final_korean_never_writes_starts_a_syllable() {
        // ㄸ, ㅃ and ㅉ are initials only, so 가 followed by one is two
        // syllables.
        assert_eq!(typed("rkE"), "가ㄸ");
        assert_eq!(typed("rkEk"), "가따");
        // ㄲ and ㅆ are in CODAS, and stay put.
        assert_eq!(typed("dlT"), "있");
    }

    /// Whole words, typed the way they are typed.
    ///
    /// 반갑습니다 carries a compound 받침 through one word: ㅂ takes ㅅ as ㅄ,
    /// and the ㅡ after it splits the pair, leaving 갑 and carrying ㅅ into 스.
    #[test]
    fn words_come_out_as_words() {
        assert_eq!(typed("gksrmf"), "한글");
        assert_eq!(typed("dkssud"), "안녕");
        assert_eq!(typed("gksrnrdj"), "한국어");
        assert_eq!(typed("qksrkqtmqslek"), "반갑습니다");
    }

    /// A lone consonant and a lone vowel are correct text, and are what
    /// [`Korean::preedit`] carries while a syllable is half typed.
    #[test]
    fn a_half_typed_syllable_shows_as_its_jamo() {
        assert_eq!(typed("r"), "ㄱ");
        assert_eq!(typed("k"), "ㅏ");
        assert_eq!(typed("rr"), "ㄱㄱ");
        assert_eq!(typed("kk"), "ㅏㅏ");
    }

    /// 앉 → 안 → 아 → ㅇ → nothing, one jamo at a time.
    #[test]
    fn backspace_decomposes_a_syllable() {
        let mut korean = Korean::default();
        for key in "dksw".chars() {
            korean.key(key);
        }
        for expected in ["앉", "안", "아", "ㅇ", ""] {
            assert_eq!(korean.preedit(), expected);
            assert_eq!(korean.backspace(), !expected.is_empty());
        }
        // Empty, and the keystroke is the document's.
        assert!(!korean.backspace());
        assert_eq!(korean.preedit(), "");
    }

    /// A compound falls back to its head.
    #[test]
    fn backspace_undoes_one_half_of_a_compound() {
        let mut korean = Korean::default();
        for key in "rhk".chars() {
            korean.key(key);
        }
        assert_eq!(korean.preedit(), "과");
        korean.backspace();
        assert_eq!(korean.preedit(), "고");
    }

    /// Every tail a 받침 can send is in [`INITIALS`], so the migration in
    /// `Korean::vowel` always has a slot to land in.
    #[test]
    fn every_final_can_become_an_initial() {
        for coda in CODAS {
            let moves = split(&COMPOUND_CODAS, coda).map_or(coda, |(_, tail)| tail);
            assert!(
                INITIALS.contains(&moves),
                "{coda} would migrate as {moves}, which is no initial"
            );
        }
    }

    /// The layout covers every letter and touches nothing else, leaving
    /// Korean's ASCII punctuation, digits and space to the editor.
    #[test]
    fn the_layout_is_the_letters_and_only_the_letters() {
        for key in 'a'..='z' {
            assert!(jamo_for(key).is_some(), "{key} writes no jamo");
        }
        for key in ['1', '0', '.', ',', ' ', '-', '/', '\'', ';', '\n'] {
            assert_eq!(jamo_for(key), None, "{key:?} should stay itself");
        }
    }

    /// Shift carries the tense consonants and the two iotised vowels. Every
    /// other capital writes the jamo under the finger.
    #[test]
    fn the_shifted_row_is_the_tense_consonants_and_two_vowels() {
        for (key, jamo) in [
            ('Q', 'ㅃ'),
            ('W', 'ㅉ'),
            ('E', 'ㄸ'),
            ('R', 'ㄲ'),
            ('T', 'ㅆ'),
            ('O', 'ㅒ'),
            ('P', 'ㅖ'),
        ] {
            assert_eq!(jamo_for(key), Some(jamo));
        }
        for key in ('A'..='Z').filter(|c| !"QWERTOP".contains(*c)) {
            assert_eq!(
                jamo_for(key),
                jamo_for(key.to_ascii_lowercase()),
                "{key} is not on the shifted row"
            );
        }
    }

    /// Every jamo [`jamo_for`] produces is in [`INITIALS`] or [`MEDIALS`], so
    /// [`Korean::key`] has an arm for all of them.
    #[test]
    fn every_key_writes_a_jamo_the_composer_knows() {
        for key in ('a'..='z').chain('A'..='Z') {
            let jamo = jamo_for(key).unwrap();
            assert!(
                INITIALS.contains(&jamo) || MEDIALS.contains(&jamo),
                "{key} writes {jamo}, which is neither an initial nor a vowel"
            );
        }
    }

    /// **A Korean keyboard types Korean**, capitals included: a capital is the
    /// tense consonant on the shifted row. Latin is reached by switching
    /// source.
    #[test]
    fn every_letter_is_a_jamo_while_korean_is_on() {
        use crate::keymap::Action;
        for composing in [false, true] {
            for key in ('a'..='z').chain('A'..='Z') {
                assert_eq!(
                    compose(&Action::Insert(key), composing, Script::Korean),
                    Compose::Jamo(key),
                    "{key} while {}composing",
                    if composing { "" } else { "not " }
                );
            }
        }
        // [`Script::Chinese`] and [`Script::Japanese`] send a capital straight
        // to the page.
        assert_eq!(
            compose(&Action::Insert('N'), true, Script::Chinese),
            Compose::Latin('N')
        );
    }

    /// **Every key that is not a letter finishes the syllable and goes on to
    /// mean what it always means.** No candidate to number, no page to turn,
    /// and no CJK punctuation: Korean writes ASCII marks.
    #[test]
    fn a_key_that_is_not_a_jamo_ends_the_syllable_without_being_eaten() {
        use crate::keymap::Action;
        for action in [
            Action::Insert(' '),
            Action::Insert('.'),
            Action::Insert('1'),
            Action::Insert('\''),
            Action::Newline,
            Action::Right,
            Action::Escape,
            Action::Undo,
        ] {
            assert_eq!(
                compose(&action, true, Script::Korean),
                Compose::Finish,
                "{action:?}"
            );
            // With nothing under construction the key passes straight through.
            assert_eq!(
                compose(&action, false, Script::Korean),
                Compose::Pass,
                "{action:?}"
            );
        }
    }

    /// Backspace decomposes while a syllable holds a jamo, and passes to the
    /// document past the last one.
    #[test]
    fn korean_backspace_reaches_the_composer_only_while_it_holds_one() {
        use crate::keymap::Action;
        assert_eq!(
            compose(&Action::Backspace, true, Script::Korean),
            Compose::Decompose
        );
        assert_eq!(
            compose(&Action::Backspace, false, Script::Korean),
            Compose::Pass
        );
    }

    #[test]
    fn the_stub_grows_a_preedit_and_narrows_the_candidates() {
        let mut ime = Stub::new();
        assert_eq!(ime.key('n').first().map(String::as_str), Some("你"));
        assert_eq!(ime.key('i').len(), 5);
        assert_eq!(ime.key('h').first().map(String::as_str), Some("你好"));
    }

    /// Backspace and space are handled inside `prv_key_handler` — `0x08`
    /// clears one symbol, `0x20` clears all — so the editor forwards them,
    /// and the stub agrees.
    #[test]
    fn backspace_and_space_are_the_engines_job() {
        let mut ime = Stub::new();
        ime.key('n');
        ime.key('i');
        assert_eq!(ime.key('\u{8}').first().map(String::as_str), Some("你"));
        assert!(ime.key(' ').is_empty());
    }

    #[test]
    fn an_unknown_syllable_offers_nothing_rather_than_guessing() {
        let mut ime = Stub::new();
        assert!(ime.key('q').is_empty());
    }

    #[test]
    fn committing_clears_the_composition() {
        let mut ime = Stub::new();
        ime.key('n');
        assert_eq!(ime.commit(0), None);
        assert_eq!(ime.key('h').first().map(String::as_str), Some("和"));
    }

    /// **A candidate does not have to cover the whole reading**, and the
    /// word is not over when one that does not is taken: it carries on with
    /// what is left, and the next keystroke belongs to that.
    #[test]
    fn a_candidate_covering_part_of_the_reading_leaves_the_rest() {
        let mut ime = Stub::new();
        for c in "shijie".chars() {
            ime.key(c);
        }
        let rest = ime
            .commit(3)
            .expect("the fourth covers one syllable of two");
        assert_eq!(rest.reading, "jie");
        assert_eq!(rest.candidates.first().map(String::as_str), Some("界"));
    }

    /// One that covers all of it finishes the word.
    #[test]
    fn a_candidate_covering_the_reading_finishes_the_word() {
        let mut ime = Stub::new();
        for c in "shijie".chars() {
            ime.key(c);
        }
        assert_eq!(ime.commit(0), None);
        assert_eq!(ime.key('h').first().map(String::as_str), Some("和"));
    }

    /// Pinyin *is* the letters typed: the Chinese engine reports no
    /// composition of its own — [`Ime::preedit`]'s default arm.
    #[test]
    fn chinese_keeps_no_composition_of_its_own() {
        let mut ime = Stub::new();
        ime.key('n');
        ime.key('i');
        assert_eq!(ime.preedit(), None);
    }

    /// The Japanese composition is not the letters typed: にほん from
    /// `nihon`.
    #[test]
    fn japanese_composes_kana_from_the_romaji_it_was_sent() {
        let mut ime = KanaStub::new();
        for c in "nihon".chars() {
            ime.key(c);
        }
        assert_eq!(ime.preedit().as_deref(), Some("にほん"));
        assert_eq!(ime.key(' ').first().map(String::as_str), Some("日本"));
    }

    /// Backspace reaches the engine, and the composition shortens by a
    /// *kana*.
    #[test]
    fn backspace_shortens_the_japanese_composition_by_a_syllable() {
        let mut ime = KanaStub::new();
        for c in "niho".chars() {
            ime.key(c);
        }
        assert_eq!(ime.preedit().as_deref(), Some("にほ"));
        ime.key('\u{8}');
        ime.key('\u{8}');
        assert_eq!(ime.preedit().as_deref(), Some("に"));
    }

    /// Finding a plugin in memory, which is everything about the discovery that
    /// can be tested without one. What is left over — the magic scan, the
    /// pending-key search, the Japanese engine answering a letter — is a
    /// conversation with a proprietary library and happens on the device.
    mod mapping {
        use super::*;

        /// An armv7 Kindle's `/proc/self/maps`, cut to karyll, the Chinese
        /// plugin, the XT9 engine it pulled in, and the neighbours that make
        /// the rules matter. The columns are the kernel's: range, permissions,
        /// file offset, device, inode, and the path if there is one.
        const KINDLE: &str = "\
00010000-000d4000 r-xp 00000000 b3:0c 2101       /mnt/us/extensions/karyll/bin/karyll
000e3000-000e5000 rw-p 000c3000 b3:0c 2101       /mnt/us/extensions/karyll/bin/karyll
000e5000-00107000 rw-p 00000000 00:00 0          [heap]
b6a1c000-b6a3f000 r-xp 00000000 b3:0c 1190       /usr/lib/libxt9a.so.1.0
b6a3f000-b6a4e000 ---p 00023000 b3:0c 1190       /usr/lib/libxt9a.so.1.0
b6a4e000-b6a4f000 rw-p 00022000 b3:0c 1190       /usr/lib/libxt9a.so.1.0
b6d2a000-b6d2f000 r-xp 00000000 b3:0c 1187       /usr/share/keyboard/zh_CN/libpredictor.so.1.0
b6d2f000-b6d3e000 ---p 00005000 b3:0c 1187       /usr/share/keyboard/zh_CN/libpredictor.so.1.0
b6d3e000-b6d3f000 r--p 00004000 b3:0c 1187       /usr/share/keyboard/zh_CN/libpredictor.so.1.0
b6d3f000-b6d40000 rw-p 00005000 b3:0c 1187       /usr/share/keyboard/zh_CN/libpredictor.so.1.0
b6d40000-b6d78000 rw-p 00000000 00:00 0
b6e00000-b6e21000 rw-p 00000000 00:00 0
b6f00000-b6f20000 r-xp 00000000 b3:0c 1044       /lib/libc-2.20.so
";

        /// Somewhere in the Chinese plugin's code, which is the only sort of
        /// address karyll has to start from: a vtable slot.
        const A_SLOT: usize = 0xb6d2_a3c4;

        /// **The path karyll opens is not the path the kernel reports.**
        /// `libpredictor.so.1` is a symlink, `/proc/self/maps` names what it
        /// resolves to, and a lookup by the opened name finds nothing.
        #[test]
        fn the_plugin_is_found_from_its_own_pointer_rather_than_from_a_path() {
            assert!(KINDLE.lines().filter_map(row).all(|r| r.path != PLUGIN_ZH));
            let map = locate(KINDLE, A_SLOT).expect("a slot lands in the plugin");
            assert_eq!(map.path, "/usr/share/keyboard/zh_CN/libpredictor.so.1.0");
            assert_eq!(map.base, 0xb6d2_a000);
        }

        /// `.bss` is a few hundred kilobytes against a file of eighteen, so
        /// almost all of it is the anonymous mapping the loader puts on the end
        /// — and every address karyll goes looking for is in there.
        #[test]
        fn the_bss_past_the_end_of_the_file_belongs_to_the_object() {
            let map = locate(KINDLE, A_SLOT).unwrap();
            assert_eq!(map.data, vec![0xb6d3_f000..0xb6d7_8000]);
        }

        /// Writable anonymous memory that is *not* butted against the object is
        /// somebody else's, and neither is a heap, whatever it is next to.
        #[test]
        fn the_neighbours_are_left_alone() {
            let plugin = locate(KINDLE, A_SLOT).unwrap();
            assert_eq!(plugin.words_from(0xb6e0_0000), 0);
            let karyll = locate(KINDLE, 0x0001_0100).unwrap();
            assert_eq!(karyll.data, vec![0x000e_3000..0x000e_5000]);
        }

        /// The check that a vtable is still the vtable: eight pointers into
        /// this plugin's own code. The engine's code is not this plugin's, and
        /// neither is the plugin's own data.
        #[test]
        fn only_this_objects_code_counts_as_a_slot() {
            let map = locate(KINDLE, A_SLOT).unwrap();
            assert!(map.holds_code(A_SLOT));
            assert!(!map.holds_code(0xb6d3_f100));
            assert!(!map.holds_code(0xb6a1_c100));
            assert!(!map.holds_code(0xb6f0_0100));
        }

        /// A code pointer in anonymous memory names no file, and an address in
        /// nothing at all names nothing. Either way there is no object to check
        /// the rest of the table against, so there is no plugin.
        #[test]
        fn a_pointer_into_no_file_locates_nothing() {
            assert!(locate(KINDLE, 0xb6e0_0100).is_none());
            assert!(locate(KINDLE, 0xdead_0000).is_none());
        }

        /// Two writable segments with a gap between them, which is what the
        /// word indexing has to survive: the words are contiguous and the
        /// addresses are not.
        const SPLIT: &str = "\
00010000-00011000 r-xp 00000000 00:01 7          /a/plugin.so
00011000-00012000 rw-p 00001000 00:01 7          /a/plugin.so
00013000-00014000 rw-p 00002000 00:01 7          /a/plugin.so
";

        #[test]
        fn a_word_knows_which_address_it_came_from() {
            let map = locate(SPLIT, 0x0001_0100).unwrap();
            assert_eq!(map.words(), 2048);
            assert_eq!(map.address(0), Some(0x0001_1000));
            assert_eq!(map.address(1023), Some(0x0001_1ffc));
            assert_eq!(map.address(1024), Some(0x0001_3000));
            assert_eq!(map.address(2047), Some(0x0001_3ffc));
            assert_eq!(map.address(2048), None);
        }

        /// **Two words next to each other in a snapshot need not be next to
        /// each other in memory**, and the pending-key search reads a pair,
        /// so it asks.
        #[test]
        fn the_last_word_of_a_segment_has_no_neighbour() {
            let map = locate(SPLIT, 0x0001_0100).unwrap();
            assert!(map.adjacent(0));
            assert!(!map.adjacent(1023));
            assert!(map.adjacent(1024));
            assert!(!map.adjacent(2047));
        }

        /// What stops a misread count from walking off the end of the mapping.
        #[test]
        fn there_is_only_so_much_room_after_an_address() {
            let map = locate(SPLIT, 0x0001_0100).unwrap();
            assert_eq!(map.words_from(0x0001_3000), 1024);
            assert_eq!(map.words_from(0x0001_3ff0), 4);
            assert_eq!(map.words_from(0x0001_2000), 0);
        }
    }

    mod composing {
        use super::*;
        use crate::keymap::Action;

        const BOTH: [Script; 2] = [Script::Chinese, Script::Japanese];

        #[test]
        fn lower_case_letters_start_and_continue_a_word() {
            for script in BOTH {
                assert_eq!(
                    compose(&Action::Insert('n'), false, script),
                    Compose::Feed('n')
                );
                assert_eq!(
                    compose(&Action::Insert('i'), true, script),
                    Compose::Feed('i')
                );
            }
        }

        #[test]
        fn a_capital_is_latin_and_never_reaches_the_engine() {
            // Pinyin and romaji are written in lower case; a capital is
            // Latin for the page.
            for script in BOTH {
                assert_eq!(
                    compose(&Action::Insert('N'), true, script),
                    Compose::Latin('N')
                );
                // And with nothing composing, so that a capital lands on the
                // page rather than opening a word.
                assert_eq!(
                    compose(&Action::Insert('N'), false, script),
                    Compose::Latin('N')
                );
            }
        }

        #[test]
        fn the_letters_as_struck_can_be_had_back() {
            // `F10` inserts the letters as struck, in both languages.
            for script in BOTH {
                assert_eq!(
                    compose(&Action::CommitTyped, true, script),
                    Compose::CommitTyped
                );
            }
        }

        /// With nothing composed, everything except letters and punctuation
        /// behaves as it does in English.
        #[test]
        fn editing_keys_are_untouched_before_a_word_starts() {
            for script in BOTH {
                for action in [
                    Action::Insert('1'),
                    Action::Insert(' '),
                    Action::Newline,
                    Action::Backspace,
                    Action::Left,
                    Action::Save,
                ] {
                    assert_eq!(
                        compose(&action, false, script),
                        Compose::Pass,
                        "{action:?} {script:?}"
                    );
                }
            }
        }

        #[test]
        fn the_number_row_picks_a_candidate_and_zero_is_the_tenth() {
            for script in BOTH {
                assert_eq!(
                    compose(&Action::Insert('1'), true, script),
                    Compose::Select(0)
                );
                assert_eq!(
                    compose(&Action::Insert('9'), true, script),
                    Compose::Select(8)
                );
                assert_eq!(
                    compose(&Action::Insert('0'), true, script),
                    Compose::Select(9)
                );
            }
        }

        /// Space accepts the best candidate in Chinese and asks the engine
        /// for the conversion in Japanese.
        #[test]
        fn space_accepts_in_chinese_and_converts_in_japanese() {
            assert_eq!(
                compose(&Action::Insert(' '), true, Script::Chinese),
                Compose::Select(0)
            );
            assert_eq!(
                compose(&Action::Insert(' '), true, Script::Japanese),
                Compose::Feed(' ')
            );
        }

        /// Backspace belongs to the engine while composing — it drops one unit
        /// and re-predicts — not to the document.
        #[test]
        fn backspace_goes_to_the_engine() {
            for script in BOTH {
                assert_eq!(
                    compose(&Action::Backspace, true, script),
                    Compose::Feed('\u{8}')
                );
            }
        }

        #[test]
        fn enter_keeps_the_composition_as_it_stands() {
            for script in BOTH {
                assert_eq!(compose(&Action::Newline, true, script), Compose::CommitRaw);
            }
        }

        #[test]
        fn escape_and_anything_unexpected_abandon_the_word() {
            for script in BOTH {
                assert_eq!(compose(&Action::Escape, true, script), Compose::Cancel);
                assert_eq!(compose(&Action::LineStart, true, script), Compose::Cancel);
                assert_eq!(compose(&Action::Undo, true, script), Compose::Cancel);
            }
        }

        /// The arrows page the candidates; the word stays composing.
        #[test]
        fn the_arrows_page_the_candidates_rather_than_abandoning_the_word() {
            for script in BOTH {
                for action in [Action::Right, Action::Down, Action::PageDown] {
                    assert_eq!(
                        compose(&action, true, script),
                        Compose::NextPage,
                        "{action:?} {script:?}"
                    );
                }
                for action in [Action::Left, Action::Up, Action::PageUp] {
                    assert_eq!(
                        compose(&action, true, script),
                        Compose::PreviousPage,
                        "{action:?} {script:?}"
                    );
                }
            }
        }

        /// The syllable separator is only a separator once there is something
        /// to separate; before that it is a quotation mark. Japanese romaji has
        /// no separator at all, so there it is always the quotation mark.
        #[test]
        fn the_apostrophe_separates_syllables_only_in_chinese_and_only_mid_word() {
            assert_eq!(
                compose(&Action::Insert('\''), true, Script::Chinese),
                Compose::Feed('\'')
            );
            assert_eq!(
                compose(&Action::Insert('\''), false, Script::Chinese),
                Compose::Punctuate('\'')
            );
            assert_eq!(
                compose(&Action::Insert('\''), true, Script::Japanese),
                Compose::Punctuate('\'')
            );
        }

        /// カレー and コーヒー need it, so mid-word a hyphen is the prolonged
        /// sound mark rather than punctuation — and it goes to the engine as
        /// that character, which `prv_key_handler` tests for by code point.
        #[test]
        fn a_hyphen_mid_word_is_the_japanese_prolonged_sound_mark() {
            assert_eq!(
                compose(&Action::Insert('-'), true, Script::Japanese),
                Compose::Feed('ー')
            );
            // Outside a word it is an ordinary hyphen, and in Chinese it always
            // is: arithmetic and dashes stay ASCII in both scripts.
            assert_eq!(
                compose(&Action::Insert('-'), false, Script::Japanese),
                Compose::Pass
            );
            assert_eq!(
                compose(&Action::Insert('-'), true, Script::Chinese),
                Compose::Cancel
            );
        }

        /// Punctuation is CJK whether or not a word is under way — unlike
        /// digits and space, which mean one thing mid-word and another outside.
        #[test]
        fn punctuation_is_cjk_in_both_states() {
            for script in BOTH {
                for composing in [true, false] {
                    assert_eq!(
                        compose(&Action::Insert('.'), composing, script),
                        Compose::Punctuate('.'),
                        "composing={composing} {script:?}"
                    );
                }
            }
        }
    }

    mod punctuation {
        use super::*;

        const ZH: Script = Script::Chinese;
        const JA: Script = Script::Japanese;

        #[test]
        fn sentence_marks_are_full_width() {
            let mut p = Punctuation::default();
            for (ascii, chinese) in [
                (',', "，"),
                ('.', "。"),
                ('?', "？"),
                ('!', "！"),
                (':', "："),
                (';', "；"),
                ('\\', "、"),
            ] {
                assert_eq!(p.resolve(ZH, ascii), Some(chinese), "{ascii}");
            }
        }

        /// **The comma is the one sentence mark that differs**: Chinese sets
        /// ， and Japanese sets 、 on the same key.
        #[test]
        fn japanese_takes_the_reading_comma_and_shares_the_rest() {
            let mut p = Punctuation::default();
            assert_eq!(p.resolve(JA, ','), Some("、"));
            for (ascii, mark) in [
                ('.', "。"),
                ('?', "？"),
                ('!', "！"),
                (':', "："),
                (';', "；"),
                ('(', "（"),
                (')', "）"),
            ] {
                assert_eq!(p.resolve(JA, ascii), Some(mark), "{ascii}");
                assert_eq!(p.resolve(ZH, ascii), Some(mark), "{ascii}");
            }
        }

        /// Both are doubled in Chinese typography — a lone dash is a hyphen and
        /// three dots are half an ellipsis. Japanese doubles neither.
        #[test]
        fn the_ellipsis_and_dash_are_doubled_in_chinese_only() {
            let mut p = Punctuation::default();
            assert_eq!(p.resolve(ZH, '^'), Some("……"));
            assert_eq!(p.resolve(ZH, '_'), Some("——"));
            assert_eq!(p.resolve(JA, '^'), None);
            assert_eq!(p.resolve(JA, '_'), None);
        }

        /// Both bracket pairs, matching macOS's Simplified Chinese input.
        #[test]
        fn both_bracket_pairs_are_on_the_bracket_keys() {
            let mut p = Punctuation::default();
            assert_eq!(p.resolve(ZH, '['), Some("【"));
            assert_eq!(p.resolve(ZH, ']'), Some("】"));
            assert_eq!(p.resolve(ZH, '{'), Some("「"));
            assert_eq!(p.resolve(ZH, '}'), Some("」"));
        }

        /// Japanese quotes with 「」 far more than Chinese does, so they take
        /// the unshifted keys — where a JIS keyboard has them, and where macOS
        /// puts them — and 『』 takes the shifted pair.
        #[test]
        fn japanese_puts_the_corner_brackets_on_the_unshifted_keys() {
            let mut p = Punctuation::default();
            assert_eq!(p.resolve(JA, '['), Some("「"));
            assert_eq!(p.resolve(JA, ']'), Some("」"));
            assert_eq!(p.resolve(JA, '{'), Some("『"));
            assert_eq!(p.resolve(JA, '}'), Some("』"));
        }

        /// Unlike the quotation marks, the brackets are fixed: each has its own
        /// key, so neither has to alternate.
        #[test]
        fn brackets_do_not_alternate() {
            let mut p = Punctuation::default();
            assert_eq!(p.resolve(ZH, '{'), Some("「"));
            assert_eq!(p.resolve(ZH, '{'), Some("「"));
        }

        #[test]
        fn quotes_alternate_open_and_closed() {
            let mut p = Punctuation::default();
            assert_eq!(p.resolve(ZH, '"'), Some("“"));
            assert_eq!(p.resolve(ZH, '"'), Some("”"));
            assert_eq!(p.resolve(ZH, '"'), Some("“"));
        }

        /// The two pairs alternate independently.
        #[test]
        fn the_two_quote_pairs_do_not_share_state() {
            let mut p = Punctuation::default();
            assert_eq!(p.resolve(ZH, '"'), Some("“"));
            assert_eq!(p.resolve(ZH, '\''), Some("‘"));
            assert_eq!(p.resolve(ZH, '\''), Some("’"));
            assert_eq!(p.resolve(ZH, '"'), Some("”"));
        }

        /// Their CJK forms are rare in prose; the keys stay ASCII.
        #[test]
        fn arithmetic_and_currency_stay_ascii() {
            let mut p = Punctuation::default();
            for script in [ZH, JA] {
                for key in ['-', '/', '=', '+', '%', '#', '&', '$', '*'] {
                    assert_eq!(p.resolve(script, key), None, "{key} {script:?}");
                }
            }
        }
    }
}
