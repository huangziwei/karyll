//! CJK input, through Amazon's own predictor plugins.
//!
//! The device ships a complete IME for each of twenty languages under
//! `/usr/share/keyboard/<locale>/libpredictor.so.1` — engine, dictionaries and
//! keyboard databases. karyll drives those plugins directly rather than
//! reimplementing pinyin or romaji, which is why the whole of this file is a
//! binding and almost none of it is linguistics.
//!
//! Two are used. **Chinese** is `zh_CN`, XT9 over `libxt9a`, and **Japanese** is
//! `ja`, Omron iWnn over `libwlf` with ICU doing the romaji-to-kana
//! transliteration from Amazon's own `hiragana_rules.txt`. They are different
//! engines with different dictionaries, and they share one ABI — the table
//! below is `libkb`'s, not any one language's — so [`Plugin`] is written once
//! and the languages differ only in the facts around it.
//!
//! `load(host)` returns a 48-byte block of function pointers and, importantly,
//! **performs the entire engine initialisation itself**. For Chinese that is
//! `ET9CPSysInit`, `ET9CPLdbInit`, `ET9CPSetInputMode`, `ET9CPSetFullSentence`,
//! `ET9CPUdbActivate`, `ET9KDB_Init`, `ET9KDB_SetPageNum`,
//! `ET9KDB_SetDiscreteMode` and the `mmap` of the two databases; for Japanese
//! it is `wlf_init`, `wlf_set_state`, `wlf_load_lang` on
//! `/usr/share/keyboard/ja/JA.conf` and `wlf_set_active_lang`. Nothing in the
//! table brings the engine up. By the time `load()` returns, it is up, and the
//! table is a *session* API.
//!
//! Three things about the calling convention, each of which was got wrong at
//! least once and cost a device round trip:
//!
//! * **There is no `self`/context argument.** Every slot resolves its own
//!   context PC-relative from the plugin's `.bss`. The pointer `load()` stores
//!   at `+0x2c` is its own bookkeeping, not something to pass back.
//! * **`userData` is the last argument**, not the first, and the plugin only
//!   ever logs it. `0` is what we send.
//! * **`+0x00` and `+0x08` are the teardown pair**, not the setup pair. Calling
//!   them first unloads and closes the engine `load()` just built, and
//!   everything after that runs on freed memory.
//!
//! | slot | zh_CN | ja | signature |
//! |---|---|---|---|
//! | `+0x00` | `0x1b9c` | `0x218c` | `prv_unload(userData) -> int` |
//! | `+0x04` | `0x13b8` | `0x1818` | `prv_open(flags, userData) -> int` — begin a session |
//! | `+0x08` | `0x1ef0` | `0x1c44` | `prv_close(userData) -> int` — ends it, writes the user dictionary |
//! | `+0x0c` | `0x1a40` | `0x19e4` | `prv_set_surround(str, position, userData) -> int` |
//! | `+0x10` | `0x117c` | `0x1b00` | `(out: *mut c_char, capacity: usize)` — the composition so far |
//! | `+0x14` | `0x1528` | `0x2510` | `prv_key_handler(key: u32, userData) -> int` |
//! | `+0x18` | `0x16fc` | `0x1904` | commit: `(index: u32, userData) -> int` |
//! | `+0x1c` | `0x1182` | `0x1ce0` | `prv_get_candidate_list(out: *mut *mut c_char, count: *mut u32, userData)` |
//!
//! Both unnamed slots are unnamed because they carry no entry trace, not
//! because they are obscure: every other slot logs its own `__func__` and these
//! two never log at all. `+0x18` is `prv_candidate_selected` in Chinese and has
//! no name in Japanese.
//!
//! All return `int`, 0 = ok — **except `+0x1c`, which returns nothing**. It is
//! the one slot that never sets `r0` on its exit path, so a caller reads
//! leftover register content. On device it returned 1, 3, 5, 7, 7 across five
//! keystrokes that all produced perfect candidates. Treating it as a status
//! means never showing a candidate. The out-count is the answer.
//!
//! `+0x1c` fills a caller-supplied `char *` array with pointers **borrowed**
//! from the plugin's own fixed-stride candidate table, valid only until the
//! next call, and overwrites `*count` with how many it produced. Commit takes
//! the index of that same table, so the caller always already holds the text it
//! committed: the host callback that reports it is confirmation, not delivery.
//!
//! The key is a **Unicode codepoint** — ASCII for pinyin and romaji, not a
//! keycode and not an index. Each engine handles a couple of keys itself, which
//! is why they are forwarded rather than special-cased here.

use std::ffi::{c_char, c_void};

/// Which language's rules apply — the input method, and the punctuation that
/// goes with it.
///
/// Not the same thing as the plugin: Simplified and Traditional Chinese are one
/// engine and one set of rules, differing only in what the candidates are
/// converted to on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Chinese,
    Japanese,
}

/// What the editor needs from an input method.
///
/// Small on purpose, and a trait so the contract can be tested against a stub:
/// the real engine is a device file that cannot be redistributed, so anything
/// only exercisable through it would be exercised once, on hardware, by hand.
pub trait Ime {
    /// Feed one key and return the candidates now available, best first.
    fn key(&mut self, key: char) -> Vec<String>;

    /// Accept candidate `index`: the engine records the choice, updates its
    /// context and clears itself for the next word.
    ///
    /// It returns nothing, because the caller already has the text — it is the
    /// candidate it just selected. The plugin does hand the committed string
    /// back through a host callback, but that is confirmation, not delivery.
    fn commit(&mut self, index: usize);

    /// Abandon whatever is being composed.
    fn clear(&mut self);

    /// Hand back Traditional characters rather than Simplified.
    ///
    /// The device ships exactly one Chinese dictionary — `zh_CN.ldb`, which is
    /// Simplified pinyin — so Traditional is not a second engine but the same
    /// candidates converted. Amazon's own Traditional locales are Cangjie and
    /// Zhuyin, neither of which is pinyin, so there was never a stock
    /// Traditional-from-pinyin to borrow.
    fn set_traditional(&mut self, traditional: bool);

    /// The engine's own reading of what has been typed, when it keeps one.
    ///
    /// Japanese needs this and Chinese does not, which is the whole reason it
    /// exists. Pinyin *is* the letters typed, so karyll can show them itself;
    /// romaji is not what the writer means to see — typing `nihon` should show
    /// にほん, and only the engine knows that, because it holds the ICU
    /// transliteration. `None` means "show what was typed".
    fn preedit(&self) -> Option<String> {
        None
    }
}

/// How many candidates to ask for. Ten fits a bar across the page and matches
/// the number row, which is how they are chosen.
pub const WANTED: usize = 10;

/// CJK punctuation for the ASCII key that produces it.
///
/// Amazon's own keymaps are no help: they are on-screen keyboards with a *page*
/// of symbols to tap, so they never had to map a physical punctuation key onto
/// its CJK form. Neither does either engine — `hiragana_rules.txt` maps letters
/// and nothing else, so a `.` typed in Japanese mode reaches the preedit as a
/// full stop and stays one. This is the half of "typing CJK" that has nothing
/// to do with prediction, and every bit of it has to be supplied here.
///
/// **macOS is the reference**, by user instruction: it is what these hands
/// already know, so where a mapping is arguable karyll copies it rather than
/// picking. That is why Chinese puts both bracket pairs on the bracket keys and
/// Japanese puts 「」 on the unshifted ones, matching a JIS keyboard.
///
/// Deliberately not exhaustive, and the same reasoning in both languages: `-`,
/// `/`, `=`, `+`, `%`, `#`, `&` and `*` stay as they are, because their CJK
/// forms are rare in prose and a writer who wanted one would be more surprised
/// than served. `$` is left alone for the same reason — mapping it to ￥ is
/// conventional, but this is a bilingual writer's editor and a dollar sign in
/// CJK text is likelier than a yuan sign.
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
            // Japanese doubles neither: 〜 already spans, and the ellipsis is
            // written as a pair only in typeset prose, which is a decision for
            // the writer rather than the keyboard.
            _ => common(),
        },
    }
}

/// A mark that is always the same, or one that alternates open and closed.
enum Punct {
    Fixed(&'static str),
    Paired(&'static str, &'static str),
}

/// The quote-pairing state, which is the only thing about punctuation that has
/// to be remembered between keystrokes.
///
/// Chinese quotation marks are directional and the keyboard has one key for
/// each pair, so the same key has to alternate. Kept per document rather than
/// per paragraph: a quotation that opens on one line and closes three lines
/// later is ordinary prose.
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

/// What a keystroke means while Chinese input is switched on.
#[derive(Debug, PartialEq, Eq)]
pub enum Compose {
    /// Send this to the engine.
    Feed(char),
    /// Take candidate `n`, counting from zero.
    Select(usize),
    /// Insert the Chinese form of this ASCII punctuation key.
    Punctuate(char),
    /// Insert the pinyin exactly as typed and stop composing. Enter does this,
    /// so an English word typed without switching modes is not lost.
    CommitRaw,
    /// Finish the word under way and add this Latin character to the document
    /// directly, without the engine seeing it.
    Latin(char),
    /// Insert the letters as they were struck, rather than what they were
    /// converted into. Japanese needs this and Chinese does not: pinyin's
    /// preedit *is* the letters, so `CommitRaw` already gives them back, while
    /// romaji has become kana by then and the letters are only in `typed`.
    CommitTyped,
    /// Abandon the composition. The keystroke is consumed.
    Cancel,
    /// Nothing to do with input — the editor should handle it as usual.
    Pass,
}

/// The prolonged sound mark, which lengthens the preceding kana — ラーメン.
///
/// It reaches the engine as itself rather than as the `-` that was typed:
/// `prv_key_handler` compares the key against this code point explicitly, so it
/// is a key the engine expects, and the romaji rules have no mapping that would
/// produce it from a hyphen.
const CHOONPU: char = 'ー';

/// Decide what a keystroke means while CJK input is on.
///
/// `composing` is whether anything has been typed towards a word yet, and it
/// changes almost every rule: a digit is a candidate number mid-word and a
/// digit otherwise, space converts mid-word and is a space otherwise. Without
/// that distinction the mode would make it impossible to type a number.
///
/// Pure, so all of this is tested without an engine, a window or a keyboard.
pub fn compose(action: &crate::keymap::Action, composing: bool, script: Script) -> Compose {
    use crate::keymap::Action;

    match action {
        // **A capital is never CJK input.** Pinyin and romaji are both written
        // in lower case, so an upper-case letter can only be Latin the writer
        // wants on the page — an acronym, a name, the start of an English
        // sentence inside a Chinese one. Folded to lower case and fed to the
        // engine, capitals are unreachable in all three CJK modes: `NASA` comes
        // out as a pinyin guess.
        //
        // Before the letter rule below, and deliberately not conditional on
        // `composing`: with the mode on and nothing typed yet, a capital would
        // otherwise open a composition rather than land on the page.
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

        // Punctuation is CJK whenever the mode is on, composing or not. This is
        // the half of "typing CJK" that has nothing to do with prediction, and
        // leaving it out gives CJK words in English-punctuated sentences.
        Action::Insert(c) if punctuation(script, *c).is_some() => Compose::Punctuate(*c),

        _ if !composing => Compose::Pass,

        // The number row picks a candidate, as it does in every CJK IME, and 0
        // is the tenth rather than the zeroth.
        Action::Insert(c @ '1'..='9') => Compose::Select(*c as usize - '1' as usize),
        Action::Insert('0') => Compose::Select(9),

        // **Space is the one rule the two languages disagree on**, and it is a
        // difference in the languages rather than in the plugins. Pinyin
        // predicts as you type, so by the time a word is spelled the best
        // candidate is already offered and space accepts it. Japanese does not:
        // kana is unambiguous and the *conversion* to kanji is the step that
        // needs asking for, which is what space means to a Japanese writer and
        // what `prv_key_handler` implements — 0x20 starts and then advances the
        // selection. So Chinese takes the candidate and Japanese asks for one.
        Action::Insert(' ') => match script {
            Script::Chinese => Compose::Select(0),
            Script::Japanese => Compose::Feed(' '),
        },

        // Backspace goes to the engine rather than the document. Chinese routes
        // 0x08 to `ET9ClearOneSymb`; Japanese truncates its own preedit buffer
        // one UTF-8 character back. Either way the engine drops one unit and
        // re-predicts from what is left, which is not something the editor
        // could do on its behalf.
        Action::Backspace => Compose::Feed('\u{8}'),

        Action::Newline => Compose::CommitRaw,
        Action::Escape => Compose::Cancel,

        // **The way out of kana for a Latin word.** `F10` converts the reading
        // to half-width Latin in every Japanese IME, and it is the answer to a
        // real gap: romaji becomes kana as it is typed, so with the mode on
        // there was no way to write a lower-case Latin word at all. Chinese
        // gets it too, where it does the same thing Enter already does.
        Action::CommitTyped => Compose::CommitTyped,

        // Anything else — an arrow, a page key, a Ctrl chord — abandons the
        // composition and is consumed. Moving the cursor out from under a
        // half-typed word and leaving it pending would be worse than costing
        // one keystroke.
        _ => Compose::Cancel,
    }
}

/// The Chinese plugin: XT9 pinyin over `libxt9a`, one dictionary, Simplified.
const PLUGIN_ZH: &str = "/usr/share/keyboard/zh_CN/libpredictor.so.1";

/// The Japanese plugin: Omron iWnn over `libwlf`, with ICU doing romaji to
/// kana. A heavier neighbour than the Chinese one — it names nineteen shared
/// libraries against Chinese's four, GTK and ICU among them — but every one of
/// them is already on the device, because the stock keyboard needs them too.
const PLUGIN_JA: &str = "/usr/share/keyboard/ja/libpredictor.so.1";

/// How many candidate pointers the plugin is given room to write.
///
/// **Sized to the plugin's own table, not to [`WANTED`], because Japanese
/// ignores what is asked for and writes as many as it produced.** Both tables
/// are bounded by the `.bss` they live in — the Japanese one starts `0x51b0`
/// into a section ending `0x8284`, at a 50-byte stride, so 250 entries is all
/// it can physically hold, and Chinese preallocates 500 buffers of 41 bytes.
/// This is the larger of the two with the reasoning attached, so that shrinking
/// it later is understood to be a memory-safety change and not a tidy-up.
const MAX_CANDIDATES: usize = 500;

/// Opaque host cookie, passed last to every slot and only ever logged.
const USER_DATA: u32 = 0;

/// Where the plugin keeps its ET9 Chinese-phonetic context, as an offset from
/// the library's load base.
///
/// The plugin never hands this out — it resolves it PC-relative from its own
/// `.bss` on every call. It is recovered the same way the plugin does it, from
/// the literal `+0x1c` loads at `0x11e2` just before `ET9CPBuildSelectionList`:
/// `word_at(0x1394) + 0x11e8`. The result lands inside `.bss` (which spans
/// `0x40ec`..`0x3ba20`), which is the check that the arithmetic is right.
const CP_CONTEXT: usize = 0x31c14;

/// `ET9CPSimplifiedToTraditional` refuses any context whose word at `+0x88` is
/// not this, so the pointer above can be *verified* rather than trusted. Worth
/// more than it looks: a wrong context would otherwise be a silent corruption
/// of the engine's own state.
const CP_MAGIC_OFFSET: usize = 0x88;
const CP_MAGIC: u32 = 0x1428_1428;

/// `+0x10`'s address inside each plugin, which is how the load base is
/// recovered: the slot pointer minus its known address. Any slot would do; this
/// one is used because both plugins' `+0x10` bodies are short and distinctive,
/// so the address was easy to be sure of.
///
/// The base is what turns an offset like [`CP_CONTEXT`] or [`JA_STATE_READY`]
/// into a real pointer, and every use of it is checked against something the
/// plugin itself wrote.
const PREEDIT_ADDR_ZH: usize = 0x117c;
const PREEDIT_ADDR_JA: usize = 0x1b00;

/// Where the Japanese plugin keeps its own state block, as an offset from the
/// load base, and the readiness flag inside it.
///
/// **`load()` hands back a complete-looking vtable even when the engine failed
/// to come up.** Its error paths log and fall through to the same return, so
/// the pointer says nothing about whether `wlf_init`, `wlf_load_lang` and
/// `wlf_set_active_lang` succeeded. This flag is the last thing `load()` writes
/// and only on the path where all of them did, so it is the difference between
/// "the plugin loaded" and "Japanese works".
const JA_STATE: usize = 0x5108;
const JA_STATE_READY: usize = 0x58;

/// The composition buffer is 52 bytes and a candidate 50, so this is past any
/// valid end of either.
const PREEDIT_CAPACITY: usize = 256;

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

/// One of Amazon's predictor plugins, loaded and ready.
///
/// The ABI is `libkb`'s rather than any one language's, so this is written once
/// and both languages use it. What differs between them lives in [`Chinese`]
/// and [`Japanese`]: which file to open, whether the session has to be opened
/// explicitly, and what to do with the candidates on the way out.
struct Plugin {
    table: *const usize,
    handle: *mut c_void,
    /// Where `+0x10` sits inside this particular plugin, which is what makes
    /// the load base recoverable.
    preedit_addr: usize,
}

impl Plugin {
    /// `dlopen` the plugin and call `load()`, which brings its engine up.
    ///
    /// Nothing is preloaded alongside it. Each plugin's own `DT_NEEDED` names
    /// the engine it was built against — `libxt9a.so.1` for Chinese — and
    /// neither `kb` nor `libkb.so` links any `libxt9*`, so that is where its
    /// calls bind under Amazon's own run too. The three XT9 engines are one
    /// build with different embedded data, every entry point at an identical
    /// address, so preloading the wrong one would interpose silently.
    fn open(path: &str, preedit_addr: usize) -> Result<Plugin, String> {
        // The host block is what the plugin calls back into. It must hold
        // callable pointers, not zeroes: committing a candidate calls into it,
        // and a zeroed block would be a call through null. karyll does not need
        // anything the callbacks carry — it already holds the text it committed
        // — so they are no-ops that return 0.
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
        Ok(Plugin {
            table: table as *const usize,
            handle,
            preedit_addr,
        })
    }

    fn slot(&self, byte_offset: usize) -> usize {
        unsafe { *self.table.add(byte_offset / 4) }
    }

    /// Where the plugin was mapped, from a slot pointer and its known address.
    /// The Thumb bit has to come off first — every one of these addresses is
    /// odd in the table.
    fn base(&self) -> Option<usize> {
        (self.slot(SLOT_PREEDIT) & !1).checked_sub(self.preedit_addr)
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
    /// it produced and fills that many array slots regardless — its inner loop
    /// bounds itself on the engine's own total and never looks at the request.
    /// So the array is sized to the plugin's whole preallocation rather than to
    /// what is wanted, and the result is truncated here instead.
    ///
    /// Getting that wrong is what put a black smear across the bottom of the
    /// screen the first time Japanese ran: iWnn answers a two-letter reading
    /// with dozens of conversions and width variants, every one of them drawn
    /// as an equal share of the strip.
    ///
    /// The strings are borrowed from the plugin's own fixed-stride table and
    /// are only valid until the next call, so they are copied here.
    fn call_candidates(&self) -> Vec<String> {
        let mut slots: Vec<*mut c_char> = vec![std::ptr::null_mut(); MAX_CANDIDATES];
        let mut count: u32 = WANTED as u32;
        let f: unsafe extern "C" fn(*mut *mut c_char, *mut u32, u32) =
            unsafe { std::mem::transmute(self.slot(SLOT_CANDIDATES)) };
        unsafe { f(slots.as_mut_ptr(), &mut count, USER_DATA) };

        let produced = (count as usize).min(MAX_CANDIDATES);
        slots[..produced]
            .iter()
            .filter_map(|p| unsafe { c_string(*p) })
            .take(WANTED)
            .collect()
    }

    /// `+0x18` — accept the candidate at `index`.
    fn call_commit(&self, index: usize) {
        let f: unsafe extern "C" fn(u32, u32) -> i32 =
            unsafe { std::mem::transmute(self.slot(SLOT_COMMIT)) };
        unsafe { f(index as u32, USER_DATA) };
    }

    /// `+0x10` — copy out the composition as the engine understands it.
    ///
    /// Empty is `None` rather than `Some("")`, because "nothing is being
    /// composed" and "the composition is the empty string" want the same
    /// treatment from the caller and only one of them is a real state.
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
    /// Close and unload, in that order, as the plugin's own lifecycle wants.
    /// Closing writes the user dictionary back to disk — Amazon's `xt9-zh.*`
    /// for Chinese, the iWnn learning data for Japanese — so a session's
    /// learned phrases survive.
    ///
    /// `panic = "abort"` skips `Drop`, so a panic loses the dictionary update.
    /// That is a fair trade: the alternative is running teardown from a broken
    /// state, and the document is protected by autosave rather than by this.
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
}

impl Chinese {
    pub fn open() -> Result<Chinese, String> {
        let plugin = Plugin::open(PLUGIN_ZH, PREEDIT_ADDR_ZH)
            .map_err(|e| format!("{e} — no Chinese input"))?;
        let mut zh = Chinese {
            plugin,
            converter: None,
            want_traditional: false,
        };
        zh.find_converter();
        // Begin a session. Skipping this produced identical candidates on
        // device, so nothing Chinese depends on it, but it is the documented
        // lifecycle and it costs one call at startup.
        let st = zh.plugin.call_open();
        if st != 0 {
            eprintln!("ime: zh prv_open returned {st}, continuing");
        }
        Ok(zh)
    }

    /// Find the engine's own Simplified-to-Traditional converter, and the
    /// context it needs.
    ///
    /// The device has one Chinese dictionary and it is Simplified, so this is
    /// the only route to Traditional that keeps pinyin as the input method.
    /// Both halves are checked before either is kept: a wrong context would not
    /// merely fail, it would let a stranger's pointer into the engine's state.
    fn find_converter(&mut self) {
        let convert = self.plugin.symbol("ET9CPSimplifiedToTraditional");
        if convert.is_null() {
            eprintln!("ime: no ET9CPSimplifiedToTraditional — Traditional unavailable");
            return;
        }
        let Some(base) = self.plugin.base() else {
            eprintln!("ime: cannot recover the zh plugin's load base");
            return;
        };
        let ctx = (base + CP_CONTEXT) as *mut c_void;
        let magic = unsafe { *(ctx.byte_add(CP_MAGIC_OFFSET) as *const u32) };
        if magic != CP_MAGIC {
            eprintln!(
                "ime: CP context at {ctx:p} reads 0x{magic:08x}, not 0x{CP_MAGIC:08x} — Traditional unavailable"
            );
            return;
        }
        self.converter = Some((
            unsafe { std::mem::transmute::<*mut c_void, ToTraditional>(convert) },
            ctx,
        ));
        eprintln!("ime: Traditional conversion available");
    }

    /// Convert one candidate to Traditional, in the engine's own terms.
    ///
    /// The engine works in 16-bit symbols and converts in place, so the string
    /// makes a round trip through UTF-16. Anything outside the basic plane is
    /// left alone rather than mangled: `encode_utf16` would split it into a
    /// surrogate pair the converter would treat as two characters.
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

impl Ime for Chinese {
    fn key(&mut self, key: char) -> Vec<String> {
        self.plugin.call_key(key);
        let candidates = self.plugin.call_candidates();
        // Converted here rather than on commit, so the bar shows what will
        // actually be inserted. Offering Simplified and inserting Traditional
        // would be worse than not offering Traditional at all.
        if self.want_traditional {
            candidates.iter().map(|c| self.to_traditional(c)).collect()
        } else {
            candidates
        }
    }

    fn set_traditional(&mut self, traditional: bool) {
        self.want_traditional = traditional && self.converter.is_some();
    }

    fn commit(&mut self, index: usize) {
        self.plugin.call_commit(index);
    }

    fn clear(&mut self) {
        // Space is the engine's own "clear everything": prv_key_handler routes
        // 0x20 to ET9CPClearContext + ET9ClearAllSymbs. Reopening the session
        // does the same thing and says so more plainly.
        self.plugin.call_open();
    }
}

/// Japanese: romaji in, kana and kanji out, through iWnn.
pub struct Japanese {
    plugin: Plugin,
}

impl Japanese {
    pub fn open() -> Result<Japanese, String> {
        let plugin = Plugin::open(PLUGIN_JA, PREEDIT_ADDR_JA)
            .map_err(|e| format!("{e} — no Japanese input"))?;

        // **Unlike Chinese, opening the session is mandatory**, and this is the
        // one place the two lifecycles genuinely differ. `prv_open` sets the
        // plugin's input mode, and until it runs the mode is whatever `.bss`
        // was zeroed to — a value on which the key handler takes a different
        // branch and the composition getter returns nothing at all. Chinese
        // tolerates skipping it; Japanese silently does nothing.
        let st = plugin.call_open();
        if st != 0 {
            eprintln!("ime: ja prv_open returned {st}, continuing");
        }

        // `load()` returns a complete-looking table whether or not the engine
        // came up, so ask the plugin what it thinks rather than trusting the
        // pointer. Without this the failure mode is an editor that swallows
        // every keystroke and shows nothing.
        let Some(base) = plugin.base() else {
            return Err("cannot recover the ja plugin's load base".into());
        };
        let ready = unsafe { *((base + JA_STATE + JA_STATE_READY) as *const u32) };
        if ready != 1 {
            return Err(format!(
                "the ja plugin loaded but its engine did not come up (ready={ready}) \
                 — check that /usr/share/keyboard/ja/JA.conf and the iWnn dictionaries are present"
            ));
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

    fn commit(&mut self, index: usize) {
        self.plugin.call_commit(index);
    }

    fn clear(&mut self) {
        // Reopening resets the composition, the selection and the input mode
        // together, which is exactly what abandoning a word should do.
        self.plugin.call_open();
    }

    /// The kana, not the romaji. This is why the trait has the method: the
    /// engine holds the ICU transliteration and karyll would otherwise show
    /// `nihon` where the writer means にほん.
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

/// Room for the host callback table. The plugin's own host struct is opaque to
/// us; this is comfortably larger than the sixteen slots filled below.
const HOST_BLOCK: usize = 0x400;

/// A host callback that does nothing.
///
/// The plugin calls into the host block to report a commit and to ask about
/// surrounding text. karyll needs neither — it already holds the text it
/// committed — but the pointers have to be callable, so they point here.
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
        ];
        canned
            .iter()
            .find(|(k, _)| *k == self.typed)
            .map(|(_, v)| v.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
impl Ime for Stub {
    fn key(&mut self, key: char) -> Vec<String> {
        match key {
            // The engine handles these itself, so the stub has to as well or
            // the editor would behave differently against the two.
            '\u{8}' => {
                self.typed.pop();
            }
            ' ' => self.typed.clear(),
            c => self.typed.push(c),
        }
        self.lookup()
    }

    fn commit(&mut self, _index: usize) {
        self.typed.clear();
    }

    fn clear(&mut self) {
        self.typed.clear();
    }

    /// The stub has no engine to convert with, so it reports Traditional as
    /// unavailable rather than pretending.
    fn set_traditional(&mut self, _traditional: bool) {}
}

#[cfg(test)]
impl Default for Stub {
    fn default() -> Self {
        Self::new()
    }
}

/// An engine that composes something other than the keys it was given.
///
/// Enough romaji to show the difference that motivates [`Ime::preedit`]: the
/// keys are `nihon` and the composition is にほん, so an editor that displayed
/// what it typed rather than what the engine composed would be visibly wrong.
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
            // A partial syllable stays as the letters typed, which is what a
            // real transliterator does too — `nih` composes にh.
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

    fn commit(&mut self, _index: usize) {
        self.typed.clear();
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

    #[test]
    fn the_stub_grows_a_preedit_and_narrows_the_candidates() {
        let mut ime = Stub::new();
        assert_eq!(ime.key('n').first().map(String::as_str), Some("你"));
        assert_eq!(ime.key('i').len(), 5);
        assert_eq!(ime.key('h').first().map(String::as_str), Some("你好"));
    }

    /// Backspace and space are handled inside `prv_key_handler` — `0x08` clears
    /// one symbol, `0x20` clears all — so the editor forwards them rather than
    /// intercepting them, and the stub has to agree or it would test a
    /// different editor than the device runs.
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
        ime.commit(0);
        assert_eq!(ime.key('h').first().map(String::as_str), Some("和"));
    }

    /// Pinyin *is* the letters typed, so the Chinese engine reports no
    /// composition of its own and the editor shows what it sent. This is the
    /// default arm of [`Ime::preedit`], and the reason it has a default.
    #[test]
    fn chinese_keeps_no_composition_of_its_own() {
        let mut ime = Stub::new();
        ime.key('n');
        ime.key('i');
        assert_eq!(ime.preedit(), None);
    }

    /// Japanese does, and it is not the letters typed — which is the whole
    /// reason the trait has the method. An editor showing `nihon` where the
    /// writer means にほん is showing its own plumbing.
    #[test]
    fn japanese_composes_kana_from_the_romaji_it_was_sent() {
        let mut ime = KanaStub::new();
        for c in "nihon".chars() {
            ime.key(c);
        }
        assert_eq!(ime.preedit().as_deref(), Some("にほん"));
        assert_eq!(ime.key(' ').first().map(String::as_str), Some("日本"));
    }

    /// Backspace reaches the engine, so the composition shortens by a *kana*
    /// rather than by a byte. An editor trimming its own copy would cut にほ
    /// down the middle of a UTF-8 character.
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
            // Pinyin and romaji are both written in lower case, so an
            // upper-case letter can only be Latin the writer wants on the page.
            // Folded to lower case and fed to the engine, capitals are
            // unreachable in every CJK mode — `NASA` comes out as a pinyin
            // guess.
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
            // Romaji has become kana by the time it is on screen, so with the
            // mode on there was no way to write a lower-case Latin word at all.
            // `F10` is where every Japanese IME puts the way out.
            for script in BOTH {
                assert_eq!(
                    compose(&Action::CommitTyped, true, script),
                    Compose::CommitTyped
                );
            }
        }

        /// The rule that makes CJK mode usable rather than modal: with nothing
        /// being composed, everything except letters and punctuation behaves
        /// exactly as it does in English. Without this, switching the mode on
        /// would make it impossible to type a number or press Enter.
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

        /// **The one rule the two languages disagree on.** Pinyin predicts as
        /// it goes, so by the time a word is spelled the best candidate is
        /// already offered and space accepts it. Japanese kana is unambiguous
        /// and it is the conversion to kanji that has to be asked for, which is
        /// what space means there — so it goes to the engine instead.
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
                assert_eq!(compose(&Action::Left, true, script), Compose::Cancel);
                assert_eq!(compose(&Action::PageDown, true, script), Compose::Cancel);
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
            // is — the plan leaves arithmetic and dashes ASCII in both.
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

        /// **The comma is the one sentence mark that differs.** Chinese sets ，
        /// and Japanese sets 、 — the same key, a different mark, and using the
        /// Chinese one in Japanese prose is the sort of error that reads as
        /// foreign rather than as a typo.
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

        /// Both bracket pairs, matching macOS's Simplified Chinese input, which
        /// is what this writer's hands already know.
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

        /// The two pairs alternate independently, or a single quote inside a
        /// double one would flip the wrong mark.
        #[test]
        fn the_two_quote_pairs_do_not_share_state() {
            let mut p = Punctuation::default();
            assert_eq!(p.resolve(ZH, '"'), Some("“"));
            assert_eq!(p.resolve(ZH, '\''), Some("‘"));
            assert_eq!(p.resolve(ZH, '\''), Some("’"));
            assert_eq!(p.resolve(ZH, '"'), Some("”"));
        }

        /// Left alone on purpose, in both languages: their CJK forms are rare
        /// in prose, and a bilingual writer means these literally.
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
