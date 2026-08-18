//! karyll — a Markdown writing app for the Kindle Scribe.

mod evdev;
mod font;
mod hid;
mod ime;
mod keymap;
mod lexicon;
mod orientation;
mod pen;
mod power;
mod render;
mod touch;
mod udev;
mod ui;
mod window;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use karyll_core::script::Region;
use karyll_core::{Dict, Document};

use font::Metrics as _;
use keymap::{Action, Mods};

/// Stamped in by `build.sh` so a log says which binary wrote it.
/// Without it there is no way to tell a stale copy on the device from a fresh
/// one, and every symptom has to be diagnosed twice.
const BUILD: &str = match option_env!("KARYLL_BUILD") {
    Some(stamp) => stamp,
    None => "dev",
};

/// The document named on the command line, or an empty one where there is no
/// file yet.
///
/// **A file that is not there is a document that has not been written yet**,
/// which is what the launcher hands over on a Kindle karyll has never run on:
/// it names the welcome document before anything has created it. Refusing to
/// start was the worst answer available — the editor exited before it drew
/// anything, so what the writer saw was a tile that does nothing.
///
/// Anything else is still an error. A file that exists and cannot be read is a
/// permission or a disk fault, and opening an empty page over the top of it
/// would invite the writer to save an empty page over their draft.
fn read_document(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "document: {} does not exist yet, starting empty",
                path.display()
            );
            Ok(String::new())
        }
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn main() -> Result<()> {
    eprintln!("karyll {} build {BUILD}", env!("CARGO_PKG_VERSION"));
    if std::env::args().nth(1).as_deref() == Some("--pair") {
        return pair();
    }
    catch_signals();
    let mut fonts = font::Fonts::load(read_choices())?;
    // A firmware that has moved or dropped a face otherwise shows up only as
    // text drawn in the wrong style, so say what was found.
    for path in fonts.present() {
        eprintln!("font: {path}");
    }
    for group in font::GROUPS {
        eprintln!(
            "font: {} in {} ({} installed)",
            group.label(),
            fonts.family(group).name,
            font::available(group).len()
        );
    }

    // **Nothing else makes this.** It sits outside the extension so that an
    // update cannot take a draft with it, which also means no install step
    // creates it — on a Kindle karyll has never run on it is simply not there,
    // and then the file list is empty, a new draft cannot be written, and the
    // welcome document has nowhere to land. karyll owns the directory, so
    // karyll makes it, however it was started.
    if let Err(e) = std::fs::create_dir_all(DOCUMENTS) {
        eprintln!("documents: cannot create {DOCUMENTS}: {e}");
    }

    let path = std::env::args().nth(1).map(PathBuf::from);
    let mut doc = match &path {
        Some(p) => Document::from_text(&read_document(p)?),
        // The specimen is a demonstration, not a draft — it opens at the top.
        None => Document::from_text(SPECIMEN),
    };
    if let Some(p) = &path {
        let at = opening_cursor(p, doc.len());
        eprintln!("cursor: {at} of {}", doc.len());
        doc.set_cursor(at);
    }

    // Bring the Bluetooth stack up ourselves. There is no kernel Bluetooth on
    // this device, so a userspace daemon is the only route to a keyboard, and
    // karyll owns its lifetime: starting it displaces the stock stack, and by
    // default it is stopped again on exit so Audible and VoiceView only go away
    // while the editor is open.
    //
    // The writer can say otherwise — see [`hid::Hid::set_keep_alive`]. The
    // setting is read before `start`, which needs it to tell an adoption from a
    // replacement.
    //
    // A failure here is reported and not fatal. The editor is still worth
    // opening — the document is readable, and a keyboard paired later is
    // picked up without a restart.
    // Tag the keyboard for X before the daemon creates it, so a keyboard that
    // arrives while karyll runs is one kterm and the home screen can read the
    // moment karyll lets go of it. karyll itself needs nothing from this, and a
    // failure costs only that reach — see [`udev`].
    match udev::ensure() {
        Ok(udev::Outcome::Present) => {}
        Ok(udev::Outcome::Installed) => eprintln!("udev: installed {}", udev::PATH),
        Err(err) => eprintln!("udev: {err:#} — the keyboard will reach karyll only"),
    }

    // Spawned and left to come up on its own — see [`hid::Hid::poll_up`]. The
    // window is what a tap on the tile is waiting for, and it must not wait for
    // a radio.
    let mut bluetooth = hid::Hid::beside_executable()?;
    bluetooth.set_keep_alive(read_keep_bluetooth());
    if let Err(err) = bluetooth.start() {
        eprintln!("bluetooth: {err:#}");
    }

    // A keyboard is not required to open. Bluetooth takes seconds to connect
    // and may not be paired at all, and refusing to start would leave a tap on
    // the tile doing nothing visible — which is worse than a page you cannot
    // type into yet. The loop picks one up whenever it appears.
    let keyboard = match evdev::Keyboard::open() {
        Ok(keyboard) => {
            report_keyboard(&keyboard, "");
            Some(keyboard)
        }
        Err(err) => {
            eprintln!("keyboard: none yet ({err:#}) — will keep looking");
            None
        }
    };

    // Touch is what makes karyll reachable — and escapable — before a keyboard
    // exists. Without it the only way out was a key chord on a keyboard that
    // has not been paired yet.
    let touch = match touch::Touchscreen::open() {
        Ok(touch) => Some(touch),
        Err(err) => {
            eprintln!("touch: unavailable ({err:#}) — the menu will be unreachable");
            None
        }
    };

    // The pen is a second pointer, not a second input method: it places the
    // cursor between two characters, which a fingertip several millimetres
    // across cannot. Absent on a Scribe without one in the room, and nothing
    // depends on it.
    let pen = match pen::Pen::open() {
        Ok(pen) => Some(pen),
        Err(err) => {
            eprintln!("pen: unavailable ({err:#})");
            None
        }
    };

    // Opened before the window, because it is what decides which way up the
    // window opens. A missing accelerometer is not worth a word to the user:
    // the Kindle that has none offers the choice in Config instead.
    let accel = match evdev::Accelerometer::open() {
        Ok(accel) => {
            eprintln!("accel: {}", accel.path().display());
            Some(accel)
        }
        Err(err) => {
            eprintln!("accel: unavailable ({err:#})");
            None
        }
    };

    let size = read_size();
    let theme = render::Theme::at(
        size,
        read_line_length(),
        font::average_advance(&mut fonts, size),
    );

    let orientation = read_orientation(accel.as_ref());
    let mut window = window::Window::open("karyll", orientation)?;
    // Only ever narrows what the panel offered: on the two grey Kindles the
    // capability is absent and this is a no-op.
    window.set_colours(read_colours());
    window.set_colour(read_colour());
    eprintln!("window: {}x{}", window.width(), window.height());

    Editor {
        doc,
        dict: None,
        dict_region: None,
        path,
        window,
        fonts,
        theme,
        mods: Mods::default(),
        frame: None,
        roles: Vec::new(),
        goal: None,
        clipboard: String::new(),
        touch_down: None,
        last_tap: None,
        bluetooth,
        mode: Mode::Writing,
        scanning: None,
        found: Vec::new(),
        panel_page: 0,
        find: None,
        polled: None,
        holding: None,
        touch_orientation: orientation,
        turns_itself: accel.is_some(),
        orientation_checked: std::time::Instant::now(),
        focus: read_focus(),
        enabled: read_languages(),
        announcing: false,
        chrome_hidden: false,
        scroll: 0,
        keyboard_present: false,
        paired: Vec::new(),
        connected: None,
        last_edit: None,
        dirty_since: None,
        engines: Vec::new(),
        cjk: false,
        typed: String::new(),
        preedit: String::new(),
        candidates: Vec::new(),
        page: 0,
        pages: Vec::new(),
        punctuation: ime::Punctuation::default(),
        // The remembered one is applied below rather than assigned here.
        language: Language::English,
        strip_drawn: Vec::new(),
        strip_changed: false,
        status_drawn: String::new(),
        last_input: std::time::Instant::now(),
        holding_awake: false,
        landing: false,
        arming: None,
    }
    .resume_language(read_language())
    .run(keyboard, touch, pen, accel)
}

/// Wait until one of `fds` has something to read, or `timeout_ms` passes
/// (negative for no timeout), and report which are ready.
///
/// A signal interrupting the wait is not an error and not a descriptor: it
/// reports nothing ready, and the caller goes round once and sees why.
fn wait(fds: &[std::os::unix::io::RawFd], timeout_ms: i32) -> Result<Vec<bool>> {
    let mut poll: Vec<libc::pollfd> = fds
        .iter()
        .map(|fd| libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let n = unsafe { libc::poll(poll.as_mut_ptr(), poll.len() as libc::nfds_t, timeout_ms) };
    if n >= 0 {
        // **Any `revents`, not just `POLLIN`.** A Bluetooth keyboard's
        // `/dev/input/eventN` is destroyed the moment the link drops, and
        // the kernel reports that as `POLLHUP`/`POLLERR` — never as
        // readable. Testing `POLLIN` alone meant the node was never read,
        // so the read never failed, so the descriptor was never dropped and
        // the search for a replacement never restarted: **the app went deaf
        // for the rest of the session** and only a relaunch fixed it. That
        // is one bug behind every symptom of it — reconnecting from Config,
        // power-cycling the keyboard, forgetting and re-pairing, and even
        // the first pairing of a session that had held a node before.
        // Reporting the hangup lets the read fail, which is what the
        // "lost — looking for another" path was always waiting for.
        return Ok(poll.iter().map(|p| p.revents != 0).collect());
    }
    let err = std::io::Error::last_os_error();
    if err.kind() != std::io::ErrorKind::Interrupted {
        return Err(err).context("poll keyboard and display");
    }
    // **A signal is an answer.** Waiting again here would block with the flag
    // [`on_signal`] just set and unread, and the editor would go on running
    // until something else happened to it. Nothing is ready, and the caller is
    // free to look at why it woke.
    Ok(vec![false; poll.len()])
}

/// Set when the editor has been asked to stop.
///
/// **A killed editor still has work to do**: the document is only written by
/// autosave a couple of seconds after the last keystroke, the Bluetooth daemon
/// is a child that outlives it and holds both the radio and its API port, and
/// powerd is holding the screen awake at karyll's request. A launch replaces
/// the editor that is running by killing it — see the launcher — so this is the
/// ordinary way karyll ends, not an emergency.
static STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Async-signal-safe by construction: one relaxed store and nothing else.
extern "C" fn on_signal(_: libc::c_int) {
    STOPPING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Ask to be told rather than killed outright.
fn catch_signals() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe { libc::signal(signal, on_signal as *const () as libc::sighandler_t) };
    }
}

/// An input source: what Ctrl+Space and the language button move between.
///
/// One cycle rather than a layout switch and an IME switch, because they are
/// one decision to the writer — "I am writing German now" — and two controls
/// for one decision is the same trap as two lists for one thing.
///
/// This is macOS's model, which is the reference for input behaviour here: the
/// list holds keyboards *and* input methods together, and one shortcut walks it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Language {
    #[default]
    /// English. Named for the language rather than for the `US` layout it is
    /// written on — the layout is what [`Language::layout`] answers, and `US`
    /// is not a language.
    English,
    German,
    Chinese,
    /// Traditional Chinese. The same pinyin engine — there is only one Chinese
    /// dictionary on the device and it is Simplified — with the engine's own
    /// converter applied to every candidate.
    ChineseTraditional,
    /// Japanese: romaji typed, kana and kanji out. A different engine from
    /// Chinese's — Omron iWnn rather than XT9 — behind the same plugin ABI.
    Japanese,
}

impl Language {
    const ALL: [Language; 5] = [
        Language::English,
        Language::German,
        Language::Chinese,
        Language::ChineseTraditional,
        Language::Japanese,
    ];

    /// What the button says. Each names itself the way its writers would, and
    /// the CJK entries name themselves in their own script.
    fn label(self) -> &'static str {
        match self {
            Language::English => "EN",
            Language::German => "DE",
            Language::Chinese => "简体",
            Language::ChineseTraditional => "繁體",
            Language::Japanese => "日本語",
        }
    }

    fn letter(self) -> char {
        match self {
            Language::English => 'e',
            Language::German => 'd',
            Language::Chinese => 'c',
            Language::ChineseTraditional => 't',
            Language::Japanese => 'j',
        }
    }

    fn from_letter(s: &str) -> Language {
        match s.trim() {
            "d" => Language::German,
            "c" => Language::Chinese,
            "t" => Language::ChineseTraditional,
            "j" => Language::Japanese,
            _ => Language::English,
        }
    }

    /// Whether the engine should convert its candidates to Traditional.
    fn traditional(self) -> bool {
        matches!(self, Language::ChineseTraditional)
    }

    /// Which input method's rules apply, and so which engine to load. `None`
    /// for the languages typed straight onto the page.
    fn script(self) -> Option<ime::Script> {
        match self {
            Language::Chinese | Language::ChineseTraditional => Some(ime::Script::Chinese),
            Language::Japanese => Some(ime::Script::Japanese),
            Language::English | Language::German => None,
        }
    }

    /// Which regional convention the Han faces should follow while this
    /// language is selected.
    ///
    /// The Latin languages keep whatever was last set rather than forcing a
    /// convention of their own: switching to English to type a word in the
    /// middle of a Japanese paragraph must not re-cut the kanji around it.
    fn region(self) -> Option<karyll_core::script::Region> {
        match self {
            Language::Chinese => Some(karyll_core::script::Region::Simplified),
            Language::ChineseTraditional => Some(karyll_core::script::Region::Traditional),
            Language::Japanese => Some(karyll_core::script::Region::Japanese),
            Language::English | Language::German => None,
        }
    }

    /// The next input source in the cycle, among those switched on.
    ///
    /// **Cycling only the enabled ones is the whole point of enabling them.**
    /// This writer uses five; someone who writes only English should not press
    /// `Ctrl+Space` four times to arrive back where they started. The order is
    /// `ALL`'s, so turning one off closes the gap rather than reshuffling the
    /// rest.
    ///
    /// A source that is not itself enabled still cycles forward from where it
    /// sits in `ALL` — that is what makes turning off the *current* language
    /// leave somewhere sensible to go.
    fn next(self, enabled: &[Language]) -> Language {
        let from = Language::ALL.iter().position(|l| *l == self).unwrap_or(0);
        Language::ALL
            .iter()
            .cycle()
            .skip(from + 1)
            .take(Language::ALL.len())
            .find(|l| enabled.contains(l))
            .copied()
            .unwrap_or(self)
    }

    /// The keyboard arrangement this language is written on.
    ///
    /// **A language determines its layout — they are not two settings.** There
    /// is no layout control anywhere in karyll, only this: choosing German
    /// chooses QWERTZ, and a French entry would choose AZERTY.
    ///
    /// Chinese is US, and Japanese would be too. Pinyin and romaji are both
    /// defined against the QWERTY letter arrangement — that is what every IME
    /// assumes and what macOS does regardless of the hardware underneath — so
    /// they do not inherit whatever keyboard was last used for prose.
    fn layout(self) -> keymap::Layout {
        match self {
            Language::German => keymap::Layout::German,
            // Everything else is written on QWERTY: pinyin and romaji are both
            // defined against that arrangement.
            _ => keymap::Layout::Us,
        }
    }
}

/// Watches for accelerometer readings that mean nothing to us.
///
/// **Almost silent on purpose, and that took two device runs to earn.** What
/// the runs established:
///
/// * **`ABS_X`, `ABS_Y` and `ABS_Z` are never sent.** The driver advertises all
///   three and reports `0` for every one forever, so there is no gravity vector
///   and the axes on [`evdev::Sample`] have nothing to work with on this
///   firmware.
///   The position code is the entire signal.
/// * **The sensor reports transitions, not a stream.** It is quiet while the
///   device is still and emits one event when it is turned. An earlier version
///   of this warned "nothing for 10s — sensor may be powered down", which was
///   built for a streaming sensor and would have cried wolf through every
///   writing session. Silence here is the normal state.
/// * **It emits a settling burst on power-up** — the same sequence appeared at
///   the top of every session before the device had been touched. Those codes
///   are real ones, so they cannot be filtered by value; they are harmless
///   because turning to an orientation you are already in does nothing.
///
/// What is left worth saying is an *unrecognised* code, which would mean this
/// firmware encodes positions differently and the mapping in
/// [`orientation::Orientation::from_tilt`] needs redoing. Reported once per
/// distinct code, because a firmware that did that would do it continuously.
#[derive(Default)]
struct AccelWatch {
    unknown: Vec<i32>,
}

impl AccelWatch {
    fn note(&mut self, sample: evdev::Sample) {
        if orientation::Orientation::from_tilt(sample.tilt).is_some()
            || self.unknown.contains(&sample.tilt)
        {
            return;
        }
        self.unknown.push(sample.tilt);
        eprintln!(
            "accel: position code {} is not one of 15/16/17/18 — \
             this firmware may encode orientation differently",
            sample.tilt
        );
    }
}

/// What the screen is showing. The menus are modal: they take the whole
/// surface, because a finger needs room and the document has nothing useful to
/// say behind them.
enum Mode {
    Writing,
    /// The file list: every `.md` under the documents directory, with what the
    /// panel says about each. New and Rename are on the strip rather than in
    /// here — they are actions, and a list of documents that also holds two
    /// things that are not documents is the mistake the Keyboard row made.
    Files(Vec<Listing>),
    /// Typing a name. Holds what has been typed and what it is for.
    Naming {
        for_new: bool,
        name: String,
    },
    /// Settings: the keyboard, the input sources, and the faces they set in.
    ///
    /// **Pairing is the first section, not a panel behind a row.** It is the
    /// first thing a new writer has to do — there is no keyboard until they do
    /// it — so it cannot sit one tap deeper than the font list.
    Config,
    /// What the keys and the glass do.
    ///
    /// **Everything this app is good at is invisible.** The chrome hides while
    /// you write, the gestures are unannounced, and the shortcuts are only in
    /// the source — so a writer who does not already know them has an editor
    /// with three buttons on it. This page is the answer, and the reason it has
    /// a button of its own rather than only a shortcut: a help page you can
    /// reach only by knowing a shortcut is the joke it exists to fix.
    Help,
    /// The headings of the open document, in order, to jump between.
    ///
    /// **Find answers "where is that word"; this answers "take me to the third
    /// section"**, which a search cannot serve: it would need a word from the
    /// heading being looked for.
    ///
    /// Read when the panel opens, like the Files list, and for the same reason:
    /// the list is a fact about the document at the moment it was asked for.
    Outline(Vec<Section>),
}

/// One heading, as the outline lists it.
#[derive(Debug, Clone)]
struct Section {
    /// `#` through `######`, for the indent.
    level: u8,
    /// The heading with its markup taken out — see [`karyll_core::markdown::plain`].
    text: String,
    /// Where its line starts, in document characters. The jump lands here.
    at: usize,
    /// How many words are under it, up to the next heading of any level.
    words: usize,
}

/// Where typed text lands.
///
/// There is one IME, one composition and one candidate box. The only thing that
/// differs between typing into the page, into the find bar and into a filename
/// is where a finished word goes and where the half-typed one is shown — so
/// that difference is named once, here, and every path that commits text asks.
///
/// The alternative is a second copy of the compose loop per field, which is
/// "one list, not two" with the IME attached: the engine holds one composition
/// whichever field is being typed into, and two places deciding what to do with
/// it would disagree the first time one of them was changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sink {
    /// The document.
    Page,
    /// The find bar's query.
    Find,
    /// The filename being typed.
    Name,
}

struct Editor {
    doc: Document,
    /// The word list for the current regional convention, when the device has
    /// one and a convention has been chosen. Selection in Han falls back to
    /// whole runs without it.
    dict: Option<Dict>,
    dict_region: Option<Region>,
    path: Option<PathBuf>,
    window: window::Window,
    fonts: font::Fonts,
    theme: render::Theme,
    mods: Mods,
    /// What was last drawn, so a keystroke presents one line instead of the
    /// page. `None` until the first paint.
    frame: Option<render::Frame>,
    /// The faces the last paint found in this document, so page movement sizes
    /// its rows the same way the page does.
    roles: Vec<karyll_core::script::Role>,
    /// Column the arrow keys are holding across a run of vertical moves.
    goal: Option<f32>,
    /// Cut and copied text. Ours, because the device has no system clipboard
    /// and nothing to share one with — and in memory only, so it does not
    /// survive a quit. Whether it should is not yet worth guessing at.
    clipboard: String,
    /// Where a finger went down on the page. There are no motion events, so
    /// this and the lift position are the whole of a drag.
    touch_down: Option<(u16, u16)>,
    /// When and where the page was last tapped, for spotting the second tap of
    /// a pair.
    last_tap: Option<(std::time::Instant, (u16, u16))>,
    /// The Bluetooth stack. Stopped when the editor drops, so the stock stack
    /// comes back when karyll exits.
    bluetooth: hid::Hid,
    mode: Mode,
    /// When the running scan began, if one is.
    scanning: Option<std::time::Instant>,
    /// What the running scan has turned up that is not already paired. Cleared
    /// when Config opens, so a list of keyboards that were in the room an hour
    /// ago is not offered as if they still are.
    found: Vec<hid::Device>,
    /// Which page of a long panel list is showing. Reset whenever a panel
    /// opens, because arriving on page 3 of a list you have not seen is not
    /// where anybody wants to start.
    panel_page: usize,
    /// The search, while one is open.
    find: Option<Find>,
    /// When it was last asked about. The loop ticks five times a second, and
    /// asking that often opened 242 connections during one scan — enough to
    /// disturb the very thing being measured on a single-threaded daemon.
    polled: Option<std::time::Instant>,
    /// What is inverted under the finger right now, and when it was shown.
    holding: Option<(Target, std::time::Instant)>,
    /// How the panel maps onto the window *right now*.
    ///
    /// Not the same as what we asked the window manager for. The framework
    /// flips the display 180° on its own from the accelerometer, and does not
    /// tell us — after which taps arrive mirrored and nothing on screen
    /// responds. So this follows what the manager reports, while the request
    /// lives on the window.
    touch_orientation: orientation::Orientation,
    /// Whether this Kindle turns its own page over, which is to say whether it
    /// has an accelerometer. The one that does not needs [`SCREENS`] on the
    /// settings page instead, and the one that does must not have it.
    turns_itself: bool,
    /// When that was last checked, since asking costs a subprocess.
    orientation_checked: std::time::Instant,
    /// Whether the page is set back around the sentence being written.
    ///
    /// iA Writer's signature, and the reason `window::QUIET` exists at all.
    /// Remembered across sessions, like the language and the last position.
    focus: bool,
    /// Which input sources `Ctrl+Space` cycles through, in `Language::ALL`'s
    /// order. Never empty — see [`read_languages`].
    enabled: Vec<Language>,
    /// Whether the language just changed and has not been written with yet.
    ///
    /// **Cleared by the next keystroke rather than by a timer.** A timer would
    /// cost a repaint of its own for nothing; going when you type costs the
    /// repaint that was happening anyway, and by then the script on the page
    /// answers the question without being asked. The mode is only ambiguous
    /// between switching and typing, which is exactly how long this lasts.
    announcing: bool,
    /// Whether the action strip is out of the way while writing.
    ///
    /// iA Writer's chrome goes on the first keystroke and comes back when you
    /// reach for it, in both normal and focus mode — its captures show a
    /// toolbar on a freshly opened document and none at all after typing.
    ///
    /// **Never true without a keyboard.** An early device run left the app
    /// unusable and inescapable with nothing paired, which is the reason the
    /// touch UI exists at all; hiding the only way out when there is no other
    /// way in would rebuild that trap. See [`Editor::strip_visible`].
    chrome_hidden: bool,
    /// How far the page is scrolled down, in pixels.
    ///
    /// **Kept, rather than derived from the cursor each paint.** Deriving it is
    /// what made ordinary writing behave as half a focus mode: with nowhere to
    /// remember the page's position, every paint had to place it from the
    /// cursor, and the only stable way to do that is to pin the cursor. Holding
    /// the offset is what lets the caret travel down the page while the text
    /// stays where it is.
    scroll: i32,
    /// Whether a keyboard is attached right now, for the panel to report.
    keyboard_present: bool,
    /// Keyboards the daemon already knows, refreshed when the panel opens and
    /// after anything changes them. Kept here rather than fetched per tap,
    /// because working out what a finger is on must not make an HTTP request.
    paired: Vec<hid::Device>,
    /// Which of `paired` holds the link, from [`hid::Hid::connected`] and
    /// refreshed with it. `keyboard_present` says a keyboard is attached; this
    /// says which row it is.
    connected: Option<String>,
    /// When the document was last changed, and when it first went unsaved.
    /// Both drive autosave; see [`Editor::poll_autosave`].
    last_edit: Option<std::time::Instant>,
    dirty_since: Option<std::time::Instant>,
    /// Amazon's predictor plugins, each loaded the first time its language is
    /// asked for. Empty while none has been wanted, and a language that failed
    /// to load is simply absent — typing Latin must not depend on any of them.
    ///
    /// **Loaded engines are kept, not swapped.** Chinese and Japanese are
    /// separate plugins with separate dictionaries, so switching between them
    /// could unload one and load the other; but a plugin's `load()` runs the
    /// whole engine initialisation and `prv_unload` tears it down, and nothing
    /// says the pair is re-runnable within one process. Holding both costs
    /// mapped dictionary pages, which the kernel can evict; getting it wrong
    /// costs a crash mid-sentence.
    engines: Vec<(ime::Script, Box<dyn ime::Ime>)>,
    /// Whether keys are going to an engine at all. Ctrl+Space toggles it.
    cjk: bool,
    /// The keys sent towards the current word, what the engine makes of them,
    /// and what it offers for it.
    ///
    /// `typed` and `preedit` are the same string for Chinese, where pinyin *is*
    /// what was typed, and differ for Japanese, where `nihon` is typed and
    /// にほん is composed. `preedit` is what the bar shows and what Enter
    /// commits; `typed` is what F10 gives back.
    ///
    /// A candidate that converts only the front of a word rebases both onto
    /// what is left of the reading, and there the letters as struck are gone —
    /// the engine holds a remainder, not a history of how it was spelled.
    typed: String,
    preedit: String,
    candidates: Vec<String>,
    /// Which page of them is on the bar. The word wanted is often not on the
    /// first page, which is what the arrows are for while composing.
    page: usize,
    /// Where each page starts, from [`ui::candidate_pages`]. A page is as many
    /// as the panel can show rather than a fixed ten, so this is the only thing
    /// that knows which candidate a digit picks.
    pages: Vec<usize>,
    /// Which way the next quotation mark faces. Chinese quotes are directional
    /// and share one key, so the same keystroke has to alternate.
    punctuation: ime::Punctuation,
    /// What the bottom strip currently has drawn on it, so it is repainted when
    /// — and only when — it would look different. Typing damages the page above
    /// the strip, so redrawing it every keystroke would throw away the damage
    /// rectangle the page just computed.
    strip_drawn: Vec<String>,
    /// Set when karyll changed what the strip says without being asked to, so
    /// that the next tap on it is spent looking rather than pressing.
    ///
    /// **The same rule as the reveal, for the same reason**: a button that was
    /// not on screen when the finger came down must not be pressed, and one
    /// that changed its mind is no different. Pairing is where it happens — it
    /// leaves Config on its own once the keyboard is up, so a writer reaching
    /// for the `[ Done ]` they have been looking at finds `[ Exit ]` under
    /// their finger and the editor closes.
    strip_changed: bool,
    /// And what the status line beside them said, for the same reason. Kept
    /// apart from the cells because it is not one: it holds the room the
    /// buttons leave rather than a cell of its own, and nothing hit-tests it.
    status_drawn: String,
    /// When a key or a finger last arrived, and whether the screensaver is
    /// currently being held off.
    ///
    /// The pair rather than the instant alone, so the latch is written on the
    /// two transitions and not on every keystroke: setting it shells out to
    /// `lipc-set-prop` and reads the daemon's answer back.
    last_input: std::time::Instant,
    holding_awake: bool,
    /// Whether the next paint is arriving somewhere the writer chose from the
    /// outline, rather than following a cursor that walked there.
    ///
    /// One paint's worth, cleared by the paint that honours it. Held longer, the
    /// page would stay pinned to the heading while they wrote under it.
    landing: bool,
    /// The document whose Delete chip has been tapped once.
    ///
    /// **The path rather than a row number**, which is what makes a stale one
    /// harmless: the second tap is honoured only if it lands on the same
    /// document, so a list that has been paged, re-read or re-sorted underneath
    /// this cannot turn a confirmation into a deletion of something else.
    arming: Option<PathBuf>,
    /// The selected input source: which keyboard, and whether Chinese input is
    /// on. Remembered in `var/language`.
    language: Language,
}

impl Editor {
    /// Read keys and repaint until asked to quit.
    ///
    /// Waits on the keyboard and the X connection together. Blocking on the
    /// keyboard alone would leave the window unable to answer an expose, and
    /// blocking on X alone would drop keystrokes.
    ///
    /// One repaint per batch of keys rather than per key: a burst of typing on
    /// eink should cost one update, and the reader hands back everything that
    /// was ready at once.
    /// Run a writing session, and remember where it left off.
    ///
    /// The loop has several ways out — the Close button, `Ctrl+Q`, the window
    /// going away, an error — and the place in the draft has to be kept for all
    /// of them. Wrapping is what makes that true by construction; patching each
    /// `return` is how one of them gets missed. There is no `Drop` to use
    /// instead: this binary aborts on panic.
    fn run(
        mut self,
        keyboard: Option<evdev::Keyboard>,
        touch: Option<touch::Touchscreen>,
        pen: Option<pen::Pen>,
        accel: Option<evdev::Accelerometer>,
    ) -> Result<()> {
        // Held for the whole session, because the keyboard is grabbed and
        // typing therefore cannot reset the idle timer. Released below on
        // every way out, the same reason `remember_position` lives here.
        self.note_input();
        let result = self.session(keyboard, touch, pen, accel);
        // **The last thing before the window goes.** Nothing wrote the document
        // on the way out: autosave fires a couple of seconds after the writer
        // stops, so leaving inside that window — which is exactly what `Exit`
        // straight after finishing a sentence is — drops it. There is no Save
        // button to fall back on.
        //
        // Reported rather than propagated, and after the session's own result
        // is in hand: a failure here must not replace the reason the session
        // ended, and the session ending badly is all the more reason to try.
        if let Err(err) = self.write_document("closing") {
            eprintln!("closing: could not save ({err:#})");
        }
        self.remember_position();
        power::prevent_screensaver(false);
        result
    }

    fn session(
        &mut self,
        mut keyboard: Option<evdev::Keyboard>,
        mut touch: Option<touch::Touchscreen>,
        mut pen: Option<pen::Pen>,
        mut accel: Option<evdev::Accelerometer>,
    ) -> Result<()> {
        self.keyboard_present = keyboard.is_some();
        let mut accel_log = AccelWatch::default();
        // Long enough ago that the first pass through the loop looks.
        let mut looked_for_keyboard = std::time::Instant::now() - std::time::Duration::from_secs(1);
        self.paint()?;

        loop {
            // Asked to stop — see [`STOPPING`]. Out through the same door as
            // `[ Exit ]`, so the document is written, the daemon is stopped and
            // the screen is let go of.
            if STOPPING.load(std::sync::atomic::Ordering::Relaxed) {
                eprintln!("signal: asked to stop");
                return Ok(());
            }
            // Drain X first: x11rb decodes into its own buffer, so an event
            // already read leaves nothing on the socket for `poll` to report.
            match self.window.drain_events()? {
                window::Surface::Gone => return Ok(()),
                // A rotation arrives as a resize. Nothing measured against the
                // previous shape still holds, so lay the page out again from
                // nothing rather than repainting part of it.
                window::Surface::Live { resized: true, .. } => {
                    // Logged, because a rotation is the only way the surface
                    // changes shape and nothing else records the landscape
                    // geometry. Without it the only source for what the page
                    // measures sideways is counting pixels in a screenshot.
                    eprintln!("window: {}x{}", self.window.width(), self.window.height());
                    self.frame = None;
                    // The candidate bar is paged by how much width there is,
                    // and there is now a different amount. Nothing else here
                    // survives a rotation either; this is the one piece of it
                    // that is not rebuilt by the paint below.
                    let candidates = std::mem::take(&mut self.candidates);
                    self.set_candidates(candidates);
                    self.paint()?;
                }
                window::Surface::Live { expose: true, .. } => self.window.refresh()?,
                window::Surface::Live { .. } => {}
            }

            // A held finger produces no events, so waiting has to time out for
            // the long press to be noticed at all. The same tick is what looks
            // for a keyboard that has not arrived yet.
            let mut fds = vec![self.window.fd()];
            let kbd_slot = keyboard.as_ref().map(|k| {
                fds.push(k.fd());
                fds.len() - 1
            });
            let touch_slot = touch.as_ref().map(|t| {
                fds.push(t.fd());
                fds.len() - 1
            });
            let pen_slot = pen.as_ref().map(|p| {
                fds.push(p.fd());
                fds.len() - 1
            });
            let accel_slot = accel.as_ref().map(|a| {
                fds.push(a.fd());
                fds.len() - 1
            });
            let ready = wait(&fds, TICK_MS)?;

            // Log-only, and deliberately before anything can repaint: this is
            // the run that establishes what the sensor's numbers mean. It must
            // not be allowed to end the session, so a read error drops the
            // device rather than propagating.
            if let (Some(slot), Some(device)) = (accel_slot, accel.as_mut())
                && ready.get(slot).copied().unwrap_or(false)
            {
                match device.read_batch() {
                    Ok(samples) => {
                        for sample in &samples {
                            accel_log.note(*sample);
                        }
                        // Only the last one matters: turning the device through
                        // an intermediate position should not repaint at every
                        // step of the way.
                        if let Some(sample) = samples.last() {
                            self.follow_device(sample.tilt)?;
                        }
                    }
                    Err(err) => {
                        eprintln!("accel: {err:#} — no longer reading it");
                        accel = None;
                    }
                }
            }

            // A running scan reports itself here rather than in a blocking
            // sleep, so the panel keeps repainting and taps are not queued
            // behind it.
            self.poll_scan()?;
            self.poll_bluetooth()?;
            self.poll_orientation();
            self.poll_autosave();
            self.poll_sleep();

            // A read that fails is not shrugged off any more. `wait` now
            // reports a hangup as ready, and a descriptor that is ready and
            // keeps failing is a tick that never blocks — the loop would spin
            // at full speed on a dead touchscreen. Nothing on this device
            // removes the panel's own node, but a spin is not a thing to leave
            // reachable.
            let mut lost_touch = false;
            if let (Some(slot), Some(device)) = (touch_slot, touch.as_mut())
                && ready.get(slot).copied().unwrap_or(false)
            {
                let extent = (device.x_extent, device.y_extent);
                // A finger resets the framework's own idle timer, so this is
                // not what keeps the page up — it is what takes the latch back
                // after an idle spell, so the typing that follows is covered.
                self.note_input();
                let taps = match device.read_batch() {
                    Ok(taps) => taps,
                    Err(err) => {
                        eprintln!("touch: lost ({err:#}) — no longer reading it");
                        lost_touch = true;
                        Vec::new()
                    }
                };
                // Read either way — a descriptor left ready is a tick that
                // never blocks — but only acted on while the editor is the
                // thing under the finger. See [`window::Window::buried`].
                if !self.window.buried() && self.contacts(taps, extent)? {
                    return Ok(());
                }
            }
            if lost_touch {
                touch = None;
            }

            // **The pen is a finger.** Same contacts, same handler, same
            // meaning for a tap and a drag — the only difference is which
            // device reported it and how finely it counts.
            let mut lost_pen = false;
            if let (Some(slot), Some(device)) = (pen_slot, pen.as_mut())
                && ready.get(slot).copied().unwrap_or(false)
            {
                let extent = (device.x_extent, device.y_extent);
                let taps = match device.read_batch() {
                    Ok(taps) => taps,
                    Err(err) => {
                        eprintln!("pen: lost ({err:#}) — no longer reading it");
                        lost_pen = true;
                        Vec::new()
                    }
                };
                // Only for a nib that touched down. A pen hovering over the page
                // reports its position continuously, and treating that as the
                // writer being present would hold the screensaver off for as
                // long as one sat in a hand.
                if !taps.is_empty() {
                    self.note_input();
                }
                if !self.window.buried() && self.contacts(taps, extent)? {
                    return Ok(());
                }
            }
            if lost_pen {
                pen = None;
            }

            if keyboard.is_none() {
                // **On a tick, not on every wake.** This reads
                // `/proc/bus/input/devices` and tries to open a node, and the
                // loop no longer wakes only on its own timeout: a pen hovering
                // over the page reports its position a hundred times a second,
                // which would turn a keyboard that is not there into a hundred
                // fruitless scans of eMMC a second.
                if looked_for_keyboard.elapsed() >= std::time::Duration::from_millis(TICK_MS as u64)
                {
                    looked_for_keyboard = std::time::Instant::now();
                    if let Ok(found) = evdev::Keyboard::open() {
                        report_keyboard(&found, " (appeared)");
                        keyboard = Some(found);
                        self.keyboard_present = true;
                        // Which of the remembered keyboards this node belongs
                        // to. The node is all evdev knows; the daemon names it,
                        // and the Keyboard section marks that row and no other.
                        self.connected = self.bluetooth.connected();
                        // Config stays open rather than being dismissed:
                        // pairing succeeded, and its Keyboard section is where
                        // that shows.
                        self.paint()?;
                    }
                }
                continue;
            }

            let Some(slot) = kbd_slot else { continue };
            if !ready.get(slot).copied().unwrap_or(false) {
                continue;
            }
            let Some(kbd) = &mut keyboard else { continue };
            let batch = match kbd.read_batch() {
                Ok(batch) => batch,
                Err(err) => {
                    // The node goes away when the keyboard disconnects or the
                    // daemon stops. Keep the document on screen and start
                    // looking again rather than dying mid-sentence.
                    eprintln!("keyboard: lost ({err:#}) — looking for another");
                    keyboard = None;
                    self.keyboard_present = false;
                    // Say so. Config draws this keyboard's state from the flag
                    // just cleared, and a page left reading `Disconnect` for a
                    // keyboard that has gone is the panel telling the writer
                    // the opposite of what happened. Losing the keyboard is
                    // otherwise silent.
                    self.paint()?;
                    continue;
                }
            };

            // While naming, keys build the name. In any other panel they do
            // nothing — that is a finger's screen.
            if matches!(self.mode, Mode::Naming { .. }) {
                // CJK gets first refusal here too: a writer who works in
                // Chinese should be able to say what a document is called in
                // Chinese, and every letter reaches the engine while the mode
                // is on. See [`Sink`].
                let mut dirty = false;
                for event in batch {
                    let Some(action) = self.pressed_action(&event) else {
                        continue;
                    };
                    if self.compose_key(&action) {
                        dirty = true;
                        continue;
                    }
                    // Settled — the name is taken or abandoned, and whatever
                    // that moved to has painted itself.
                    if self.typed_name(&action)? {
                        dirty = false;
                        break;
                    }
                }
                if dirty {
                    self.paint()?;
                }
                continue;
            }
            // The other panels take a few keys, because each has a shortcut
            // that opens it: a screen reachable from the keyboard and escapable
            // only by putting a hand on the glass is a trap. Esc is the way out
            // of each, the arrows page, and a shortcut for another panel goes
            // straight there.
            if !matches!(self.mode, Mode::Writing) {
                for event in batch {
                    let Some(action) = self.pressed_action(&event) else {
                        continue;
                    };
                    match action {
                        Action::Quit => return Ok(()),
                        Action::Escape => {
                            self.leave_panel()?;
                            break;
                        }
                        // **The arrows, and not only `PageUp`/`PageDown`.** The
                        // keyboard this is used with is a compact one with no
                        // page keys on it at all, so binding those alone left
                        // the strip saying `Previous` and `Next` with no way to
                        // reach either from the keys. Left and right, because
                        // turning a page is what they do on this device.
                        Action::Left | Action::PageUp if self.pages() > 1 => {
                            self.turn_page(true)?;
                        }
                        Action::Right | Action::PageDown if self.pages() > 1 => {
                            self.turn_page(false)?;
                        }
                        Action::Files
                        | Action::Config
                        | Action::Help
                        | Action::Outline
                        | Action::NewDocument
                        | Action::Refresh => self.apply(action)?,
                        _ => {}
                    }
                }
                continue;
            }

            // The find bar takes the keyboard while it is open. It is not a
            // mode — the document is still on screen and still being scrolled
            // to the hits — but the keys build the query rather than the draft.
            if self.find.is_some() {
                // CJK gets first refusal here exactly as it does on the page,
                // and by the same call: without it the bar takes pinyin letters
                // literally, and a document written in Chinese cannot be
                // searched at all. See [`Sink`].
                let mut dirty = false;
                for event in batch {
                    let Some(action) = self.pressed_action(&event) else {
                        continue;
                    };
                    if self.compose_key(&action) {
                        dirty = true;
                        continue;
                    }
                    // The bar has closed, and closing it painted.
                    if self.typed_query(&action)? {
                        dirty = false;
                        break;
                    }
                }
                if dirty {
                    self.paint()?;
                }
                continue;
            }

            let mut dirty = false;
            for event in batch {
                let Some(action) = self.pressed_action(&event) else {
                    continue;
                };
                if matches!(action, Action::Quit) {
                    return Ok(());
                }
                // Writing puts the chrome away, which is iA Writer's rhythm in
                // both modes: a freshly opened document has a toolbar and a
                // document being typed into has none.
                self.set_chrome_hidden(true);
                // Chinese input gets first refusal. It only takes keys it has a
                // use for, so English typing is untouched even while the engine
                // is switched on.
                if self.compose_key(&action) {
                    dirty = true;
                    continue;
                }
                self.apply(action)?;
                dirty = true;
            }
            if dirty {
                self.paint()?;
            }
        }
    }

    /// What a key event means, or `None` for a release, a modifier, or a key
    /// that is bound to nothing.
    ///
    /// **One place**, because four surfaces read the keyboard now — the page,
    /// the find bar, the name prompt and the panels — and every one of them has
    /// to track the modifier state, ignore releases and resolve against the
    /// selected layout in exactly the same way. The find bar first read raw
    /// codes instead, which is why it could not be given CJK input: the IME
    /// speaks [`keymap::Action`], and a second reading of the keyboard had
    /// nothing to hand it.
    fn pressed_action(&mut self, event: &evdev::KeyEvent) -> Option<Action> {
        if self.mods.track(event.code, event.pressed) || !event.pressed {
            return None;
        }
        // The writer is here. Every key comes through this, which is what makes
        // it the place to say so — and a keystroke is the one kind of input the
        // framework's own idle timer cannot see, because the keyboard is
        // grabbed.
        self.note_input();
        let Some(action) = keymap::action(event.code, self.mods, self.language.layout()) else {
            // Named, because a key that does nothing is otherwise
            // indistinguishable from one that never arrived. Compact keyboards
            // have no `Home`, and whether their `fn`+← reaches us as code 102
            // or as something else depends on the kernel's Apple quirk driver —
            // one press and this line settles it.
            eprintln!("key: {} unbound ({:?})", event.code, self.mods);
            return None;
        };
        // The language notice goes with the next keystroke: anything that is
        // not another switch means the writer has read it and moved on. Here
        // rather than in the writing loop because the panels take keys too, and
        // a notice raised on one of those has to be cleared by the same path
        // that raised it.
        self.announcing = matches!(action, Action::CycleLanguage);
        Some(action)
    }

    /// The strip cells for the current mode, left to right.
    ///
    /// `&mut self` for one reason: whether there is a `More` to offer depends
    /// on how many rows fit, which is measured from the face in use.
    fn strip_wanted(&mut self) -> Vec<Bar> {
        // The find bar takes the strip rather than sitting above it, for the
        // reason the candidate bar does: the strip's cells are already the
        // right size for a finger, and a second band would push the page up
        // and reflow it on every letter typed into the search.
        if let Some(find) = &self.find {
            // **Both fields at once while replacing.** Changing `colour` to
            // `color` is a comparison of two nearly identical strings, and one
            // field at a time cannot be compared.
            return if find.replacing {
                vec![
                    Bar::Query,
                    Bar::With,
                    Bar::Count,
                    Bar::Previous,
                    Bar::Next,
                    Bar::Change,
                    Bar::All,
                    Bar::Done,
                ]
            } else {
                vec![
                    Bar::Query,
                    Bar::Count,
                    Bar::Previous,
                    Bar::Next,
                    Bar::Replace,
                    Bar::Done,
                ]
            };
        }
        let mut cells = match self.mode {
            // **Three, in the left corner**, matching `sidle/native` and `steb`
            // — a convention shared with the other apps on this device rather
            // than an order invented here.
            //
            // Save came off with the status line that replaced it: a button
            // that duplicates an autosave, next to nothing saying the autosave
            // ran, was a redundant control paid for with an act of faith.
            // `Ctrl`/`⌘`+`S` still works, because the habit costs nothing.
            //
            // The language button came off with UI-17's notice at the caret,
            // which names the source you land in for one keystroke. Without a
            // keyboard there is nothing to switch the language *of*.
            //
            // After the corner, the order is how often a writer reaches for it:
            // Files is daily, Config is occasional, Help is read once.
            //
            // **Outline is a shortcut only.** karyll is keyboard-first — without
            // one there is no way to enter text at all — so a control that earns
            // its width on the strip has to be one a finger genuinely reaches
            // for, and jumping between the headings of a long document is not
            // that. `Ctrl`/`⌘`+`Shift`+`O` is the way to it, and the Help page
            // says so. **Help has a button at all because it must** — it is the
            // page that explains the shortcuts, and reaching it only by shortcut
            // would be the same joke it exists to answer.
            Mode::Writing => vec![Bar::Exit, Bar::Files, Bar::Config, Bar::Help],
            Mode::Naming { .. } => vec![Bar::Cancel],
            // The Files panel's own actions, on the strip rather than mixed
            // into the list of documents they act on.
            Mode::Files(_) => vec![Bar::Done, Bar::New, Bar::Rename],
            Mode::Config | Mode::Help | Mode::Outline(_) => vec![Bar::Done],
        };
        // Only when there is somewhere to go. Both directions wrap, so neither
        // is ever a button that does nothing — and because both are always
        // there once they are there at all, the strip never changes width
        // under a finger that is paging through a list.
        if self.pages() > 1 {
            cells.extend([Bar::PageBack, Bar::PageAt, Bar::PageOn]);
        }
        cells
    }

    /// The strip's cells and their words together, cut to the panel it is on.
    ///
    /// **The cells and the words are one answer, so they are worked out in one
    /// place.** Three consumers ask — drawing, hit-testing and press feedback —
    /// and two of them disagreeing is how a tap lands on the wrong cell.
    ///
    /// The strip was laid out on a 10.2″ panel and the smaller Kindles are two
    /// thirds of that width, where the find bar wants more than there is. What
    /// it gives up, in order: its longer words, and then its readouts, which
    /// are the only cells that are not controls. **A control is never
    /// dropped** — which is exactly what [`ui::cell_bounds`] does when it runs
    /// out of room, and what it drops is the tail, so on a 7″ panel the first
    /// thing to go would be `[ Done ]`.
    fn strip_fitted(&mut self) -> (Vec<Bar>, Vec<String>) {
        let wanted = self.strip_wanted();
        let readouts = self.readouts();
        let width = self.window.width();
        let fonts = &mut self.fonts;
        let (bars, labels) = fit_strip(width, wanted, &readouts, |s| {
            ui::measure(fonts, s, ui::TEXT_PX) as u16
        });
        self.with_fields(bars, labels)
    }

    /// What the editor knows that the strip has to say.
    ///
    /// Gathered once so that the fitting below is free of the editor and can be
    /// tested against a stub metric, which is the only place it ever runs
    /// outside a device.
    fn readouts(&mut self) -> Readouts {
        Readouts {
            // The two numbers, not a copy of the search. `hits` is one range
            // per occurrence, so cloning it to read a length would copy
            // thousands of them off a common word — on every paint, which is
            // every keystroke. Composing *into the query*, which is the only
            // field whose half-typed word puts the count out of step with what
            // is beside it.
            composing: self.composing()
                && self.find.as_ref().is_some_and(|f| f.field == Field::Query),
            count: self
                .find
                .as_ref()
                .map(|f| (f.query.is_empty(), f.at, f.hits.len())),
            armed: self.find.as_ref().is_some_and(|f| f.arming_all),
            page: (self.panel_page + 1, self.pages()),
        }
    }

    /// Write what has been typed into the fields, trimmed to the room the other
    /// cells leave them.
    fn with_fields(&mut self, bars: Vec<Bar>, mut labels: Vec<String>) -> (Vec<Bar>, Vec<String>) {
        let fields = stretch_cells(&bars);
        if !fields.is_empty() {
            let others: Vec<String> = labels
                .iter()
                .enumerate()
                .filter(|(i, _)| !fields.contains(i))
                .map(|(_, label)| bracket(label))
                .collect();
            let width = self.window.width();
            let fonts = &mut self.fonts;
            let room = ui::stretch_room(width, &others, fields.len(), |s| {
                ui::measure(fonts, s, ui::TEXT_PX) as u16
            });
            // Against the cell's own `Bar`, so the field a label is written for
            // is the field that cell *is* — a position would be a second
            // statement of the bar's order.
            for cell in fields {
                if let Some(which) = Field::of(bars[cell]) {
                    labels[cell] = self.find_field(which, room);
                }
            }
        }
        (bars, labels)
    }

    /// The strip's labels, for drawing.
    ///
    /// Only the find bar's change: the two fields say what has been typed, the
    /// count says what was found, and `All` says which tap it is on. Everything
    /// else is a fixed word.
    fn strip_labels(&mut self) -> Vec<String> {
        self.strip_fitted().1
    }

    /// Which strip cells take whatever width the others leave.
    ///
    /// Only the find bar has them, and it has to: a field grows as it is typed
    /// into, so packing one like a label would shove `Previous`, `Next` and
    /// `Done` along under the writer's finger and eventually push them off the
    /// end of the strip. The replace bar has two, sharing the slack equally.
    fn strip_stretch(&mut self) -> Vec<usize> {
        stretch_cells(&self.strip_fitted().0)
    }

    /// The status line: what this document is, how long it is, and whether it
    /// is on disk.
    ///
    /// **Because autosave is otherwise invisible.** It exists so a fault in
    /// Amazon's predictor plugin cannot take unsaved prose with it, and until
    /// this line the writer had to take that on trust — the same objection this
    /// project already raised about an unnamed input mode.
    ///
    /// Empty for a panel and for the find bar: a panel says what it is in its
    /// own title, and the find bar has taken the room.
    fn status_line(&mut self) -> String {
        if !matches!(self.mode, Mode::Writing) || self.find.is_some() {
            return String::new();
        }
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_string());
        let words = karyll_core::words::describe(karyll_core::words::count(&self.doc.chars()));
        // Said in the present tense either way, because "unsaved" reads as a
        // warning and this is not one: a document is written out a couple of
        // seconds after the writer stops, and the honest report in between is
        // that there is something still to write rather than that anything is
        // at risk.
        let saved = if self.doc.is_dirty() {
            "not yet saved"
        } else {
            "saved"
        };
        format!("{name}  ·  {words}  ·  {saved}")
    }

    /// What the bottom strip says right now.
    ///
    /// Drawing, hit-testing and press feedback all ask here. Two of them
    /// disagreeing is how a tap lands on the wrong cell, which is the exact
    /// shape of bug recorded under "One list, not two".
    ///
    fn strip_cells(&mut self) -> Vec<String> {
        self.strip_labels().iter().map(|l| bracket(l)).collect()
    }

    /// What one of the find bar's fields says, trimmed to what its cell can
    /// hold.
    ///
    /// **The bar is the field**: what has been typed is on it, with a rule for
    /// a caret so it reads as somewhere text goes rather than as a label that
    /// happens to say a word.
    ///
    /// **The caret is on the field the keys are going into, and only that one.**
    /// With two fields on the strip it is the whole of what says which is
    /// listening.
    ///
    /// The composition is on the end of it, ahead of the caret, exactly where
    /// it sits when the same word is typed into the page. It is shown but not
    /// searched — see [`Editor::research`].
    ///
    /// Trimmed from the *left* into `room`, and measured rather than counted.
    /// A character count cannot say when a query has outgrown its cell, because
    /// 二十四文字 is twice the width of 24 letters. The tail is what is kept:
    /// that is where the writer is typing.
    fn find_field(&mut self, which: Field, room: u16) -> String {
        let Some(find) = &self.find else {
            return String::new();
        };
        let focused = find.field == which;
        let (name, typed) = match which {
            Field::Query => ("Find", &find.query),
            Field::With => ("With", &find.with),
        };
        // The composition belongs to whichever field is taking keys.
        let query = if focused {
            format!("{typed}{}", self.preedit)
        } else {
            typed.clone()
        };
        let caret = if focused { "_" } else { "" };
        let name = name.to_string();
        let mut chars: Vec<char> = query.chars().collect();
        let mut trimmed = false;
        loop {
            let shown: String = chars.iter().collect();
            let text = if trimmed {
                format!("{name}: …{shown}{caret}")
            } else {
                format!("{name}: {shown}{caret}")
            };
            // Measured as it will be drawn, brackets included — they are part
            // of what has to fit.
            if chars.is_empty()
                || ui::measure(&mut self.fonts, &bracket(&text), ui::TEXT_PX) as u16 <= room
            {
                return text;
            }
            chars.remove(0);
            trimmed = true;
        }
    }

    /// The panel's geometry, sized from the Latin face alone.
    ///
    /// Deliberately not from the document's faces: rows are already `lh * 2`
    /// with a 96 px floor, so there is room to spare for a Han label, and
    /// asking about the Han faces here would load 10 MB of them to open the
    /// Files panel in an English session. Each label's own baseline still comes
    /// from the faces it draws — that is `ui::draw_line`'s business.
    fn layout(&mut self) -> ui::Layout {
        let text = self.fonts.line_height(ui::TEXT_PX, font::LATIN_ROW) as u16;
        let title = self.fonts.line_height(ui::TITLE_PX, font::LATIN_ROW) as u16;
        ui::Layout::compute(text, title, self.window.height())
    }

    /// Raw panel coordinates in, window coordinates out.
    ///
    /// Split out from `target` because a tap on the page is answered by a
    /// character index rather than by a control, and both need this first.
    fn point(
        &mut self,
        raw_x: i32,
        raw_y: i32,
        extent: (touch::Extent, touch::Extent),
    ) -> (u16, u16) {
        let size = (self.window.width(), self.window.height());
        // Scale into the **panel's** own pixel space, which is always portrait,
        // before rotating into the window. Scaling against the window instead
        // stretched one axis and squashed the other whenever the two differed:
        // in landscape a tap aimed at the third button of five landed on the
        // second, because its axis had been compressed by 1860/2480.
        let panel = (size.0.min(size.1), size.0.max(size.1));
        let (x, y) = self.touch_orientation.apply(
            extent.0.to_pixels(raw_x, panel.0),
            extent.1.to_pixels(raw_y, panel.1),
            size,
        );
        (
            x.clamp(0, size.0 as i32) as u16,
            y.clamp(0, size.1 as i32) as u16,
        )
    }

    /// Which control a window point lands on.
    fn target(&mut self, x: u16, y: u16) -> Option<Target> {
        let size = (self.window.width(), self.window.height());
        let layout = self.layout();
        // The bottom strip outranks the list: it spans the full width. With
        // the chrome hidden `page_bottom` is the foot of the panel, so nothing
        // is ever on a strip that is not drawn.
        let bottom = self.page_bottom();
        let hit = if y >= bottom && y >= layout.strip_top {
            let cells = self.strip_cells();
            let stretch = self.strip_stretch();
            let fonts = &mut self.fonts;
            let bounds = ui::cell_bounds(size.0, &cells, &stretch, |s| {
                ui::measure(fonts, s, ui::TEXT_PX) as u16
            });
            // `None` past the last cell. The buttons are packed at their own
            // width now, so most of this band is the status line's — and a tap
            // on a line that only reports must not run the button nearest it.
            ui::cell_at(&bounds, x).map(Target::Strip)
        } else {
            let items = self.visible_items();
            let fonts = &mut self.fonts;
            ui::hit(&items, layout, size.0, x, y, |s| {
                ui::measure(fonts, s, ui::TEXT_PX) as u16
            })
            .map(|hit| match hit {
                ui::Hit::Row(row) => Target::Row(row),
                ui::Hit::Option(item, option) => Target::Option(item, option),
            })
        };
        eprintln!("touch: ({x},{y}) {:?} -> {hit:?}", self.touch_orientation);
        hit
    }

    /// Whether a window y is on the page rather than on the chrome.
    ///
    /// Only while writing: with a panel open the same pixels are a list of
    /// files or keyboards, and those rows outrank the document behind them.
    fn writing_area(&mut self, y: u16) -> bool {
        matches!(self.mode, Mode::Writing) && y < self.page_bottom()
    }

    /// Which candidate a point falls on, if the box is on screen and the point
    /// is inside it.
    ///
    /// Measured against the box the last paint actually drew, the way the tap
    /// test already works for the strip — recomputing the geometry here is how
    /// the two copies drift apart.
    fn candidate_at(&mut self, x: u16, y: u16) -> Option<usize> {
        let rect = self.frame.as_ref()?.candidate_box()?;
        if x < rect.x || x >= rect.x + rect.width || y < rect.y || y >= rect.y + rect.height {
            return None;
        }
        // The cells the box was drawn from, by the same function that drew
        // them. Measuring something other than what is on screen is how a tap
        // lands on the wrong one.
        let labels =
            ui::Overlay::Candidates(candidate_page(&self.candidates, &self.pages, self.page))
                .labels();
        let cells = ui::overlay_cells(&mut self.fonts, rect, self.theme.body_px, &labels);
        // `None` past the last cell: inside the box but beyond the choices
        // belongs to nothing.
        ui::cell_at(&cells, x)
    }

    /// Whether the action strip is on screen.
    ///
    /// One thing overrides the hidden flag, and it is safety rather than taste:
    /// **without a keyboard the strip is the only way out of the app**, and an
    /// early device run that left it unreachable cost a hard reset.
    ///
    /// Composing does not override it: the candidate box is drawn against the
    /// text being composed, so a Chinese word does not drag the chrome back.
    fn strip_visible(&self) -> bool {
        strip_visible(
            self.chrome_hidden,
            self.keyboard_present,
            self.find.is_some(),
            matches!(self.mode, Mode::Writing),
        )
    }

    /// Where the page ends: above the strip, or the foot of the panel when the
    /// strip is out of the way.
    fn page_bottom(&mut self) -> u16 {
        if self.strip_visible() {
            self.layout().strip_top
        } else {
            self.window.height()
        }
    }

    /// Put the chrome away, or bring it back, repainting if that changed
    /// anything.
    ///
    /// The page grows and shrinks with it, so this cannot be a damage
    /// rectangle — every row moves.
    fn set_chrome_hidden(&mut self, hidden: bool) {
        if self.chrome_hidden == hidden || !matches!(self.mode, Mode::Writing) {
            return;
        }
        let was = self.strip_visible();
        self.chrome_hidden = hidden;
        if was != self.strip_visible() {
            self.frame = None;
        }
    }

    /// The character a window point is nearest, against the frame on screen.
    fn index_at_point(&mut self, x: u16, y: u16) -> Option<usize> {
        let frame = self.frame.take()?;
        // The buffer the frame was laid out from, so the glyph under the
        // finger is the one that was drawn there.
        let (chars, preedit) = self.display();
        let markup = karyll_core::markdown::analyze(&chars);
        let bottom = self.page_bottom();
        let page = render::Page::new(
            &chars,
            &markup,
            &self.theme,
            (self.window.width(), self.window.height()),
            bottom,
        )
        .focused_on(self.focus_span(&chars))
        .composing(preedit);
        let index = render::index_at_point(&page, &mut self.fonts, &frame, x as f32, y as f32);
        self.frame = Some(frame);
        // Back to document space: the caller is going to move the cursor with
        // it, and the cursor lives in the document.
        index.map(|i| self.document_index(i))
    }

    /// What a finger — or the nib — lifting off the page means.
    ///
    /// **A contact is only Down and Up**, each carrying a position. The
    /// touchscreen reports nothing between them, and [`crate::pen`] deliberately
    /// reports nothing either. That suits the panel: pressing at one place and
    /// lifting at another *is* a drag, and resolving it on the lift paints the
    /// selection once instead of on every motion event. The cost is no live
    /// feedback under the finger, which is the right trade at ten milliseconds
    /// a refresh.
    fn tap_text(&mut self, x: u16, y: u16) -> Result<()> {
        let down = self.touch_down.take();
        let far = |a: (u16, u16), b: (u16, u16)| {
            a.0.abs_diff(b.0) > TOUCH_SLOP || a.1.abs_diff(b.1) > TOUCH_SLOP
        };
        let dragged = down.is_some_and(|d| far(d, (x, y)));

        // **A tap in a margin moves the page.** Asked before the character
        // under it, because a margin holds no character worth putting a cursor
        // in — and a drag is exempt, so selecting a run that ends past the last
        // word of a line still selects.
        if !dragged && let Some(edge) = self.page_edge(x, y) {
            self.last_tap = None;
            return self.go(edge);
        }

        let Some(index) = self.index_at_point(x, y) else {
            return Ok(());
        };

        // A drag: the two ends are where the finger went down and came up.
        if let Some(from) = down.filter(|&d| far(d, (x, y)))
            && let Some(start) = self.index_at_point(from.0, from.1)
        {
            self.doc.select(start.min(index)..start.max(index));
            self.last_tap = None;
            return self.paint();
        }

        // Shift+tap extends, which needs no new gesture: the keyboard is in
        // front of the writer anyway and shift-click is the habit already.
        if self.mods.shift {
            self.doc.extend_to(index);
            self.last_tap = None;
            return self.paint();
        }

        // A second tap in the same place selects the word under it. Both the
        // interval and the distance have to match, or a quick tap somewhere
        // else would count as the second half of a double-tap.
        let again = self
            .last_tap
            .is_some_and(|(when, at)| when.elapsed() < DOUBLE_TAP && !far(at, (x, y)));
        if again {
            self.doc.select_word_at(index, self.dict.as_ref());
            self.last_tap = None;
        } else {
            self.doc.set_cursor(index);
            self.last_tap = Some((std::time::Instant::now(), (x, y)));
        }
        self.paint()
    }

    /// Which margin a point is in, if any.
    fn page_edge(&mut self, x: u16, y: u16) -> Option<render::Edge> {
        let bottom = self.page_bottom();
        render::edge_at(&self.theme, self.window.width(), bottom, x, y)
    }

    /// Move the page, the way a tap on a margin asks.
    ///
    /// **The four keys the margins stand in for**, and not a second set of
    /// movements beside them: a finger and `PageUp` have to leave the document
    /// in the same place, or the two would drift the first time either was
    /// adjusted.
    fn go(&mut self, edge: render::Edge) -> Result<()> {
        self.apply(match edge {
            render::Edge::Back => Action::PageUp,
            render::Edge::On => Action::PageDown,
            render::Edge::Start => Action::DocStart,
            render::Edge::End => Action::DocEnd,
        })?;
        self.paint()
    }

    /// Run a batch of contacts through the editor, reporting whether one of
    /// them asked to leave.
    ///
    /// **One handler for the glass, whatever touched it.** The finger panel and
    /// the pen speak different evdev protocols and report in different
    /// coordinate spaces, and both arrive here as the same three contacts with
    /// their own extents — so a tap means the same thing, a drag selects the
    /// same way, and neither device can grow a behaviour the other lacks.
    fn contacts(
        &mut self,
        taps: Vec<touch::Touch>,
        extent: (touch::Extent, touch::Extent),
    ) -> Result<bool> {
        for tap in taps {
            match tap {
                touch::Touch::Down { x, y } => self.pressed(x, y, extent)?,
                touch::Touch::Up { x, y } => {
                    // Restore first, and synchronously, so the button is
                    // visibly released before whatever it does repaints over it.
                    self.release()?;
                    if self.tapped(x, y, extent)? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Invert whatever the finger landed on, and show it immediately.
    fn pressed(
        &mut self,
        raw_x: i32,
        raw_y: i32,
        extent: (touch::Extent, touch::Extent),
    ) -> Result<()> {
        let (x, y) = self.point(raw_x, raw_y, extent);
        // A finger on the page is remembered rather than drawn: where it lands
        // only means something once it lifts, and inverting a row of prose
        // under it would be feedback for a control that is not there.
        if self.writing_area(y) {
            self.touch_down = Some((x, y));
            return Ok(());
        }
        self.touch_down = None;
        let Some(target) = self.target(x, y) else {
            return Ok(());
        };
        self.draw_target(target, true)?;
        // Timed from after the server has it, so the hold is a hold on screen
        // rather than on a queued request.
        self.holding = Some((target, std::time::Instant::now()));
        Ok(())
    }

    /// Put back whatever was inverted.
    ///
    /// A quick tap arrives as Down and Up in the same read, so without holding
    /// the inverted state briefly it is drawn and undone between two panel
    /// updates and never becomes visible — which looks exactly like the
    /// feedback running one key behind.
    fn release(&mut self) -> Result<()> {
        let Some((target, shown)) = self.holding.take() else {
            return Ok(());
        };
        if let Some(remaining) = FEEDBACK.checked_sub(shown.elapsed()) {
            std::thread::sleep(remaining);
        }
        self.draw_target(target, false)
    }

    fn draw_target(&mut self, target: Target, pressed: bool) -> Result<()> {
        let layout = self.layout();
        let rect = match target {
            Target::Strip(index) => {
                let cells = self.strip_cells();
                let stretch = self.strip_stretch();
                ui::paint_strip_cell(
                    &mut self.window,
                    &mut self.fonts,
                    layout,
                    &cells,
                    index,
                    pressed,
                    &stretch,
                )
            }
            Target::Row(index) => {
                // Rows only exist in a panel; in the document there is nothing
                // under the finger to acknowledge.
                if matches!(self.mode, Mode::Writing) {
                    return Ok(());
                }
                let items = self.visible_items();
                ui::paint_row(
                    &mut self.window,
                    &mut self.fonts,
                    layout,
                    &items,
                    index,
                    pressed,
                )
            }
            Target::Option(item, option) => {
                if matches!(self.mode, Mode::Writing) {
                    return Ok(());
                }
                let items = self.visible_items();
                ui::paint_chip(
                    &mut self.window,
                    &mut self.fonts,
                    layout,
                    &items,
                    item,
                    option,
                    pressed,
                )
            }
        };
        // Synchronous: an invert that is only queued gets merged with the
        // restore that follows it, and neither is ever seen.
        self.window.present_sync(rect)
    }

    /// Handle a tap, in raw panel coordinates. True when the app should close.
    fn tapped(
        &mut self,
        raw_x: i32,
        raw_y: i32,
        extent: (touch::Extent, touch::Extent),
    ) -> Result<bool> {
        // Resolved by the **same** function that decided what to invert. A
        // second copy of the mapping puts the invert on one control and runs
        // another: after a 180° flip the right button lights and the wrong one
        // fires, and in landscape a tap highlights a button and triggers its
        // neighbour.
        let (x, y) = self.point(raw_x, raw_y, extent);
        // Any touch brings the chrome back. iA Writer reveals on mouse
        // movement; the nearest thing a touchscreen has is a finger arriving,
        // so the reveal is the whole glass rather than a band along the bottom.
        let waking = !self.strip_visible();
        self.set_chrome_hidden(false);
        // **A tap that lands where the strip is about to appear reveals it and
        // stops there.** It must not press a button that was not on screen when
        // the finger came down: with the chrome away those rows are blank page,
        // and tapping blank page ran Save — and would as easily have run Close.
        // Elsewhere the tap still does its job, so placing the cursor does not
        // cost two taps.
        //
        // A strip that changed its own mind is the same case — see
        // [`Editor::strip_changed`] — and is spent here for the same reason.
        // Cleared by a tap anywhere, because a tap is the writer having looked
        // at the screen since it changed.
        let changed = std::mem::take(&mut self.strip_changed);
        if (waking || changed) && y >= self.layout().strip_top {
            self.touch_down = None;
            return self.paint().map(|()| false);
        }
        // The candidate box floats over the page, so it is asked before the
        // page is: a tap on a candidate is choosing it, not moving the cursor
        // to whatever prose the box happens to be covering.
        if let Some(n) = self.candidate_at(x, y) {
            self.touch_down = None;
            self.select_candidate(n);
            self.paint()?;
            return Ok(false);
        }
        // **Anything else ends the word under way**, which is the rule
        // [`ime::compose`] already applies to an arrow or a chord, for the
        // reason recorded there: leaving a half-typed word pending while the
        // writer goes somewhere else is worse than costing them a keystroke.
        //
        // One place, and it has to be: a composition belongs to the field it
        // was started in, and a tap is the only way to leave a field with one
        // still held — every keyboard route out of a composition is consumed by
        // the engine first. Left held, [`Editor::page_preedit`] splices the
        // half-typed word into the prose at the cursor.
        self.abandon_composition();
        if self.writing_area(y) {
            self.tap_text(x, y)?;
            // A tap that resolved to no character repaints nothing, which
            // would leave a revealed strip undrawn. The dropped frame is what
            // says the reveal has not been honoured yet.
            if self.frame.is_none() {
                self.paint()?;
            }
            return Ok(false);
        }
        // A finger that went down on the page and lifted on the strip: the
        // press is spent, and leaving it set would make the *next* tap on the
        // page look like a drag from wherever that one started.
        self.touch_down = None;
        let Some(target) = self.target(x, y) else {
            return Ok(false);
        };
        let row = match target {
            Target::Strip(cell) => {
                // The cells as drawn, not as wanted: a strip that gave up a
                // readout to fit is a strip whose fourth cell is not the fourth
                // one this would otherwise dispatch.
                let cells = self.strip_fitted().0;
                return self.strip_action(cells[cell.min(cells.len() - 1)]);
            }
            // **Page-relative in, absolute out.** A tap reports the row it
            // landed on within what is drawn; the lists it dispatches against
            // are the whole thing.
            Target::Row(row) => self.page_window().start + row,
            Target::Option(item, option) => {
                let item = self.page_window().start + item;
                match self.mode {
                    Mode::Config => self.config_action(item, option)?,
                    // A file row's only chip is Delete.
                    Mode::Files(_) => self.arm_or_delete(item)?,
                    Mode::Help | Mode::Outline(_) | Mode::Writing | Mode::Naming { .. } => {}
                }
                return Ok(false);
            }
        };
        match &self.mode {
            // Every row is a document now, so a tap on one opens it. There is
            // no arithmetic past the end of the list to get wrong, because
            // there is nothing past the end of it.
            Mode::Files(files) => {
                if let Some(listing) = files.get(row) {
                    let path = listing.path.clone();
                    self.open(path)?;
                }
            }
            // Every row is a heading, and tapping one goes there. The list a
            // tap dispatches against is the list that was drawn — the panel
            // holds it rather than reading the document again, so a jump cannot
            // land on a heading that has moved since.
            Mode::Outline(sections) => {
                if let Some(at) = sections.get(row).map(|s| s.at) {
                    self.jump_to(at)?;
                }
            }
            // Every line of Config is a chip, so a bare row tap is a heading or
            // a label — nothing to run. Every line of Help is a fact, and a
            // fact does nothing when you press it.
            Mode::Config | Mode::Help | Mode::Writing | Mode::Naming { .. } => {}
        }
        Ok(false)
    }

    /// A tap on a document's Delete chip: arm it, or carry it out.
    ///
    /// **Two taps, because there is no undo and no bin.** The chip says which
    /// tap it is on, so the confirmation is where the finger already is rather
    /// than in a panel covering the name being read off the list.
    ///
    /// Arming another document disarms the first, which is what makes a stray
    /// tap harmless: the only way to reach the second tap is to aim at the same
    /// chip twice.
    fn arm_or_delete(&mut self, row: usize) -> Result<()> {
        let Mode::Files(files) = &self.mode else {
            return Ok(());
        };
        let Some(path) = files.get(row).map(|l| l.path.clone()) else {
            return Ok(());
        };
        if self.arming.as_ref() != Some(&path) {
            self.arming = Some(path);
            return self.paint();
        }
        self.arming = None;
        self.delete_document(&path)
    }

    /// Remove a document, and everything karyll remembers about it.
    fn delete_document(&mut self, path: &Path) -> Result<()> {
        if let Err(err) = std::fs::remove_file(path) {
            return self.show_status(&format!("Could not delete it: {err:#}"));
        }
        forget_position(path);
        eprintln!("deleted {}", path.display());

        // **The open document needs disowning before anything else runs**, or
        // the next thing to touch it puts it back: `open` saves a dirty buffer
        // on the way out, and the buffer is still holding every word of the
        // file just removed.
        if self.path.as_deref() == Some(path) {
            self.path = None;
            self.doc.mark_saved();
            let next = match list_documents().into_iter().next() {
                Some(listing) => listing.path,
                // Nothing left. A fresh one rather than an empty screen with no
                // file behind it, which is a page that cannot be saved into —
                // the same rule the launcher follows when it finds the
                // directory empty.
                None => {
                    let path = new_document();
                    let _ = std::fs::write(&path, "");
                    path
                }
            };
            self.load(next)?;
        }

        // Read again rather than dropping the row: the words and ages of
        // everything else are a snapshot from when the panel opened, and one of
        // them may be the document just opened in place of this.
        self.mode = Mode::Files(list_documents());
        self.panel_page = 0;
        self.paint()
    }

    /// Show what the keys and the glass do.
    fn open_help(&mut self) -> Result<()> {
        self.mode = Mode::Help;
        self.panel_page = 0;
        self.paint()
    }

    /// Show the document's headings, on the page holding the one the cursor is
    /// in.
    ///
    /// **Not page 1.** The outline is opened to get somewhere; forty sections
    /// into a draft, the top of the list is the part already written.
    fn open_outline(&mut self) -> Result<()> {
        let sections = self.sections();
        let cursor = self.doc.cursor();
        let here = sections.iter().rposition(|s| s.at <= cursor).unwrap_or(0);
        self.mode = Mode::Outline(sections);
        let capacity = self.layout().capacity().max(1);
        self.panel_page = here / capacity;
        self.paint()
    }

    /// Go to a heading, and put it at the top of the page.
    ///
    /// [`render::Scroll::Follow`] moves the page as little as it can, which is
    /// right while writing and wrong for a jump: the destination would land on
    /// the last line, with the whole section above the fold. `landing` is what
    /// says this paint is an arrival.
    ///
    /// Out through [`Editor::leave_panel`], so a jump tidies up after whatever
    /// panel was open exactly as `[ Done ]` does — a scan left running would go
    /// on drawing over the page arrived at.
    fn jump_to(&mut self, at: usize) -> Result<()> {
        self.doc.set_cursor(at);
        self.landing = true;
        self.leave_panel()
    }

    /// Set the page at another size.
    fn set_size(&mut self, px: f32) -> Result<()> {
        if self.theme.body_px == px {
            return Ok(());
        }
        write_size(px);
        self.reset_page(px, self.theme.chars)
    }

    /// Set the page to another line length.
    ///
    /// **This is the margin control**, though it is not named as one: the
    /// column is the characters asked for and the margin is the rest of the
    /// surface, so a shorter line is a wider margin. Naming it the other way
    /// round would make the setting mean a different amount of page on each of
    /// the three panels.
    fn set_line_length(&mut self, chars: u16) -> Result<()> {
        if self.theme.chars == chars {
            return Ok(());
        }
        write_line_length(chars);
        self.reset_page(self.theme.body_px, chars)
    }

    /// Lay the page out again at `px` and `chars`, and draw it.
    ///
    /// A full repaint, and it cannot be anything else: the measure, the margin
    /// and the leading all move with the type, so every line is somewhere new.
    /// The remembered frame describes a page that no longer exists.
    fn reset_page(&mut self, px: f32, chars: u16) -> Result<()> {
        let advance = font::average_advance(&mut self.fonts, px);
        self.theme = render::Theme::at(px, chars, advance);
        eprintln!(
            "page: {px} px, {chars} characters, measure {}",
            self.theme.measure
        );
        self.frame = None;
        self.paint()
    }

    /// Clear the panel of whatever it is holding onto, and draw the screen
    /// again.
    ///
    /// **Deliberate only.** There is no counter forcing one every N updates and
    /// no idle trigger, because neither is worth its cost until ghosting is
    /// something a writer actually sees. A flash nobody asked for, arriving
    /// whenever they paused to think, is worse than the residue it went looking
    /// for. This is the key for the day that changes.
    ///
    /// Everything remembered about what is on screen goes with it, or the next
    /// paint would compare against a frame describing a page that has just been
    /// painted over in black and redraw almost none of it.
    fn refresh_panel(&mut self) -> Result<()> {
        self.window.flash()?;
        self.frame = None;
        self.strip_drawn.clear();
        self.status_drawn.clear();
        self.paint()
    }

    /// Back to the page, from whichever panel is over it.
    ///
    /// The same thing `[ Done ]` does, and it has to be the same thing: a scan
    /// left running would go on drawing over whatever the writer went back to.
    /// This editor's answer to [`reopens`].
    ///
    /// Naming is not in it because it never reaches [`Editor::apply`] — it takes
    /// the keyboard itself, so `Ctrl`/`⌘`+`N` closing it lives in
    /// [`Editor::typed_name`] alongside Esc.
    fn reopens(&self, action: &Action) -> bool {
        reopens(
            &self.mode,
            self.find.is_some(),
            self.find.as_ref().is_some_and(|find| find.replacing),
            action,
        )
    }

    /// Close what [`Editor::reopens`] reported, by the door that surface's own
    /// Done or Esc already uses.
    fn close_reopened(&mut self, action: &Action) -> Result<()> {
        match action {
            Action::Find | Action::Replace => self.close_find(),
            _ => self.leave_panel(),
        }
    }

    fn leave_panel(&mut self) -> Result<()> {
        self.scanning = None;
        // A half-tapped Delete does not survive leaving the list. Correctness
        // does not need this — the second tap has to land on the same
        // document's chip, so a stale arm cannot delete anything else — but a
        // chip still reading `Delete?` on a list opened afresh would be the
        // page remembering something the writer has moved on from.
        self.arming = None;
        self.mode = Mode::Writing;
        self.paint()
    }

    /// Open the find bar, seeded from the selection if there is one.
    ///
    /// Seeded because every editor does it and the habit is worth serving:
    /// select a word you suspect you have overused, `Ctrl+F`, and the count is
    /// already on screen.
    fn open_find(&mut self) -> Result<()> {
        self.open_bar(false)
    }

    /// Open the bar's second field, or open the whole bar with it already
    /// showing.
    ///
    /// Reached from `Ctrl`/`⌘`+`Shift`+`F` on the page and from `[ Replace ]` on
    /// the find bar. It does not reopen: a query already typed must survive the
    /// second field appearing.
    ///
    /// The keys go to the new field, because asking for it is asking to type
    /// into it.
    fn open_replace(&mut self) -> Result<()> {
        if self.find.is_none() {
            return self.open_bar(true);
        }
        // The word being composed belongs to the field it was started in.
        self.abandon_composition();
        if let Some(find) = &mut self.find {
            find.replacing = true;
            find.field = Field::With;
            find.arming_all = false;
        }
        self.frame = None;
        self.paint()
    }

    /// Put the bar on the strip, in one state or the other, and go to the
    /// nearest match.
    fn open_bar(&mut self, replacing: bool) -> Result<()> {
        let query = self.doc.selected_text().unwrap_or_default();
        // A selection spanning a line break is not a phrase anybody meant to
        // search for, and it would never match anything anyway.
        let query = if query.contains('\n') {
            String::new()
        } else {
            query
        };
        self.find = Some(Find {
            query,
            replacing,
            // Whichever field the writer just asked for.
            field: if replacing { Field::With } else { Field::Query },
            ..Find::default()
        });
        // The chrome is almost certainly away — the writer has been typing —
        // and the bar is chrome.
        self.chrome_hidden = false;
        self.frame = None;
        self.research();
        self.paint()
    }

    /// Put the keys in one of the bar's two fields.
    fn focus_field(&mut self, which: Field) -> Result<()> {
        let already = self.find.as_ref().is_some_and(|f| f.field == which);
        if already {
            return Ok(());
        }
        // A composition belongs to the field it was started in, and moving with
        // one held would splice half a word into the other.
        self.abandon_composition();
        if let Some(find) = &mut self.find {
            find.field = which;
        }
        self.paint()
    }

    /// Swap between the two fields, which is what Tab does in every find bar
    /// that has two. Nothing while only one is showing.
    fn swap_field(&mut self) -> Result<()> {
        let Some(find) = &self.find else {
            return Ok(());
        };
        if !find.replacing {
            return Ok(());
        }
        let other = match find.field {
            Field::Query => Field::With,
            Field::With => Field::Query,
        };
        self.focus_field(other)
    }

    /// Change the match on screen, and step to the next.
    ///
    /// **Stepping on is the point.** Staying put would need a second gesture
    /// between every pair of changes, and the hit just changed no longer
    /// matches the query.
    fn change_one(&mut self) -> Result<()> {
        let Some(find) = &self.find else {
            return Ok(());
        };
        let (Some(hit), with) = (find.hits.get(find.at).cloned(), find.with.clone()) else {
            return Ok(());
        };
        if find.query.is_empty() {
            return Ok(());
        }
        self.doc.replace_range(hit, &with);
        self.note_edit();
        // The hits are stale the moment the document changes — every one after
        // this has moved. Searching again is what puts the writer on the next
        // one, and it is the same call the bar makes on every keystroke.
        self.research();
        self.frame = None;
        self.paint()
    }

    /// Take the arm off `[ All ]`, reporting whether it was on.
    ///
    /// **The confirming tap has to be the very next thing the writer does.**
    /// There is one `[ All ]` chip rather than one per row, so an arm left
    /// standing could be finished by a single tap meant for something else, on
    /// a replacement that is no longer the text that was armed.
    fn disarm_all(&mut self) -> bool {
        match &mut self.find {
            Some(find) if find.arming_all => {
                find.arming_all = false;
                true
            }
            _ => false,
        }
    }

    /// A tap on `[ All ]`: arm it, or carry it out.
    ///
    /// Two taps, the rule the Delete chip already follows. Replacing everything
    /// changes places the writer cannot see, and by touch alone there is no undo
    /// to reach for.
    fn arm_or_change_all(&mut self) -> Result<()> {
        let armed = self.find.as_ref().is_some_and(|f| f.arming_all);
        if !armed {
            if let Some(find) = &mut self.find {
                find.arming_all = true;
            }
            return self.paint();
        }
        self.change_all()
    }

    /// Change every match, as one undo step.
    fn change_all(&mut self) -> Result<()> {
        let Some(find) = &mut self.find else {
            return Ok(());
        };
        find.arming_all = false;
        if find.query.is_empty() {
            return self.paint();
        }
        let hits = std::mem::take(&mut find.hits);
        let with = find.with.clone();
        let changed = self.doc.replace_all(&hits, &with);
        eprintln!("replace: {changed} changed");
        self.note_edit();
        self.research();
        self.frame = None;
        self.paint()
    }

    /// Stamp the document as just touched, the way [`Editor::apply`] does for
    /// every keystroke — autosave reads it, and a replace is an edit.
    fn note_edit(&mut self) {
        self.last_edit = Some(std::time::Instant::now());
    }

    /// Recompute the hits and go to the one nearest the cursor.
    ///
    /// Run on every keystroke in the bar: a search that only answers on Enter
    /// makes the writer type blind, and this document is small enough that
    /// answering as they type costs nothing.
    fn research(&mut self) {
        let Some(find) = &self.find else { return };
        let needle: Vec<char> = find.query.chars().collect();
        let chars = self.doc.chars();
        let hits = karyll_core::find::matches(&chars, &needle);
        // From the *start* of the selection, not the cursor: arriving at a hit
        // leaves the cursor at its end, and searching on from there for a
        // longer word would skip the hit the writer is looking at.
        let from = self
            .doc
            .selection()
            .map_or_else(|| self.doc.cursor(), |s| s.start);
        let at = karyll_core::find::from(&hits, from).unwrap_or(0);
        if let Some(find) = &mut self.find {
            find.hits = hits;
            find.at = at;
        }
        self.show_hit();
    }

    /// Select the current hit, which is what scrolls the page to it and inverts
    /// it — the selection was already drawn that way.
    fn show_hit(&mut self) {
        let Some(find) = &self.find else { return };
        let Some(hit) = find.hits.get(find.at).cloned() else {
            // Nothing matches, so nothing is highlighted. Leaving the last hit
            // inverted while the bar says "not found" shows the writer a match
            // for a search that has none.
            self.doc.clear_selection();
            return;
        };
        self.doc.select(hit);
    }

    /// Step to the next hit, or the previous one going back. Wraps either way.
    fn step_find(&mut self, back: bool) {
        // From the selection's *start*. `select` leaves the cursor at the end
        // of the range, so stepping from the cursor would measure forwards from
        // one edge of the hit and backwards from the other.
        let cursor = self
            .doc
            .selection()
            .map_or_else(|| self.doc.cursor(), |s| s.start);
        let Some(find) = &self.find else { return };
        let at = if back {
            karyll_core::find::previous(&find.hits, cursor)
        } else {
            karyll_core::find::next(&find.hits, cursor)
        };
        if let (Some(at), Some(find)) = (at, &mut self.find) {
            find.at = at;
        }
        self.show_hit();
    }

    /// Close the find bar, leaving the cursor on the hit it found.
    ///
    /// **Leaving it there is the point.** Esc in a find bar means "stop
    /// searching", not "forget where I got to" — every editor lands the writer
    /// at the match, and the match is still selected, so the next keystroke can
    /// replace it.
    fn close_find(&mut self) -> Result<()> {
        if self.find.take().is_none() {
            return Ok(());
        }
        self.frame = None;
        self.paint()
    }

    /// A keystroke the IME did not want, while the find bar is open. True once
    /// the bar has closed.
    ///
    /// Reached only after [`Editor::compose_key`] has passed, so every arm here
    /// is the "nothing is being composed" case: `Esc` closes the bar because
    /// there is no composition left to abandon, and `Enter` steps to the next
    /// hit because there is nothing left to commit.
    fn typed_query(&mut self, action: &Action) -> Result<bool> {
        // The same rule the strip follows: the tap or key confirming `[ All ]`
        // has to be the next thing the writer does. `ChangeAll` is the chord
        // that carries it out and clears the arm itself.
        if !matches!(action, Action::ChangeAll) && self.disarm_all() {
            self.paint()?;
        }
        match action {
            Action::Escape => {
                self.close_find()?;
                return Ok(true);
            }
            // The chord that opened the bar closes it, the same way it does
            // from the page. `Ctrl`/`⌘`+`Shift`+`F` only closes a bar that is
            // already showing the second field; on a plain one it reveals it,
            // which is the arm below.
            Action::Find => {
                self.close_find()?;
                return Ok(true);
            }
            Action::Replace if self.reopens(&Action::Replace) => {
                self.close_find()?;
                return Ok(true);
            }
            // Enter steps on, Shift+Enter steps back. Shift+Enter reaches here
            // as `CommitTyped` — mid-composition it means "the letters, not the
            // conversion" — and out of one it is only ever Enter with Shift
            // held. Matching `Newline` alone and reading the modifier lost the
            // backwards step entirely, because with Shift down it is not a
            // `Newline` to begin with.
            Action::Newline | Action::CommitTyped => self.step_find(self.mods.shift),
            // **Enter keeps one meaning in both fields.** Stepping is what it
            // does in a find bar; a key that edited the document in one field
            // and only moved in the other could not be trusted. Changing is its
            // own chord.
            Action::Change => return self.change_one().map(|()| false),
            Action::ChangeAll => return self.change_all().map(|()| false),
            // Tab moves between the two fields, as it does in every find bar
            // that has two. It cannot mean an indent here: this is a one-line
            // field, not the page.
            Action::Indent => return self.swap_field().map(|()| false),
            Action::Replace => return self.open_replace().map(|()| false),
            Action::Backspace => {
                if self.edit_field(|text| {
                    text.pop();
                }) {
                    self.research();
                }
            }
            // The way back to Latin with a CJK engine switched on, and the
            // reason it is here rather than left to fall through: every letter
            // goes to the engine while the mode is on, so a bar with no way to
            // switch is a bar that cannot search an English word in a Chinese
            // document. Ctrl+Space is the binding everywhere else in the app.
            Action::CycleLanguage => self.cycle_language(),
            // The panel is the panel whatever is being typed into it.
            Action::Refresh => return self.refresh_panel().map(|()| false),
            Action::Insert(c) => {
                let c = *c;
                if self.edit_field(|text| text.push(c)) {
                    self.research();
                }
            }
            // Chords, arrows and page keys mean nothing to a one-line field,
            // and must not repaint over the hit the writer is looking at.
            _ => return Ok(false),
        }
        self.paint()?;
        Ok(false)
    }

    /// Change whichever of the bar's fields is taking keys, reporting whether
    /// it was the query.
    ///
    /// One place, so a keystroke cannot land in the query while the caret is
    /// drawn on the replacement. The answer says whether the document has to be
    /// searched again: a replacement does not move the matches.
    fn edit_field(&mut self, change: impl FnOnce(&mut String)) -> bool {
        let Some(find) = &mut self.find else {
            return false;
        };
        match find.field {
            Field::Query => {
                change(&mut find.query);
                true
            }
            Field::With => {
                change(&mut find.with);
                false
            }
        }
    }

    /// Open the name prompt, for a new document or to rename the open one.
    fn start_naming(&mut self, for_new: bool) -> Result<()> {
        let name = if for_new {
            String::new()
        } else {
            self.path
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        self.mode = Mode::Naming { for_new, name };
        self.paint()
    }

    /// Run the chip a finger landed on, against the list it was drawn from.
    fn config_action(&mut self, item: usize, option: usize) -> Result<()> {
        match self.config_items().into_iter().nth(item).map(|(_, a)| a) {
            Some(ConfigRow::Languages(languages)) => {
                let Some(language) = languages.get(option) else {
                    return Ok(());
                };
                self.toggle_language(*language);
            }
            Some(ConfigRow::Font(group, installed)) => {
                let Some(family) = installed.get(option) else {
                    return Ok(());
                };
                self.set_family(group, *family);
            }
            // It paints itself, because the chip that moves is on the page
            // doing the painting. The panel's own type does not change with
            // this — chrome is set at [`ui::TEXT_PX`] and stays there, the way
            // a toolbar does not grow with the document it sits under.
            Some(ConfigRow::Size) => {
                return match render::SIZES.get(option) {
                    Some(px) => self.set_size(*px),
                    None => Ok(()),
                };
            }
            Some(ConfigRow::LineLength) => {
                return match render::LINE_LENGTHS.get(option) {
                    Some(chars) => self.set_line_length(*chars),
                    None => Ok(()),
                };
            }
            // These paint themselves: each reports what the daemon said, and a
            // scan goes on repainting for the ten seconds it runs.
            Some(ConfigRow::Keyboard(actions)) => {
                return match actions.get(option) {
                    Some(KeyAction::Disconnect(device)) => self.disconnect(&device.clone()),
                    Some(KeyAction::Forget(device)) => self.forget(&device.clone()),
                    Some(KeyAction::Pair(device)) => self.pair_with(&device.clone()),
                    Some(KeyAction::Scan) => self.start_scan(),
                    None => Ok(()),
                };
            }
            // It paints itself: the status line carries the cost, which the chip
            // has no room for.
            Some(ConfigRow::KeepBluetooth) => return self.set_keep_bluetooth(option == 1),
            Some(ConfigRow::Screen) => {
                return match SCREENS.get(option) {
                    Some((_, way)) => self.turn_to(*way),
                    None => Ok(()),
                };
            }
            Some(ConfigRow::Colour) => return self.set_colour(option == 1),
            Some(ConfigRow::CaretColour) => {
                let mut inks = self.window.colours();
                inks.caret = option;
                return self.set_colours(inks);
            }
            Some(ConfigRow::HighlightColour) => {
                let mut inks = self.window.colours();
                inks.highlight = option;
                return self.set_colours(inks);
            }
            Some(ConfigRow::None) | None => return Ok(()),
        }
        self.paint()
    }

    /// Break the line, carrying a list or quote marker onto the next one.
    ///
    /// The list the cursor is *in* is the line it sits on, so this reads the
    /// text before the cursor rather than the whole line — breaking in the
    /// middle of an item continues it from what is above, which is what every
    /// editor does.
    fn newline(&mut self) {
        let chars = self.doc.chars();
        let start = self.doc.line_start(self.doc.cursor());
        let line = &chars[start..self.doc.cursor().min(chars.len())];
        match karyll_core::continues(line) {
            karyll_core::Continue::Break => self.doc.insert_char('\n'),
            karyll_core::Continue::Marker(marker) => {
                self.doc.insert(&format!("\n{marker}"));
            }
            // The empty marker goes with the break, so Enter on a bare bullet
            // leaves a clean blank line rather than a stranded `- `.
            karyll_core::Continue::End(back) => {
                for _ in 0..back {
                    self.doc.backspace();
                }
                self.doc.insert_char('\n');
            }
        }
    }

    /// Wrap the selection, or the word under the cursor, in `marker`.
    ///
    /// **With nothing selected it takes the word**, which is what makes the
    /// shortcut worth having: reaching for the mouse to select one word before
    /// emboldening it is most of the work the shortcut was meant to save.
    ///
    /// The wrapped text is left selected, so the same key is a round trip and
    /// the writer can see what was affected.
    /// Tick the task the cursor is on, or untick it.
    ///
    /// One character, in place, and **the cursor does not move**: this is a mark
    /// against a line being read, not an edit being made. Nothing happens on a
    /// line that is not a task.
    fn toggle_task(&mut self) {
        let chars = self.doc.chars();
        let start = self.doc.line_start(self.doc.cursor());
        let end = self.doc.line_end(self.doc.cursor());
        let Some((at, done)) = karyll_core::markdown::task_box(&chars[start..end]) else {
            return;
        };
        let was = self.doc.cursor();
        self.doc
            .replace_range(start + at..start + at + 1, if done { " " } else { "x" });
        self.doc.set_cursor(was);
    }

    fn emphasise(&mut self, marker: &'static str) {
        let chars = self.doc.chars();
        let span = self
            .doc
            .selection()
            .unwrap_or_else(|| karyll_core::word_at(&chars, self.doc.cursor(), self.dict.as_ref()));
        if span.is_empty() {
            return;
        }
        let (range, text) = karyll_core::toggle_emphasis(&chars, span, marker);
        let width = marker.chars().count();
        // Where the text itself ends up, markers excluded — which is the same
        // span the next press has to find in order to undo this one.
        let inner = if text.starts_with(marker) {
            range.start + width..range.start + text.chars().count() - width
        } else {
            range.start..range.start + text.chars().count()
        };
        self.doc.select(range);
        self.doc.insert(&text);
        self.doc.select(inner);
    }

    /// Set the line the cursor is on to a heading level, or back to prose.
    fn set_heading(&mut self, level: u8) {
        let chars = self.doc.chars();
        let cursor = self.doc.cursor();
        let start = self.doc.line_start(cursor);
        let end = self.doc.line_end(cursor);
        let line = &chars[start..end];
        let replacement = karyll_core::toggle_heading(line, level);

        // The cursor keeps its place *in the words*, not its offset in the
        // line: adding `## ` should not leave it two characters further into
        // the sentence than the writer left it.
        let shift = replacement.chars().count() as i64 - line.len() as i64;
        let moved = (cursor as i64 + shift).clamp(start as i64, i64::MAX) as usize;
        self.doc.select(start..end);
        self.doc.insert(&replacement);
        self.doc.set_cursor(moved.min(self.doc.len()));
    }

    /// The Config panel, paired with what each control does.
    ///
    /// One list, as the Keyboard panel learned to be: drawing and hit-testing
    /// both come from here, so a control cannot say one thing and do another.
    ///
    /// **Two sections, and every line is a label with its values beside it.**
    /// Undifferentiated rows hiding their choices behind a tap that cycles them
    /// are a phone's answer to a phone's problem, and this is a 10.2″ panel
    /// with half the page empty. Every value is on screen, and picking one is a
    /// tap on the value itself rather than a walk through the others.
    ///
    /// **The action carries the list it was drawn from**, rather than a group
    /// the handler would look up again: the chips are only the *installed*
    /// families, so a second call to `available` deciding what option 1 means
    /// is the two-lists bug with extra steps.
    fn config_items(&self) -> Vec<(ui::Item, ConfigRow)> {
        // **Pairing comes first, because it is the first thing a new writer has
        // to do.** There is nothing to type on until they have done it, and
        // nothing else on this page can be reached from a keyboard that does
        // not exist yet.
        let mut items = vec![(ui::Item::Heading("Keyboard".into()), ConfigRow::None)];
        items.extend(self.keyboard_items());

        items.push((ui::Item::Heading("Input".into()), ConfigRow::None));
        let languages: Vec<Language> = Language::ALL.into_iter().collect();
        items.push((
            ui::Item::Choice {
                label: "Languages".into(),
                options: languages.iter().map(|l| l.label().to_string()).collect(),
                on: languages.iter().map(|l| self.enabled.contains(l)).collect(),
                inert: Vec::new(),
            },
            ConfigRow::Languages(languages),
        ));

        // A writing system with nothing installed at all is left off entirely.
        // One that has exactly one keeps its row: the chip cannot be changed,
        // but it still says what the writing on screen is set in, which is half
        // of what the row is for.
        let type_rows: Vec<(ui::Item, ConfigRow)> = font_groups(&self.enabled)
            .into_iter()
            .flat_map(|group| {
                let installed = font::available(group);
                let chosen = self.fonts.choices().get(group);
                chip_rows(&installed)
                    .into_iter()
                    .enumerate()
                    .map(move |(row, part)| {
                        (
                            ui::Item::Choice {
                                // Only the first says which writing system this
                                // is. A second name would read as a second
                                // setting, and there is one face per system.
                                label: if row == 0 {
                                    group.label().into()
                                } else {
                                    String::new()
                                },
                                options: part
                                    .iter()
                                    .map(|at| font::families(group)[*at].name.to_string())
                                    .collect(),
                                on: part.iter().map(|at| *at == chosen).collect(),
                                inert: Vec::new(),
                            },
                            ConfigRow::Font(group, part),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        items.push((ui::Item::Heading("Type".into()), ConfigRow::None));
        // Size before the faces: it is the setting a writer reaches for first,
        // and unlike them it applies to every writing system at once.
        items.push((
            ui::Item::Choice {
                label: "Size".into(),
                options: render::SIZES.iter().map(|px| format!("{px:.0}")).collect(),
                on: render::SIZES
                    .iter()
                    .map(|px| *px == self.theme.body_px)
                    .collect(),
                inert: Vec::new(),
            },
            ConfigRow::Size,
        ));
        // Directly under Size, because the two are read together: the pair is
        // what decides how much page is left around the text, and neither
        // number means much without the other in view.
        items.push((
            ui::Item::Choice {
                label: "Line length".into(),
                options: render::LINE_LENGTHS
                    .iter()
                    .map(|chars| chars.to_string())
                    .collect(),
                on: render::LINE_LENGTHS
                    .iter()
                    .map(|chars| *chars == self.theme.chars)
                    .collect(),
                inert: Vec::new(),
            },
            ConfigRow::LineLength,
        ));
        items.extend(type_rows);

        // Only on a Kindle that has a colour panel to switch off, the same rule
        // the Screen section follows below. One row: whether the caret and the
        // highlighter use the panel.
        if self.window.colour_capable() {
            let on = self.window.colour();
            items.push((ui::Item::Heading("Colour".into()), ConfigRow::None));
            items.push((
                ui::Item::Choice {
                    label: "Caret and highlights".into(),
                    options: vec!["Grey".into(), "Colour".into()],
                    on: vec![!on, on],
                    inert: Vec::new(),
                },
                ConfigRow::Colour,
            ));
            // Only while there is colour to pick. Off, the swatches would be
            // drawn through the grey palette — six near-black circles, which
            // is a picker that lies about every one of its choices.
            if on {
                let inks = self.window.colours();
                let swatches = |label: &str, chosen: usize| ui::Item::Swatches {
                    label: label.into(),
                    inks: (0..window::COLOURS.len())
                        .map(window::ink::swatch)
                        .collect(),
                    on: (0..window::COLOURS.len()).map(|at| at == chosen).collect(),
                };
                items.push((swatches("Caret", inks.caret), ConfigRow::CaretColour));
                items.push((
                    swatches("Highlight", inks.highlight),
                    ConfigRow::HighlightColour,
                ));
            }
        }

        // Last, and only where it is the only way: a Kindle that turns its own
        // page over has the better control already, and it is the device.
        if !self.turns_itself {
            let now = self.window.orientation();
            items.push((ui::Item::Heading("Screen".into()), ConfigRow::None));
            items.push((
                ui::Item::Choice {
                    label: "Hold it".into(),
                    options: SCREENS.iter().map(|(name, _)| (*name).into()).collect(),
                    on: SCREENS.iter().map(|(_, way)| *way == now).collect(),
                    inert: Vec::new(),
                },
                ConfigRow::Screen,
            ));
        }
        items
    }

    /// The Keyboard section: a line per keyboard, and the scan that finds more.
    ///
    /// Remembered keyboards first — the daemon keeps them and their link keys
    /// across restarts, so this is where "already paired" is visible at all —
    /// then anything a scan has turned up that is not already known, then the
    /// scan itself.
    ///
    /// Each keyboard is **one line with its actions beside it** rather than the
    /// two stacked rows this was when it had a screen of its own, where Forget
    /// sat under the device it forgot and read like a second keyboard.
    fn keyboard_items(&self) -> Vec<(ui::Item, ConfigRow)> {
        let mut items: Vec<(ui::Item, ConfigRow)> = self
            .paired
            .iter()
            .map(|device| {
                // **The first chip answers for this keyboard, not for the
                // radio.** With several remembered only one of them holds the
                // link, so a row reading the attached-or-not flag calls every
                // one of them the keyboard being typed on.
                //
                // **`Connect` is grey and does nothing**, because there is no
                // request behind it: the daemon is already waiting on every
                // remembered keyboard at once and takes the first that answers,
                // so the way to reach this one is to wake it — and, if another
                // holds the link, to Disconnect that one. Keeping the word in
                // grey holds the column steady while saying it is a state.
                let connected = self.is_connected(device);
                (
                    ui::Item::Choice {
                        label: device.name.clone(),
                        options: vec![
                            if connected { "Disconnect" } else { "Connect" }.into(),
                            "Forget".into(),
                        ],
                        on: vec![connected, false],
                        inert: vec![!connected, false],
                    },
                    ConfigRow::Keyboard(vec![
                        KeyAction::Disconnect(device.clone()),
                        KeyAction::Forget(device.clone()),
                    ]),
                )
            })
            .collect();

        items.extend(
            self.found
                .iter()
                .filter(|d| {
                    !self
                        .paired
                        .iter()
                        .any(|p| hid::same_address(&p.address, &d.address))
                })
                .map(|device| {
                    (
                        ui::Item::Choice {
                            label: format!("{}  ({})", device.name, device.protocol),
                            options: vec!["Pair".into()],
                            on: vec![false],
                            inert: Vec::new(),
                        },
                        ConfigRow::Keyboard(vec![KeyAction::Pair(device.clone())]),
                    )
                }),
        );

        // Deliberately not started on opening Config. Scanning suspends the
        // daemon — the log says `Connection cancelled (suspend)` — which drops
        // the very keyboard being typed on. What is remembered shows without
        // asking; scanning is a choice.
        //
        // **It says what it is doing, including when that is nothing yet.** The
        // editor opens without waiting for the radio, so for the first seconds
        // of a session there is no daemon to answer — and a chip that goes on
        // offering a scan then is not being kind, it is being wrong twice: it
        // hides a state the writer can see the consequences of, and it answers
        // a tap with a connection error.
        items.push((
            ui::Item::Choice {
                label: "Bluetooth".into(),
                options: vec![match (self.bluetooth.ready(), self.scanning) {
                    (hid::Ready::Starting, _) => "Starting…".to_string(),
                    (hid::Ready::Unavailable, _) => "Unavailable".to_string(),
                    (_, Some(started)) => {
                        format!("Scanning… {}s", started.elapsed().as_secs())
                    }
                    (_, None) => "Scan for keyboards".to_string(),
                }],
                on: vec![self.scanning.is_some()],
                inert: Vec::new(),
            },
            ConfigRow::Keyboard(vec![KeyAction::Scan]),
        ));

        // Last in the section: it is about the keyboard, not about any one of
        // them. The keyboard karyll pairs is the device's only keyboard, so this
        // row is the only place to ask for it outside karyll. What it does is in
        // [`hid::Hid::set_keep_alive`].
        let keep = self.bluetooth.keep_alive();
        items.push((
            ui::Item::Choice {
                label: "When karyll closes".into(),
                options: vec!["Turn Bluetooth off".into(), "Keep it on".into()],
                on: vec![!keep, keep],
                inert: Vec::new(),
            },
            ConfigRow::KeepBluetooth,
        ));
        items
    }

    /// Draw a writing system in the face that was tapped.
    ///
    /// The page is laid out again from scratch on the way back to it — two
    /// families are not the same height, so every row moves and the remembered
    /// frame describes a screen that no longer exists. Leaving the panel does
    /// that anyway, and [`Editor::paint`] drops the frame whenever a panel is
    /// up, so there is nothing extra to invalidate here.
    fn set_family(&mut self, group: font::Group, family: usize) {
        if self.fonts.choices().get(group) == family {
            return;
        }
        self.fonts.set_family(group, family);
        write_choices(self.fonts.choices());
        // The measure is a character count, so a face of a different width
        // moves the column rather than the line length. A Han family leaves the
        // Latin advance where it was and this settles back on the same number.
        let advance = font::average_advance(&mut self.fonts, self.theme.body_px);
        self.theme = render::Theme::at(self.theme.body_px, self.theme.chars, advance);
        self.frame = None;
        eprintln!(
            "font: {} in {}, measure {}",
            group.label(),
            self.fonts.family(group).name,
            self.theme.measure
        );
    }

    /// Switch an input source on or off.
    ///
    /// **The last one cannot be switched off.** An empty set would leave no way
    /// to type, and a settings screen that can make the app unusable is a worse
    /// answer than a row that declines.
    ///
    /// Switching off the one in use moves to the next that is still on, rather
    /// than leaving the keyboard in a source the cycle can no longer reach.
    fn toggle_language(&mut self, language: Language) {
        if self.enabled.contains(&language) {
            if self.enabled.len() == 1 {
                eprintln!("config: {} is the only one left", language.label());
                return;
            }
            self.enabled.retain(|l| *l != language);
        } else {
            self.enabled.push(language);
            self.enabled
                .sort_by_key(|l| Language::ALL.iter().position(|a| a == l).unwrap_or(0));
        }
        write_languages(&self.enabled);
        eprintln!(
            "config: cycling {}",
            self.enabled
                .iter()
                .map(|l| l.label())
                .collect::<Vec<_>>()
                .join(" ")
        );
        if !self.enabled.contains(&self.language) {
            self.set_language(self.language.next(&self.enabled));
        }
    }

    fn strip_action(&mut self, button: Bar) -> Result<bool> {
        // Reaching for any other button takes the arm off `[ All ]`, and says
        // so before the button does whatever it does — the chip going back to
        // `All` is the writer being told the confirmation lapsed.
        if button != Bar::All && self.disarm_all() {
            self.paint()?;
        }
        match button {
            Bar::Exit => return Ok(true),
            // Tapping a field puts the keys in it. On the find bar there is
            // only one and it already has them; on the replace bar this is how
            // a hand on the glass gets from one to the other.
            Bar::Query => self.focus_field(Field::Query)?,
            Bar::With => self.focus_field(Field::With)?,
            Bar::Count => {}
            Bar::Replace => self.open_replace()?,
            Bar::Change => self.change_one()?,
            Bar::All => self.arm_or_change_all()?,
            Bar::Previous => {
                self.step_find(true);
                self.paint()?;
            }
            Bar::Next => {
                self.step_find(false);
                self.paint()?;
            }
            // Done closes the find bar when that is what is on the strip. The
            // cell says Done and means it; which Done depends on what is open.
            Bar::Done if self.find.is_some() => self.close_find()?,
            Bar::Done | Bar::Cancel => self.leave_panel()?,
            Bar::Files => {
                self.mode = Mode::Files(list_documents());
                self.panel_page = 0;
                self.arming = None;
                self.paint()?;
            }
            Bar::Help => self.open_help()?,
            Bar::Outline => self.open_outline()?,
            Bar::New => self.start_naming(true)?,
            Bar::Rename => self.start_naming(false)?,
            Bar::PageBack => self.turn_page(true)?,
            Bar::PageOn => self.turn_page(false)?,
            Bar::PageAt => {}
            Bar::Config => {
                self.mode = Mode::Config;
                // What the daemon remembers, read fresh — the Keyboard section
                // is drawn from it. Last session's scan results are not: those
                // keyboards were in the room then, not necessarily now.
                self.refresh_paired();
                self.found.clear();
                self.panel_page = 0;
                self.paint()?;
            }
        }
        Ok(false)
    }

    /// Switch to another document, saving the current one first.
    fn open(&mut self, path: PathBuf) -> Result<()> {
        self.load(path)?;
        self.mode = Mode::Writing;
        self.paint()
    }

    /// Take up a document without deciding what screen to show afterwards.
    ///
    /// Split out for the one caller that has somewhere else to go: deleting the
    /// open document has to put another one behind the Files list, and going
    /// through `open` painted the writing screen first and the list over it —
    /// two full-screen updates, which on this panel is a second of flashing for
    /// one tap.
    fn load(&mut self, path: PathBuf) -> Result<()> {
        if self.doc.is_dirty() {
            self.save()?;
        }
        // The outgoing draft's place is kept before it is replaced, or
        // switching away and back would lose it.
        self.remember_position();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        self.doc = Document::from_text(&text);
        self.doc.set_cursor(opening_cursor(&path, self.doc.len()));
        self.path = Some(path);
        Ok(())
    }

    /// A key or a finger just arrived: the writer is here, so hold the
    /// screensaver off.
    fn note_input(&mut self) {
        self.last_input = std::time::Instant::now();
        if !self.holding_awake {
            power::prevent_screensaver(true);
            self.holding_awake = true;
        }
    }

    /// Let the device sleep once the writer has been away long enough.
    ///
    /// **The latch is on writing, not on the app being open.** Holding it for
    /// the whole session fixed the screensaver arriving mid-sentence and bought
    /// a Kindle that could never sleep: it also holds WiFi awake, and a device
    /// rated in weeks of standby was flat by morning if the editor was left on a
    /// desk.
    ///
    /// Safe to give back only because of the keyboard fix: a suspend destroys
    /// the Bluetooth keyboard's `/dev/input` node, and until `wait` learned to
    /// report a hangup, waking up meant an editor that never typed again.
    fn poll_sleep(&mut self) {
        if !self.holding_awake || self.last_input.elapsed() < power::IDLE_SLEEP {
            return;
        }
        eprintln!(
            "power: no input for {} minutes — letting the device sleep",
            power::IDLE_SLEEP.as_secs() / 60
        );
        power::prevent_screensaver(false);
        self.holding_awake = false;
    }

    /// Record where the cursor is in the open document.
    ///
    /// Called wherever a document is left — saved, switched away from, or
    /// quit. Not in `Drop`: this binary is built with `panic = "abort"`, so
    /// `Drop` is not a place anything important can live.
    fn remember_position(&self) {
        if let Some(path) = &self.path {
            write_position(path, self.doc.cursor());
        }
    }

    /// Which slice of the current panel's list is on screen.
    ///
    /// **The one place the page offset is turned into indices**, so drawing,
    /// hit-testing and dispatch all take the same slice of the same list. Two
    /// of them disagreeing about where page 2 starts would open the wrong
    /// document — the "one list, not two" failure with an offset added.
    /// **It clamps in place**, which is why it takes `&mut self`. A list can
    /// shrink under the page you are on — a scan result that stops answering,
    /// a keyboard forgotten — and stranding the writer on a blank page with a
    /// `More` button that wraps oddly is worse than quietly moving them back.
    fn page_window(&mut self) -> std::ops::Range<usize> {
        let capacity = self.layout().capacity().max(1);
        let pages = self.panel_len().div_ceil(capacity).max(1);
        self.panel_page = self.panel_page.min(pages - 1);
        let start = self.panel_page * capacity;
        start..start + capacity
    }

    /// The lines of the current panel that are actually on screen.
    ///
    /// Everything that draws or hits the list comes through here rather than
    /// through [`Editor::panel_items`], so nothing can see a line the writer
    /// cannot.
    fn visible_items(&mut self) -> Vec<ui::Item> {
        let window = self.page_window();
        self.panel_items()
            .into_iter()
            .skip(window.start)
            .take(window.len())
            .collect()
    }

    /// How many lines the current panel has in total, paged or not.
    fn panel_len(&self) -> usize {
        match &self.mode {
            Mode::Files(files) => files.len(),
            Mode::Config => self.config_items().len(),
            Mode::Help => help_items().len(),
            // A document with no headings still has the one line saying so.
            Mode::Outline(sections) => sections.len().max(1),
            Mode::Writing | Mode::Naming { .. } => 0,
        }
    }

    /// How many pages the current panel takes.
    fn pages(&mut self) -> usize {
        let capacity = self.layout().capacity().max(1);
        self.panel_len().div_ceil(capacity).max(1)
    }

    /// Turn a page, wrapping either way.
    ///
    /// Wrapping rather than stopping, because a button that does nothing at one
    /// end is a button you press twice to find out — and because the pair then
    /// never has to appear and disappear, which on fitted cells would move the
    /// other buttons out from under a finger.
    fn turn_page(&mut self, back: bool) -> Result<()> {
        let pages = self.pages();
        self.panel_page = if back {
            (self.panel_page + pages - 1) % pages
        } else {
            (self.panel_page + 1) % pages
        };
        self.paint()
    }

    fn panel_items(&self) -> Vec<ui::Item> {
        match &self.mode {
            // **A list of documents, and nothing that is not one.** New and
            // Rename are on the strip: they were rows here, in the same rules
            // as the files, which is the category error the Keyboard row made
            // on the Config page.
            //
            // Each line says what is worth knowing before opening it — how long
            // it is and how lately it was written — because four identical
            // filenames on a 10.2″ page answer neither question.
            Mode::Files(files) => files
                .iter()
                .map(|listing| {
                    let open = Some(&listing.path) == self.path.as_ref();
                    // The open document's count comes from the buffer, not from
                    // the file: what is on disk is a save behind whatever has
                    // just been typed.
                    let words = if open {
                        karyll_core::words::count(&self.doc.chars())
                    } else {
                        listing.words
                    };
                    ui::Item::Row {
                        label: listing
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        detail: describe_listing(words, listing.modified, open),
                        on: open,
                        // **Two taps, and the chip says which one it is on.**
                        // Deleting prose cannot be undone and there is no bin on
                        // this device to fish it out of, so the first tap only
                        // arms it. A confirmation panel would be the heavier
                        // answer and would cover the list the writer is reading
                        // the name off.
                        action: Some(
                            if self.arming.as_ref() == Some(&listing.path) {
                                "Delete?"
                            } else {
                                "Delete"
                            }
                            .into(),
                        ),
                    }
                })
                .collect(),
            // Derived from `keyboard_rows`, never rebuilt. A second listing
            // agrees with it only while nothing is paired: each remembered
            // keyboard adds two rows, so the panel would draw and hit-test one
            // list while taps dispatched against another. "One list, not two."
            Mode::Config => self
                .config_items()
                .into_iter()
                .map(|(item, _)| item)
                .collect(),
            Mode::Help => help_items(),
            // **Indented by level**, which is what makes it an outline rather
            // than a list of headings: the shape of the draft is the thing
            // being looked at, and a flat column of names does not have one.
            //
            // A document with no headings gets a heading item — not a row —
            // because a row is something you tap and there is nothing here to
            // go to. [`ui::hit`] guarantees the difference.
            Mode::Outline(sections) if sections.is_empty() => {
                vec![ui::Item::Heading("No headings in this document".into())]
            }
            Mode::Outline(sections) => outline_items(sections, self.doc.cursor()),
            Mode::Writing | Mode::Naming { .. } => Vec::new(),
        }
    }

    /// Every heading in the open document, in order.
    fn sections(&self) -> Vec<Section> {
        sections_of(&self.doc.chars())
    }

    /// Start a scan. Results are collected by [`Editor::poll_scan`] on the
    /// loop's tick.
    ///
    /// Not a blocking sleep: holding the loop for twenty seconds stops the
    /// panel repainting and queues taps behind it, which reads as the app being
    /// dead and invites tapping again.
    fn start_scan(&mut self) -> Result<()> {
        // Tapping the chip again while it counts would restart the ten seconds
        // and leave the writer waiting longer for having asked twice.
        if self.scanning.is_some() {
            return Ok(());
        }
        if self.keyboard_present {
            // Worth saying, because the keyboard will go quiet for the duration
            // and come back on its own afterwards.
            self.show_status("Scanning disconnects the keyboard for a moment…")?;
        }
        // **Still coming up is not a failure to report as one.** The editor
        // does not wait for the radio, so this chip can be tapped seconds
        // before there is anything listening on the other end, and the daemon
        // is already on its way — there is nothing to do but say so.
        match self.bluetooth.ready() {
            hid::Ready::Starting => {
                self.scanning = None;
                return self.show_status("Bluetooth is still starting — try again in a moment.");
            }
            hid::Ready::Unavailable => {
                self.scanning = None;
                if let Err(err) = self.bluetooth.start() {
                    return self.show_status(&format!("Bluetooth would not start: {err:#}"));
                }
                return self.show_status("Starting Bluetooth…");
            }
            hid::Ready::Up => {}
        }
        if let Err(err) = self.bluetooth.scan() {
            self.scanning = None;
            return self.show_status(&format!("Could not scan: {err:#}"));
        }
        self.scanning = Some(std::time::Instant::now());
        self.polled = Some(std::time::Instant::now());
        self.show_status("Scanning…")
    }

    /// Follow the framework if it has turned the screen under us.
    ///
    /// The compositor rotates our pixels for us, so nothing needs redrawing —
    /// but the touchscreen is panel-fixed, so the mapping does change, and
    /// without this every tap after a 180° flip lands on the mirror of where it
    /// was aimed and the buttons appear dead.
    /// Turn the page to match the way the device is being held.
    ///
    /// **Not a *Rotate* button**, which would be an invisible mode: turning the
    /// Scribe ninety degrees would do nothing until you remembered the button
    /// was there, and until you tapped it the app's idea of which way was up
    /// would silently disagree with the
    /// device's. The same argument that put the input source on the strip
    /// applies here in reverse: the fix is to delete the mode, not to label it.
    ///
    /// An unrecognised code holds the current orientation rather than guessing.
    /// The sensor emits a settling burst when it powers up, and a page that
    /// spun on that would be worse than one that ignored it.
    fn follow_device(&mut self, tilt: i32) -> Result<()> {
        let Some(want) = orientation::Orientation::from_tilt(tilt) else {
            return Ok(());
        };
        if want != self.window.orientation() {
            eprintln!("orientation: device turned, asking for {want:?}");
        }
        self.turn_to(want)
    }

    /// Turn the page, whatever asked for it — the accelerometer, or the writer
    /// on a Kindle that has none.
    ///
    /// One place, because the window manager has to be told the same way each
    /// time: the request goes in the window's name, the answer comes back as a
    /// resize, and the touch mapping has to move with it or every tap lands
    /// ninety degrees from the finger.
    fn turn_to(&mut self, want: orientation::Orientation) -> Result<()> {
        if want == self.window.orientation() {
            return Ok(());
        }
        self.window.set_orientation(want)?;
        self.touch_orientation = want;
        self.orientation_checked = std::time::Instant::now();
        // Only where the orientation is a setting. A Kindle that reads its own
        // position opens on that reading, and nothing there reads the file.
        if !self.turns_itself {
            write_orientation(want);
        }
        // The window manager answers with a resize, which the loop picks up.
        // Repaint anyway: if it declines, nothing else would.
        self.frame = None;
        self.paint()
    }

    /// Notice when the Bluetooth stack finished coming up, or that it did not.
    ///
    /// **The Keyboard section has to be told.** Config reads what the daemon
    /// remembers when it opens, and a writer who opens it in the seconds before
    /// the daemon answers would otherwise be looking at a page that says they
    /// have never paired anything.
    fn poll_bluetooth(&mut self) -> Result<()> {
        match self.bluetooth.poll_up() {
            Some(Ok(())) => {
                eprintln!("bluetooth: daemon up");
                self.refresh_paired();
                if matches!(self.mode, Mode::Config) {
                    self.paint()?;
                }
            }
            Some(Err(err)) => eprintln!("bluetooth: {err:#}"),
            None => {}
        }
        Ok(())
    }

    fn poll_orientation(&mut self) {
        if self.orientation_checked.elapsed() < ORIENTATION_POLL {
            return;
        }
        self.orientation_checked = std::time::Instant::now();
        let now = orientation::Orientation::detect();
        if now != self.touch_orientation {
            eprintln!(
                "orientation: framework moved {:?} -> {now:?}",
                self.touch_orientation
            );
            self.touch_orientation = now;
        }
    }

    /// The document as it should appear right now, with any preedit spliced in
    /// at the cursor, and where that preedit sits.
    ///
    /// **Every `Page` in the app is built from this and nothing else.** Once
    /// the preedit is in the text, display indices stop matching document
    /// indices, and a second place that laid out `doc.chars()` instead would
    /// disagree with the frame on screen about where every character after the
    /// cursor is. One buffer, one index space; [`Editor::document_index`] is
    /// the only way back.
    fn display(&self) -> (Vec<char>, Option<std::ops::Range<usize>>) {
        let mut chars = self.doc.chars();
        let composing = self.page_preedit();
        if composing.is_empty() {
            return (chars, None);
        }
        let at = self.doc.cursor().min(chars.len());
        let preedit: Vec<char> = composing.chars().collect();
        let span = at..at + preedit.len();
        chars.splice(at..at, preedit);
        (chars, Some(span))
    }

    /// Where a keystroke goes right now.
    ///
    /// Naming outranks the find bar for the same reason the key loop routes
    /// that way: a panel covers the page, and a bar underneath one is not
    /// reachable. They cannot in fact both be open — the find bar takes the
    /// strip, so there is no `New` button on it — but the order says which
    /// would win rather than leaving it to whichever test is written first.
    fn sink(&self) -> Sink {
        sink_for(
            matches!(self.mode, Mode::Naming { .. }),
            self.find.is_some(),
        )
    }

    /// The composition **as far as the page is concerned**: empty unless the
    /// page is what is being typed into.
    ///
    /// Everything that lays the document out asks here rather than reading
    /// `preedit` directly — the splice, the caret, the index conversion, the
    /// selection. A composition bound for the find bar must not appear in the
    /// prose, and must not shift the indices of the text after the cursor
    /// either: the hits are document indices, and a preedit that moved them
    /// would highlight the wrong characters while a word was being typed.
    fn page_preedit(&self) -> &str {
        page_composition(&self.preedit, self.sink())
    }

    /// Where the caret goes in display space: past the preedit, which is where
    /// the next keystroke will land.
    fn display_cursor(&self) -> usize {
        self.doc.cursor() + self.page_preedit().chars().count()
    }

    /// Turn a display index back into a document one.
    ///
    /// Anything at or after the preedit is that much further along in the
    /// display than it is in the document. The preedit's own characters belong
    /// to no document position, so they collapse onto the cursor.
    fn document_index(&self, display: usize) -> usize {
        document_index(
            display,
            self.doc.cursor(),
            self.page_preedit().chars().count(),
        )
    }

    /// The sentence to leave solid, or `None` while focus mode is off.
    /// Takes display indices, like everything else that reads a laid-out page —
    /// the sentence being composed into is the one around the preedit.
    fn focus_span(&self, chars: &[char]) -> Option<std::ops::Range<usize>> {
        self.focus
            .then(|| karyll_core::sentence_at(chars, self.display_cursor()))
    }

    /// Turn the page's focus on or off, and remember which.
    ///
    /// A full repaint rather than a damage rectangle: every row changes ink at
    /// once, so there is no smaller rectangle to find.
    fn toggle_focus(&mut self) -> Result<()> {
        self.focus = !self.focus;
        write_focus(self.focus);
        eprintln!("focus: {}", if self.focus { "on" } else { "off" });
        self.frame = None;
        self.paint()
    }

    /// Collect scan results while one is running. Called on every tick.
    fn poll_scan(&mut self) -> Result<()> {
        // Only while the panel that asked for it is open, or the poll goes on
        // repainting the panel over whatever the writer went back to.
        if !matches!(self.mode, Mode::Config) {
            self.scanning = None;
            return Ok(());
        }
        let Some(started) = self.scanning else {
            return Ok(());
        };
        // Once a second, not once a tick.
        let due = self
            .polled
            .is_none_or(|last| last.elapsed() >= std::time::Duration::from_secs(1));
        if !due {
            return Ok(());
        }
        self.polled = Some(std::time::Instant::now());
        let elapsed = started.elapsed().as_secs();
        let (devices, done) = match self.bluetooth.scan_results() {
            // Asked for but not begun. Waiting is the whole job here; calling it
            // finished would end the scan before the radio had done anything.
            Ok(hid::Scan::Starting) => {
                return if elapsed >= SCAN_SECONDS {
                    self.scanning = None;
                    self.show_status("The scan never started. Try again.")
                } else {
                    self.show_status(&format!("Starting the scan… {elapsed}s"))
                };
            }
            Ok(hid::Scan::Running(devices)) => (devices, elapsed >= SCAN_SECONDS),
            Ok(hid::Scan::Done(devices)) => (devices, true),
            Err(err) => {
                self.scanning = None;
                return self.show_status(&format!("Scan failed: {err:#}"));
            }
        };

        let changed = self.found != devices;
        if done {
            self.scanning = None;
        }
        if changed || done {
            self.found = devices;
            return self.paint();
        }
        self.show_status(&format!("Scanning… {elapsed}s"))
    }

    /// A keystroke the IME did not want, while a name is being typed. True once
    /// the name is settled.
    ///
    /// Reached only after [`Editor::compose_key`] has passed, so `Enter` here
    /// always means "this is the name" rather than "commit the word", and `Esc`
    /// always means "never mind" rather than "drop the syllable".
    fn typed_name(&mut self, action: &Action) -> Result<bool> {
        let Mode::Naming { for_new, name } = &mut self.mode else {
            return Ok(false);
        };
        let (for_new, mut name) = (*for_new, std::mem::take(name));

        match action {
            // `CommitTyped` as well, because that is what Shift+Enter is once
            // there is no composition to commit, and a name field should not
            // care which way the writer's little finger fell.
            Action::Newline | Action::CommitTyped => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    // Enter on an empty name still means "make me one" for a
                    // new document; it only cancels a rename, where there is
                    // nothing sensible to invent.
                    if for_new {
                        let path = new_document();
                        let _ = std::fs::write(&path, "");
                        self.open(path)?;
                    } else {
                        self.mode = Mode::Writing;
                        self.paint()?;
                    }
                    return Ok(true);
                }
                let path = PathBuf::from(format!("{DOCUMENTS}/{name}.md"));
                if for_new {
                    if !path.exists() {
                        let _ = std::fs::write(&path, "");
                    }
                    self.open(path)?;
                } else {
                    self.rename_to(path)?;
                }
                return Ok(true);
            }
            Action::Escape => {
                self.mode = Mode::Writing;
                self.paint()?;
                return Ok(true);
            }
            // The chord that asked for the name abandons it, the rule every
            // opening shortcut follows — see [`Editor::reopens`]. Only for a new
            // document: a rename is opened from the Files strip, so `N` is not
            // the key that opened it and closing on it would answer a shortcut
            // the writer never pressed.
            Action::NewDocument if for_new => {
                self.mode = Mode::Writing;
                self.paint()?;
                return Ok(true);
            }
            Action::Backspace => {
                name.pop();
            }
            // As in the find bar: with a CJK engine on, every letter goes to
            // the engine, so this is the only way back to a Latin filename.
            Action::CycleLanguage => {
                self.mode = Mode::Naming { for_new, name };
                self.cycle_language();
                self.paint()?;
                return Ok(false);
            }
            Action::Insert(c) if in_filename(*c) => name.push(*c),
            _ => {}
        }
        self.mode = Mode::Naming { for_new, name };
        self.paint()?;
        Ok(false)
    }

    /// Move the open document to `path`, keeping its contents.
    fn rename_to(&mut self, path: PathBuf) -> Result<()> {
        if let Some(old) = self.path.clone() {
            self.save()?;
            if old != path {
                let _ = std::fs::rename(&old, &path);
            }
        } else {
            let _ = std::fs::write(&path, self.doc.text());
        }
        self.path = Some(path);
        self.doc.mark_saved();
        self.mode = Mode::Writing;
        self.paint()
    }

    /// Re-read what the daemon has paired, and which one it is on.
    fn refresh_paired(&mut self) {
        self.paired = self.bluetooth.devices().unwrap_or_default();
        self.connected = self.bluetooth.connected();
    }

    /// Whether `device` is the keyboard being typed on.
    ///
    /// Both halves are needed. The daemon names the keyboard it has a link to,
    /// and the evdev node is what a keystroke actually arrives on — a link
    /// whose node has gone is not something to offer Disconnect for.
    fn is_connected(&self, device: &hid::Device) -> bool {
        self.keyboard_present
            && self
                .connected
                .as_deref()
                .is_some_and(|address| hid::same_address(address, &device.address))
    }

    /// Ask the daemon to drop the link, keeping the pairing.
    ///
    /// The node goes with it, and the session notices on the next tick and says
    /// so, which is what makes this safe to offer: without that, dropping the
    /// link would leave the app holding a dead descriptor for good.
    fn disconnect(&mut self, device: &hid::Device) -> Result<()> {
        self.show_status(&format!("Disconnecting {}…", device.name))?;
        let others = self.paired.len() > 1;
        match self.bluetooth.disconnect(device) {
            // What happens next is the whole of how a writer changes keyboard,
            // and nothing on the page says it: the daemon comes back in a few
            // seconds and takes whichever is awake.
            Ok(()) if others => self.show_status(&format!(
                "Dropped {}. Switch on the keyboard you want — it reconnects in a few seconds.",
                device.name
            )),
            Ok(()) => self.show_status(&format!(
                "Dropped {}. It reconnects in a few seconds.",
                device.name
            )),
            Err(err) => self.show_status(&format!("Could not disconnect: {err:#}")),
        }
    }

    /// Choose whether the Bluetooth stack outlives the editor.
    ///
    /// **Takes effect on the way out.** The daemon runs either way, and the
    /// keyboard being typed on is its keyboard — turning this off mid-session
    /// must not drop the link under the writer's hands.
    ///
    /// **The status line carries the cost.** It is paid on the home screen and
    /// in apps karyll has no part in, where nothing points back to this row.
    fn set_keep_bluetooth(&mut self, on: bool) -> Result<()> {
        if self.bluetooth.keep_alive() == on {
            return Ok(());
        }
        self.bluetooth.set_keep_alive(on);
        write_keep_bluetooth(on);
        eprintln!("bluetooth: keep alive {}", if on { "on" } else { "off" });
        self.show_status(if on {
            "Kept on — so is the keyboard. Audible and VoiceView have no radio until this is off."
        } else {
            "Off with karyll — the keyboard goes too, and Audible and VoiceView get the radio."
        })
    }

    /// Switch the colour panel on or off.
    ///
    /// **A full refresh, not a repaint.** The backing store still holds the
    /// bytes the old setting wrote, and a partial update over ink that is
    /// changing hue leaves a ghost of it.
    fn set_colour(&mut self, on: bool) -> Result<()> {
        if self.window.colour() == on {
            return Ok(());
        }
        self.window.set_colour(on);
        write_colour(on);
        eprintln!("window: colour {}", if on { "on" } else { "off" });
        self.frame = None;
        self.window.refresh()?;
        self.paint()
    }

    /// Take a colour for the caret or the highlighter.
    ///
    /// A full refresh for the reason [`Editor::set_colour`] takes one: the
    /// ink already on the panel is about to change hue under a partial update,
    /// which is what leaves a ghost of the old colour.
    fn set_colours(&mut self, inks: window::Inks) -> Result<()> {
        if inks.caret >= window::COLOURS.len() || inks.highlight >= window::COLOURS.len() {
            return Ok(());
        }
        if self.window.colours() == inks {
            return Ok(());
        }
        self.window.set_colours(inks);
        write_colours(inks);
        eprintln!("window: colours {inks}");
        self.frame = None;
        self.window.refresh()?;
        self.paint()
    }

    /// Remove a paired keyboard, so it can be paired afresh.
    fn forget(&mut self, device: &hid::Device) -> Result<()> {
        self.show_status(&format!("Forgetting {}…", device.name))?;
        if let Err(err) = self.bluetooth.remove(&device.address) {
            return self.show_status(&format!("Could not forget it: {err:#}"));
        }
        self.refresh_paired();
        self.show_status(&format!("Forgot {}.", device.name))
    }

    fn pair_with(&mut self, device: &hid::Device) -> Result<()> {
        self.show_status(&format!("Pairing with {}…", device.name))?;
        // Read the daemon's log from here on. A BLE keyboard has no display, so
        // the host shows a passkey and the writer types it on the keyboard —
        // and the daemon prints that passkey to its log and nowhere else. Every
        // attempt makes a fresh one, so start from this attempt's mark.
        let mark = self.bluetooth.log_mark();
        if let Err(err) = self.bluetooth.pair(device) {
            self.show_status(&format!("Could not pair: {err:#}"))?;
            return Ok(());
        }
        let mut asked = false;
        for _ in 0..PAIR_TICKS {
            std::thread::sleep(std::time::Duration::from_secs(1));
            // Repaint only when the passkey first appears: this is a panel on
            // an e-ink screen, and once a second would be a flashing mess.
            if !asked && let Some(hid::Prompt::Passkey(key)) = self.bluetooth.pair_prompt(mark) {
                self.show_status(&format!("Type {key} on the keyboard, then Enter."))?;
                asked = true;
            }
            match self.bluetooth.pair_done(&device.address) {
                Ok(None) => continue,
                Ok(Some(true)) => {
                    // The daemon takes a fresh pairing into run mode itself,
                    // and says it is done before the link is up.
                    // [`hid::Hid::connect`] on top of that drops it.
                    self.refresh_paired();
                    self.show_status("Paired. Start typing.")?;
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    // Out of Config, because there is nothing left to do here
                    // and a keyboard that works wants typing on. The strip
                    // changes from `[ Done ]` to the writing row as it goes,
                    // under a finger that may already be on its way to the
                    // corner — so the next tap there is spent looking.
                    self.mode = Mode::Writing;
                    self.strip_changed = true;
                    return self.paint();
                }
                Ok(Some(false)) | Err(_) => {
                    // Emphatically **not** removing the device here. A pair that
                    // reports failure may still have completed — and deleting it
                    // throws away a saved link key, so the next attempt starts
                    // from nothing instead of reconnecting.
                    let why = match self.bluetooth.pair_prompt(mark) {
                        Some(hid::Prompt::Failed(reason)) => reason,
                        // Nothing said: SMP simply ran out of its 30 seconds,
                        // which is what an unanswered passkey looks like.
                        _ if asked => "The code was not typed in time.".into(),
                        _ => "Put it in pairing mode and try again.".into(),
                    };
                    self.show_status(&format!("Pairing failed. {why}"))?;
                    return Ok(());
                }
            }
        }
        self.show_status("Pairing timed out.")
    }

    /// Repaint the current panel with `status` under its title.
    fn show_status(&mut self, status: &str) -> Result<()> {
        let layout = self.layout();
        let items = self.visible_items();
        let strip = self.strip_labels();
        let title = match self.mode {
            Mode::Files(_) => "Files",
            Mode::Config => "Config",
            Mode::Naming { for_new: true, .. } => "New document",
            Mode::Naming { for_new: false, .. } => "Rename",
            Mode::Help => "Help",
            Mode::Outline(_) => "Outline",
            Mode::Writing => "Karyll",
        };
        ui::Panel {
            title,
            status,
            items: &items,
            strip: &strip,
            overlay: overlay(
                candidate_page(&self.candidates, &self.pages, self.page),
                self.announcing,
                self.language,
            ),
        }
        .paint(&mut self.window, &mut self.fonts, layout)
    }

    fn apply(&mut self, action: Action) -> Result<()> {
        // Before anything else, because the arms below all *open* things: a
        // shortcut aimed at the surface already on screen closes it instead.
        if self.reopens(&action) {
            return self.close_reopened(&action);
        }
        // Every editing action comes through here, so this is the one place
        // that has to notice the document was touched. Cursor moves stamp it
        // too, which is harmless: autosave only fires when the document is
        // actually dirty.
        self.last_edit = Some(std::time::Instant::now());
        match action {
            Action::Insert(c) => self.doc.insert_char(c),
            Action::Newline => self.newline(),
            Action::Emphasis(marker) => self.emphasise(marker),
            Action::Heading(level) => self.set_heading(level),
            Action::Indent => self.doc.insert(INDENT),
            Action::Backspace => self.doc.backspace(),
            Action::Delete => self.doc.delete(),
            Action::DeleteWordBack => self.doc.delete_word_back(self.dict.as_ref()),
            Action::DeleteWordForward => self.doc.delete_word_forward(self.dict.as_ref()),
            Action::DeleteToLineStart => self.doc.delete_to_line_start(),
            Action::DeleteToLineEnd => self.doc.delete_to_line_end(),
            Action::Left => self.doc.move_left(),
            Action::Right => self.doc.move_right(),
            Action::LineStart => self.doc.move_to_line_start(),
            Action::LineEnd => self.doc.move_to_line_end(),
            Action::WordLeft => self.doc.move_word_left(self.dict.as_ref()),
            Action::WordRight => self.doc.move_word_right(self.dict.as_ref()),
            Action::DocStart => self.doc.move_to_start(),
            Action::DocEnd => self.doc.move_to_end(),
            Action::ExtendDocStart => self.doc.extend_to_start(),
            Action::ExtendDocEnd => self.doc.extend_to_end(),
            Action::ExtendLeft => self.doc.extend_left(),
            Action::ExtendRight => self.doc.extend_right(),
            Action::ExtendLineStart => self.doc.extend_to_line_start(),
            Action::ExtendLineEnd => self.doc.extend_to_line_end(),
            Action::ExtendWordLeft => self.doc.extend_word_left(self.dict.as_ref()),
            Action::ExtendWordRight => self.doc.extend_word_right(self.dict.as_ref()),
            Action::SelectAll => self.doc.select_all(),
            // Copying nothing leaves the clipboard alone rather than emptying
            // it: a stray Ctrl+C must not lose what was already in there.
            Action::Copy => {
                if let Some(text) = self.doc.selected_text() {
                    self.clipboard = text;
                }
            }
            Action::Cut => {
                if let Some(text) = self.doc.selected_text() {
                    self.clipboard = text;
                    self.doc.delete_selection();
                }
            }
            // One `Edit` for the whole string, so a paste undoes as one step —
            // and `insert` replaces any selection, which is what pasting over
            // a selection has to do.
            Action::Paste => self.doc.insert(&self.clipboard),
            Action::Undo => self.doc.undo(),
            Action::Redo => self.doc.redo(),
            Action::Save => self.save()?,
            Action::CycleLanguage => self.cycle_language(),
            // Only meaningful while composing, and that is handled before this
            // is reached. Writing prose, Escape does nothing.
            Action::Escape => {}
            // Vertical and page movement need the wrapped layout, which the
            // renderer owns. Until that seam exists they do nothing, rather
            // than approximating in a way the real behaviour would contradict.
            Action::Up => self.move_vertical(-1, false),
            Action::Down => self.move_vertical(1, false),
            Action::ExtendUp => self.move_vertical(-1, true),
            Action::ExtendDown => self.move_vertical(1, true),
            Action::PageUp => {
                let lines = self.page_lines();
                self.move_vertical(-lines, false);
            }
            Action::PageDown => {
                let lines = self.page_lines();
                self.move_vertical(lines, false);
            }
            Action::ToggleFocus => self.toggle_focus()?,
            Action::Find => self.open_find()?,
            Action::Replace => self.open_replace()?,
            // On the page this ticks a task off, which is where Obsidian and
            // Typora both put it. In the replace bar it carries out the
            // replacement, and [`Editor::typed_query`] takes it before this is
            // reached.
            Action::Change => self.toggle_task(),
            Action::ChangeAll => {}
            // Through the strip's own handlers, so a key and its button cannot
            // come to mean different things — the panels each have setting up
            // to do beyond assigning the mode.
            Action::Files => {
                self.strip_action(Bar::Files)?;
            }
            Action::Config => {
                self.strip_action(Bar::Config)?;
            }
            Action::Outline => {
                self.strip_action(Bar::Outline)?;
            }
            Action::NewDocument => self.start_naming(true)?,
            Action::Help => self.open_help()?,
            Action::Refresh => self.refresh_panel()?,
            Action::Resize(larger) => {
                self.set_size(render::step_size(self.theme.body_px, larger))?
            }
            // Only means anything mid-composition, where `compose_key` has
            // already taken it. Reaching here is Shift+Enter with nothing being
            // converted, which is a line break like any other.
            Action::CommitTyped => self.apply(Action::Newline)?,
            Action::Quit => {}
        }
        // Any movement that is not vertical abandons the column the arrow keys
        // were holding, so the next Up or Down takes its column from here.
        if !matches!(
            action,
            Action::Up
                | Action::Down
                | Action::ExtendUp
                | Action::ExtendDown
                | Action::PageUp
                | Action::PageDown
        ) {
            self.goal = None;
        }
        Ok(())
    }

    fn page_lines(&mut self) -> i32 {
        let roles = std::mem::take(&mut self.roles);
        let lines =
            render::lines_per_page(&mut self.fonts, &self.theme, &roles, self.window.height());
        self.roles = roles;
        lines
    }

    /// Move to the next input source. Ctrl+Space and the language button are
    /// the same action, so they cannot disagree about what is selected.
    fn cycle_language(&mut self) {
        self.set_language(self.language.next(&self.enabled));
        // Say which one, beside the caret. The strip is hidden while writing,
        // so nothing else answers it and `Ctrl+Space` would cycle blind.
        self.announcing = true;
    }

    /// Select an input source: its keyboard, its input method, and the regional
    /// convention its Han faces follow.
    ///
    /// Each engine is loaded the first time it is asked for rather than at
    /// startup, because bringing a plugin up maps its dictionaries — 1.4 MB for
    /// Chinese, some 17 MB for Japanese — and a session that only writes German
    /// should not pay for either. A failure is reported and the source is left
    /// selected without an engine: an editor that refused to switch language
    /// because a plugin was missing would be a worse editor than one that types
    /// Latin letters.
    /// Take up the language the last session ended in.
    ///
    /// Through [`Editor::set_language`], because a language is not a label — it
    /// is an IME, a keyboard layout and a set of Han faces, and only that
    /// function applies all four. Assigning the field at construction restored
    /// the button and nothing behind it: the strip read 日本語 while the keys
    /// typed English, and the next cycle stepped on from the name rather than
    /// from what was actually selected.
    ///
    /// The engine loads here for a session resuming into CJK, which is the
    /// point — a session that resumes into English still loads none.
    fn resume_language(mut self, language: Language) -> Self {
        self.set_language(language);
        self
    }

    /// Load the word list the Han faces are set for, unless it is already the
    /// one loaded.
    ///
    /// **It follows the faces rather than the keyboard**, which puts a list
    /// behind Han from the first tap even when the writer never leaves the
    /// English keyboard — they may be editing Chinese they typed yesterday.
    /// A language that names no convention leaves both alone.
    fn set_lexicon(&mut self) {
        let region = self.fonts.region();
        if self.dict_region == Some(region) {
            return;
        }
        self.dict = lexicon::load(region);
        self.dict_region = self.dict.is_some().then_some(region);
    }

    fn set_language(&mut self, language: Language) {
        self.abandon_composition();
        self.language = language;
        write_language(language);

        if let Some(region) = language.region() {
            self.fonts.set_region(region);
        }
        self.set_lexicon();

        self.cjk = match language.script() {
            Some(script) => self.load_engine(script),
            None => false,
        };
        if let Some(engine) = self.engine() {
            engine.set_traditional(language.traditional());
        }
        eprintln!(
            "language: {} ({} keyboard)",
            language.label(),
            language.layout().name()
        );
    }

    /// Make sure `script`'s engine is loaded, and say whether it is usable.
    fn load_engine(&mut self, script: ime::Script) -> bool {
        if self.engines.iter().any(|(s, _)| *s == script) {
            return true;
        }
        let loaded: Result<Box<dyn ime::Ime>, String> = match script {
            ime::Script::Chinese => ime::Chinese::open().map(|e| Box::new(e) as Box<dyn ime::Ime>),
            ime::Script::Japanese => {
                ime::Japanese::open().map(|e| Box::new(e) as Box<dyn ime::Ime>)
            }
        };
        match loaded {
            Ok(engine) => {
                eprintln!("ime: {script:?} engine loaded");
                self.engines.push((script, engine));
                true
            }
            Err(err) => {
                eprintln!("ime: {err}");
                false
            }
        }
    }

    /// The engine for the language now selected, if it has one and it loaded.
    fn engine(&mut self) -> Option<&mut Box<dyn ime::Ime>> {
        let script = self.language.script()?;
        self.engines
            .iter_mut()
            .find(|(s, _)| *s == script)
            .map(|(_, e)| e)
    }

    /// Offer a keystroke to Chinese input. Returns whether it was consumed.
    ///
    /// Every rule about *what* a key means lives in [`ime::compose`], which is
    /// pure and tested. This is only the part that needs the engine and the
    /// document.
    fn compose_key(&mut self, action: &Action) -> bool {
        let Some(script) = self.language.script() else {
            return false;
        };
        if !self.cjk {
            return false;
        }
        // Asked of [`Editor::composing`] rather than of `typed`, so that one
        // answer drives the rules, the bar and the hit-testing together. They
        // can disagree: Japanese swallows a space as a conversion request, so
        // `typed` grows a character the engine's own composition does not have,
        // and a backspace then empties `typed` while the engine is still
        // holding kana. Keying the rules off `typed` would stop composing with
        // a composition still on screen.
        match ime::compose(action, self.composing(), script) {
            ime::Compose::Pass => return false,
            // Backspace is fed to the engine, which drops one unit and
            // re-predicts, so what is shown follows rather than leads.
            ime::Compose::Feed('\u{8}') => {
                self.typed.pop();
                self.feed('\u{8}');
                if self.typed.is_empty() {
                    self.set_candidates(Vec::new());
                }
            }
            ime::Compose::Feed(c) => {
                self.typed.push(c);
                self.feed(c);
            }
            ime::Compose::Select(n) => self.select_candidate(n),
            // With no bar on screen there is nothing to page, and an arrow
            // means there what it means everywhere else: leave the half-typed
            // word behind. Japanese passes through this at the start of every
            // syllable, where the letters are not yet kana.
            ime::Compose::NextPage | ime::Compose::PreviousPage if self.candidates.is_empty() => {
                self.abandon_composition()
            }
            // Consumed at either end of the list whether or not the page moves.
            // Letting the arrow through from the last page would move the
            // cursor out from under a word that is still being written.
            ime::Compose::NextPage => {
                if self.page + 1 < self.pages.len() {
                    self.page += 1;
                }
            }
            ime::Compose::PreviousPage => self.page = self.page.saturating_sub(1),
            // A capital ends the word being composed and then lands on the
            // page itself, the same shape as punctuation: typing `中国NASA`
            // should not need the mode switched off and back.
            ime::Compose::Latin(c) => {
                self.commit_composition(script);
                self.insert_committed(&c.to_string());
            }
            // The letters as struck, rather than the kana they turned into.
            ime::Compose::CommitTyped => {
                let text = std::mem::take(&mut self.typed);
                self.abandon_composition();
                self.insert_committed(&text);
            }
            // Punctuation ends the word being composed and then adds the mark.
            // Typing "nihao," should give 你好， without a separate keystroke
            // to accept 你好.
            ime::Compose::Punctuate(key) => {
                self.commit_composition(script);
                if let Some(text) = self.punctuation.resolve(script, key) {
                    self.insert_committed(text);
                }
            }
            // What is composed, as it stands: pinyin for an English word that
            // did not need converting, kana for Japanese that is meant to stay
            // kana. Nothing is fed to the engine, so it is told to reset.
            ime::Compose::CommitRaw => {
                let text = std::mem::take(&mut self.preedit);
                self.abandon_composition();
                self.insert_committed(&text);
            }
            ime::Compose::Cancel => self.abandon_composition(),
        }
        true
    }

    /// Send one key to the engine, and take back both of the things it changes:
    /// the candidate list, and the composition as the engine now reads it.
    ///
    /// The composition is asked for rather than assumed, because for Japanese
    /// it is not what was typed — `nihon` composes にほん — and only the engine
    /// holds the transliteration. An engine that keeps no composition of its
    /// own says so, and then the keys as typed are the composition, which is
    /// exactly right for pinyin.
    fn feed(&mut self, key: char) {
        let Some(engine) = self.engine() else {
            return;
        };
        let candidates = engine.key(key);
        let composed = engine.preedit();
        self.preedit = composed.unwrap_or_else(|| self.typed.clone());
        self.set_candidates(candidates);
    }

    /// Take a new list of candidates, and work out how it pages.
    ///
    /// **Everything that changes the list comes through here**, because how it
    /// pages is not a property of the list: it is what the panel can hold at
    /// the size being written in, and a list set without asking would be paged
    /// by whatever the last one needed. Drawing, tapping and the number row all
    /// read the answer, so there is one.
    fn set_candidates(&mut self, candidates: Vec<String>) {
        self.candidates = candidates;
        self.pages = ui::candidate_pages(
            &mut self.fonts,
            self.window.width(),
            self.theme.body_px,
            &self.candidates,
            ime::WANTED,
        );
        // A new list is read from its first page. Holding the old page would
        // leave the bar empty on the keystroke that shortened the list.
        self.page = 0;
    }

    /// Accept a candidate by its place on the bar, from the number row or a tap.
    ///
    /// Out of range does nothing rather than committing something else: the
    /// engine offers fewer than a page of candidates often, and pressing 7 for
    /// a list of three should not insert the third.
    ///
    /// **`n` is a place on the bar, so it is looked up on the bar.** Going to
    /// the list directly would let a digit past the end of a short page commit
    /// the first candidate of the next one — a word the writer cannot see.
    fn select_candidate(&mut self, n: usize) {
        let from = self.pages.get(self.page).copied().unwrap_or(0);
        let Some(text) = candidate_page(&self.candidates, &self.pages, self.page)
            .get(n)
            .cloned()
        else {
            return;
        };
        let at = from + n;
        match self.engine().and_then(|engine| engine.commit(at)) {
            // **The word is not over.** The candidate converted the front of
            // the reading and the engine is composing the rest, so the bar goes
            // on carrying it and the next keystroke belongs to it. Ending here
            // would strand that reading in the engine, where it would splice
            // itself onto the front of the next word typed.
            Some(rest) => {
                self.typed.clone_from(&rest.reading);
                self.preedit = rest.reading;
                self.set_candidates(rest.candidates);
            }
            None => {
                self.typed.clear();
                self.preedit.clear();
                self.set_candidates(Vec::new());
            }
        }
        self.insert_committed(&text);
    }

    /// Finish the word under way, the way this language finishes one.
    ///
    /// The two differ, and it is a difference in the languages rather than in
    /// the plugins:
    ///
    /// * **Chinese takes the best candidate**, and takes it again for as long
    ///   as one covers only the front of the reading. Pinyin predicts as it
    ///   goes, so the top candidate is what the writer has been watching the
    ///   whole time and is what they mean by typing on — but a word is finished
    ///   only when the whole reading is converted.
    /// * **Japanese takes the composition itself**, because that is already the
    ///   answer either way: the engine's composition getter returns the raw
    ///   kana while nothing is selected, and the selected candidate once space
    ///   has asked for a conversion. Taking candidate 0 instead would convert
    ///   words the writer meant to leave as kana — ここ becoming 個々 — which
    ///   is a wrong word rather than a missing one.
    fn commit_composition(&mut self, script: ime::Script) {
        if !self.composing() {
            return;
        }
        if script == ime::Script::Chinese {
            // Bounded by the reading, which every pass shortens by at least a
            // syllable. An engine that handed back what it was given would
            // otherwise spin here.
            for _ in 0..=self.typed.chars().count() {
                if self.candidates.is_empty() {
                    break;
                }
                self.select_candidate(0);
            }
            if !self.composing() {
                return;
            }
        }
        // Whatever is left when there is nothing to convert it with: pinyin the
        // dictionary does not have, or kana that is meant to stay kana.
        let text = std::mem::take(&mut self.preedit);
        self.abandon_composition();
        self.insert_committed(&text);
    }

    /// Put committed text where it is being typed, as one undo step.
    ///
    /// **Every path out of the IME ends here** — a chosen candidate, a
    /// converted word, raw pinyin, CJK punctuation, a capital letter typed
    /// mid-composition — so this is the only place that has to know which field
    /// is taking text. See [`Sink`].
    fn insert_committed(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.sink() {
            Sink::Page => {
                self.doc.insert(text);
                self.last_edit = Some(std::time::Instant::now());
            }
            // Whichever of the bar's fields is taking keys, so a writer can say
            // what to look for *and* what to put in its place in Chinese.
            Sink::Find => {
                let text = text.to_string();
                if self.edit_field(|field| field.push_str(&text)) {
                    self.research();
                }
            }
            Sink::Name => {
                if let Mode::Naming { name, .. } = &mut self.mode {
                    name.extend(text.chars().filter(|c| in_filename(*c)));
                }
            }
        }
    }

    /// Throw away the half-typed word, in the engine as well as here. Leaving
    /// the engine holding symbols would make the next word start mid-syllable.
    fn abandon_composition(&mut self) {
        if let Some(engine) = self.engine() {
            engine.clear();
        }
        self.typed.clear();
        self.preedit.clear();
        self.set_candidates(Vec::new());
    }

    /// Whether a word is being composed, which is what swaps the action strip
    /// for the candidate bar.
    ///
    /// A composition with no candidates still counts, and has to: the letters
    /// typed towards a word never reach the document, so if the bar is not
    /// showing them they are invisible. Chinese reaches that state on a
    /// syllable the dictionary does not have; Japanese passes through it at the
    /// start of every word, because the first letters of a romaji syllable are
    /// not yet kana and there is nothing to convert.
    fn composing(&self) -> bool {
        !self.preedit.is_empty() || !self.candidates.is_empty()
    }

    fn save(&mut self) -> Result<()> {
        self.write_document("saved")
    }

    /// Write the document out. `why` names the reason in the log, so an
    /// autosave and a deliberate save are told apart when reading `karyll.log`
    /// after something went wrong.
    fn write_document(&mut self, why: &str) -> Result<()> {
        let Some(path) = &self.path else {
            eprintln!("{why}: no file given, nothing written");
            return Ok(());
        };
        std::fs::write(path, self.doc.text())
            .with_context(|| format!("write {}", path.display()))?;
        self.doc.mark_saved();
        self.dirty_since = None;
        eprintln!("{why} {}", path.display());
        // Riding along with every save and autosave keeps the remembered place
        // close to current without writing a file on every keystroke.
        self.remember_position();
        Ok(())
    }

    /// Write the document out on its own, so a crash cannot cost prose.
    ///
    /// This exists because CJK input runs Amazon's closed predictor plugin
    /// inside this process, and a fault in foreign code would otherwise take
    /// the unsaved document with it. It is not specific to that: an editor
    /// should survive its own crashes too, whatever caused them.
    ///
    /// Idle-triggered rather than periodic, because writing mid-word is what
    /// makes autosave feel intrusive, and because a pause is when the file
    /// system is otherwise quiet. [`AUTOSAVE_MAX`] is the backstop for someone
    /// who types continuously and never pauses.
    fn poll_autosave(&mut self) {
        if !self.doc.is_dirty() {
            self.dirty_since = None;
            return;
        }
        let now = std::time::Instant::now();
        let since = *self.dirty_since.get_or_insert(now);
        let idle_for = self.last_edit.map(|last| now.duration_since(last));
        if !autosave_due(idle_for, now.duration_since(since)) {
            return;
        }
        // A failed autosave must not end the session — the document is still
        // on screen and Ctrl+S can still be tried. Say so and carry on.
        if let Err(err) = self.write_document("autosaved") {
            eprintln!("autosave failed: {err:#}");
            // Back off a full interval rather than retrying every tick against
            // a full or read-only filesystem.
            self.dirty_since = Some(now);
            self.last_edit = Some(now);
        }
    }

    fn paint(&mut self) -> Result<()> {
        if !matches!(self.mode, Mode::Writing) {
            // A panel covers the page, so what was last drawn no longer
            // describes the screen; dropping it forces a full repaint when the
            // document comes back.
            self.frame = None;
            let status = match &self.mode {
                // The composition counts as typing: a writer three keystrokes
                // into 日本語 has not typed a name yet as far as `name` is
                // concerned, and telling them to type one would be answering a
                // question they are in the middle of.
                Mode::Naming { name, .. } if name.is_empty() && self.preedit.is_empty() => {
                    "Type a name, then Enter. Esc cancels.".to_string()
                }
                Mode::Naming { name, .. } => format!("{name}{}.md", self.preedit),
                Mode::Files(files) => {
                    let total = files.len();
                    let window = self.page_window();
                    match total {
                        0 => format!("Nothing in {DOCUMENTS} yet."),
                        1 => format!("1 document in {DOCUMENTS}"),
                        // Which of them are on screen, once they stop all
                        // fitting — otherwise `More` is a button with nothing
                        // saying what it will bring.
                        n if n > window.len() => format!(
                            "{}–{} of {n} documents in {DOCUMENTS}",
                            window.start + 1,
                            window.end.min(n)
                        ),
                        n => format!("{n} documents in {DOCUMENTS}"),
                    }
                }
                // Its own arm, or it falls through to whatever the last one
                // says. There is no Save on this page and nothing to confirm,
                // so that is what it says.
                Mode::Config => "Changes apply as you tap.".to_string(),
                // The two keys that are not in the list below, because a list
                // of what the keys do cannot name the key that opened it or the
                // key that closes it — by then it is too late to read either.
                Mode::Help => "Ctrl/⌘ + H opens this page.  Esc closes it.".to_string(),
                // What a tap will do, and — once the list is longer than the
                // page — which part of it is on screen, the same report the
                // Files panel gives.
                Mode::Outline(sections) => {
                    let total = sections.len();
                    let window = self.page_window();
                    match total {
                        0 => "Ctrl/⌘ + 1 … 6 makes a heading.".to_string(),
                        n if n > window.len() => format!(
                            "{}–{} of {n} sections. Tap one to go there.",
                            window.start + 1,
                            window.end.min(n)
                        ),
                        1 => "1 section. Tap it to go there.".to_string(),
                        n => format!("{n} sections. Tap one to go there."),
                    }
                }
                Mode::Writing => String::new(),
            };
            return self.show_status(&status);
        }
        // A panel covered the page, so the next paint starts from scratch —
        // and that is also exactly when the bar needs drawing again.
        let fresh = self.frame.is_none();
        let (chars, preedit) = self.display();
        let markup = karyll_core::markdown::analyze(&chars);
        // The page reaches the foot of the panel while the chrome is away, so
        // both the text and the centring measure against that rather than
        // against a strip that is not there.
        let bottom = self.page_bottom();
        // Before the page borrows the theme, and it needs the faces to measure
        // the strip it hangs off.
        let anchor = self.overlay_anchor();
        let page = render::Page::new(
            &chars,
            &markup,
            &self.theme,
            (self.window.width(), self.window.height()),
            bottom,
        )
        .focused_on(self.focus_span(&chars))
        .composing(preedit);
        // Kept so page movement measures the same row the page is drawn with.
        // A document with Han in it has taller rows, and paging by Latin rows
        // in one would step past a line every screen.
        self.roles = page.roles.clone();
        let mut lines = render::layout(&page, &mut self.fonts, self.theme.margin_y as i32);
        // Normal writing lets the caret run to the foot of the page; focus mode
        // holds the sentence in the middle of it. One behaviour for both makes
        // ordinary writing read as a half-applied focus mode — see
        // `render::Scroll`.
        let how = render::scroll_mode(
            self.focus,
            self.find.is_some(),
            // Taken, not read: this placement belongs to the arrival, and the
            // next paint follows the cursor like any other.
            std::mem::take(&mut self.landing),
            self.theme.margin_y as i32,
            bottom as i32,
            self.window.height() as i32,
        );
        // Display indices throughout: the caret belongs past the preedit,
        // which is where the next keystroke lands.
        let cursor = self.display_cursor();
        self.scroll = render::scroll_for(&lines, cursor, self.scroll, how);
        render::shift(&mut lines, self.scroll);
        // A selection cannot survive a composition *in the page* — typing over
        // one replaces it — so there is nothing here to translate into display
        // space. A composition bound for the find bar is a different matter:
        // the selection is the hit being searched for, and it has to stay
        // inverted while the next word is typed into the bar.
        let selection = self
            .page_preedit()
            .is_empty()
            .then(|| self.doc.selection())
            .flatten();
        let editing = render::Editing {
            cursor,
            selection,
            overlay: overlay(
                candidate_page(&self.candidates, &self.pages, self.page),
                self.announcing,
                self.language,
            ),
            anchor,
        };
        self.frame = Some(render::paint(
            &mut self.window,
            &mut self.fonts,
            &page,
            lines,
            &editing,
            self.frame.as_ref(),
        )?);
        // The strip is chrome: always present, and always the way out. It only
        // needs drawing when something covered it — a panel, or the first
        // paint. Typing damages the page above it, so redrawing per keystroke
        // would throw away the damage rectangle just computed.
        //
        // While composing it carries the candidates instead, and then it does
        // change on every keystroke, because that is the whole point of it.
        //
        // Redrawn when what it would say differs from what is on it, the same
        // rule the page uses. A flag for "was showing candidates" gets it wrong
        // the other way: switching language repaints nothing, and the button
        // goes on naming the previous one until something else forces a paint.
        //
        // While the chrome is away there is nothing to draw and nothing to
        // remember drawing: the page has already been painted over those rows,
        // and `strip_drawn` is cleared so that bringing the strip back does
        // not compare equal and skip itself.
        if !self.strip_visible() {
            self.strip_drawn.clear();
            self.status_drawn.clear();
            return Ok(());
        }
        let cells = self.strip_cells();
        // The status is compared alongside the buttons because it shares their
        // band and their repaint. It is also the half that actually changes: the
        // buttons say the same three words all session, while this counts the
        // words and reports the save. Comparing only the cells would leave a
        // count from before the last paragraph on screen — and, worse, leave it
        // reading `not yet saved` after the autosave had run, which is the one
        // thing this line exists to say.
        let status = self.status_line();
        if fresh || cells != self.strip_drawn || status != self.status_drawn {
            let layout = self.layout();
            let stretch = self.strip_stretch();
            ui::paint_cells(
                &mut self.window,
                &mut self.fonts,
                layout,
                &cells,
                &stretch,
                &status,
            );
            self.strip_drawn = cells;
            self.status_drawn = status;
            self.window
                .present(layout.strip_rect(self.window.width()))?;
        }
        Ok(())
    }

    /// What the candidate box hangs off, or `None` for the caret.
    ///
    /// The find bar's own cell, when that is where the typing is going. The
    /// caret is over at the last match by then, and hanging the choices there
    /// would put them beside a word the writer is not typing — the same
    /// disorientation the box exists to avoid.
    fn overlay_anchor(&mut self) -> Option<window::Rect> {
        if self.sink() != Sink::Find {
            return None;
        }
        // The field taking the keys, found by what it *is* rather than by where
        // the bar happens to put it.
        let field = self.find.as_ref().map(|f| f.field).unwrap_or_default();
        let cell = self
            .strip_fitted()
            .0
            .iter()
            .position(|bar| *bar == field.cell())
            .unwrap_or(0);
        let width = self.window.width();
        let layout = self.layout();
        let cells = self.strip_cells();
        let stretch = self.strip_stretch();
        let fonts = &mut self.fonts;
        // Measured from the cells actually drawn, not from a second guess at
        // the geometry: the box has to sit over the field it belongs to.
        let bounds = ui::cell_bounds(width, &cells, &stretch, |s| {
            ui::measure(fonts, s, ui::TEXT_PX) as u16
        });
        Some(ui::strip_cell_rect(layout, &bounds, cell))
    }

    /// Move the cursor by whole visual lines, dragging the selection along if
    /// `extend`.
    ///
    /// Needs the wrapped layout, so it works off the last frame — which is the
    /// same thing the reader is looking at.
    fn move_vertical(&mut self, delta: i32, extend: bool) {
        let Some(frame) = self.frame.take() else {
            return;
        };
        // The same buffer the frame was laid out from, or the rows measured
        // here would be a different shape from the rows on screen.
        let (chars, preedit) = self.display();
        let markup = karyll_core::markdown::analyze(&chars);
        let bottom = self.page_bottom();
        let page = render::Page::new(
            &chars,
            &markup,
            &self.theme,
            (self.window.width(), self.window.height()),
            bottom,
        )
        .focused_on(self.focus_span(&chars))
        .composing(preedit);
        let from = self.display_cursor();
        if let Some((cursor, goal)) =
            render::move_vertical(&page, &mut self.fonts, &frame, from, delta, self.goal)
        {
            let cursor = self.document_index(cursor);
            if extend {
                self.doc.extend_to(cursor);
            } else {
                self.doc.set_cursor(cursor);
            }
            // Set after moving: `set_cursor` is what clears the column, and a
            // run of vertical moves has to keep it.
            self.goal = Some(goal);
        }
        self.frame = Some(frame);
    }
}

/// Scan for Bluetooth keyboards and pair with one.
///
/// Needs a terminal — kterm or ssh — because it prints a list and reads a
/// choice. Once a keyboard is paired the link key is kept, so this is a
/// one-time step per keyboard and the editor never needs it again.
///
/// karyll drives this itself rather than deferring to the Bluetooth stack's own
/// interactive mode, so there is one way in and one place the behaviour lives.
fn pair() -> Result<()> {
    let mut bluetooth = hid::Hid::beside_executable()?;
    // The same setting the editor reads, so pairing from a terminal leaves the
    // stack the way the writer has asked for it.
    bluetooth.set_keep_alive(read_keep_bluetooth());
    if bluetooth.keep_alive() {
        eprintln!("Starting the Bluetooth stack. Config keeps it running after this exits.");
    } else {
        eprintln!("Starting the Bluetooth stack. This displaces the stock one until karyll exits.");
    }
    bluetooth.start()?;

    let known = bluetooth.devices().unwrap_or_default();
    if !known.is_empty() {
        eprintln!("\nAlready paired:");
        for device in &known {
            eprintln!("  {} [{}] {}", device.address, device.protocol, device.name);
        }
    }

    eprintln!("\nScanning. Put the keyboard into pairing mode now.");
    bluetooth.scan()?;

    let mut found = Vec::new();
    for _ in 0..SCAN_SECONDS {
        std::thread::sleep(std::time::Duration::from_secs(1));
        match bluetooth.scan_results()? {
            hid::Scan::Starting => continue,
            hid::Scan::Running(devices) => {
                found = devices;
                eprint!("\r  {} found…    ", found.len());
            }
            hid::Scan::Done(devices) => {
                found = devices;
                break;
            }
        }
    }
    eprintln!();

    if found.is_empty() {
        bail!("nothing found — is the keyboard in pairing mode?");
    }
    for (i, device) in found.iter().enumerate() {
        println!(
            "  {:>2}. {} [{}] {}",
            i + 1,
            device.name,
            device.protocol,
            device.address
        );
    }

    print!("\nPair with which? (number, or blank to cancel): ");
    std::io::Write::flush(&mut std::io::stdout()).ok();
    let mut choice = String::new();
    std::io::stdin().read_line(&mut choice)?;
    let Some(device) = choice
        .trim()
        .parse::<usize>()
        .ok()
        .and_then(|n| found.get(n - 1))
    else {
        eprintln!("Cancelled.");
        return Ok(());
    };

    eprintln!("Pairing with {}…", device.name);
    let mark = bluetooth.log_mark();
    bluetooth.pair(device)?;
    let mut asked = false;
    for _ in 0..PAIR_TICKS {
        std::thread::sleep(std::time::Duration::from_secs(1));
        // A BLE keyboard types the passkey in; the host is the side that shows
        // it, and the daemon only ever writes it to its log.
        if !asked && let Some(hid::Prompt::Passkey(key)) = bluetooth.pair_prompt(mark) {
            eprintln!("Type {key} on the keyboard, then press Enter.");
            asked = true;
        }
        match bluetooth.pair_done(&device.address)? {
            None => continue,
            Some(true) => {
                // The daemon takes a fresh pairing into run mode itself, and
                // says it is done before the link is up. Nothing else is asked
                // of it here.
                eprintln!("Paired. Tap the karyll tile; it brings the keyboard up itself.");
                return Ok(());
            }
            Some(false) => match bluetooth.pair_prompt(mark) {
                Some(hid::Prompt::Failed(reason)) => bail!("pairing failed — {reason}"),
                _ if asked => bail!("pairing failed — the code was not typed in time"),
                _ => bail!("pairing failed — try again with the keyboard in pairing mode"),
            },
        }
    }
    bail!("pairing did not finish in time")
}

/// The daemon scans for a fixed **10 seconds** (`controller._do_scan`), BLE and
/// Classic concurrently. This is only the safety net for a scan that never
/// reports itself finished — the UI follows the daemon's own `scanning` flag,
/// so a scan normally ends as soon as it is actually over.
const SCAN_SECONDS: u64 = 14;
/// Seconds to wait for pairing to settle. Longer than the SMP pairing timeout
/// of 30 seconds plus the few the daemon spends suspending, powering the chip
/// and connecting before it starts — otherwise this gives up first and reports
/// a timeout of its own instead of the daemon's actual reason.
const PAIR_TICKS: usize = 45;

/// How often the loop wakes when nothing has happened. A held finger produces
/// no events, so the long press needs a tick to be noticed; the same tick looks
/// for a keyboard that has not arrived yet.
const TICK_MS: i32 = 200;

/// How long a pause in typing before the document is written by itself. Long
/// enough not to fire between words, short enough that a crash costs a sentence
/// at most.
const AUTOSAVE_IDLE: std::time::Duration = std::time::Duration::from_secs(3);
/// The longest a change is ever left unwritten, however continuously the writer
/// types. Without this a fast writer who never pauses would never autosave.
const AUTOSAVE_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// Is an autosave due, given how long since the last keystroke and how long the
/// document has been unsaved?
///
/// `idle_for` is `None` before anything has been typed this session — a
/// document that arrived dirty is written at the first opportunity rather than
/// waiting for a keystroke that may never come.
///
/// Pure, so the timing is tested without a window, a keyboard or a filesystem.
fn autosave_due(idle_for: Option<std::time::Duration>, dirty_for: std::time::Duration) -> bool {
    idle_for.is_none_or(|idle| idle >= AUTOSAVE_IDLE) || dirty_for >= AUTOSAVE_MAX
}

/// The orientation a Kindle with no sensor was last set to, beside the logs so
/// it survives an update.
fn orientation_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/orientation")
}

/// Which way up to open.
///
/// **Where there is a sensor, the way the device is being held wins, and
/// nothing is remembered.** Sessions are minutes apart and the device is put
/// down between them, so the orientation the last one ended in says nothing
/// about the one starting: a writer who reaches for a Kindle lying flat on a
/// stand and gets a portrait page has to turn it a full ninety degrees the
/// wrong way and back before the page follows. The sensor already knows, and
/// [`evdev::Accelerometer::position`] asks it without waiting for a movement
/// that may not come.
///
/// The framework's own orientation is the answer when the sensor gives none —
/// lying flat, or nothing reported since boot. It has been following this same
/// sensor all along, so it holds the last position that did name an
/// orientation, which is the best available reading of a device that is not
/// currently telling anyone which way up it is.
///
/// Only a Kindle with no sensor remembers, because there the orientation is a
/// setting a writer chose rather than a fact about the room, and forgetting it
/// would mean choosing it again every session.
fn read_orientation(accel: Option<&evdev::Accelerometer>) -> orientation::Orientation {
    let (orientation, from) = match accel {
        Some(accel) => match accel
            .position()
            .and_then(orientation::Orientation::from_tilt)
        {
            Some(held) => (held, "held"),
            None => (orientation::Orientation::detect(), "flat, asked winmgr"),
        },
        None => match std::fs::read_to_string(orientation_file()) {
            Ok(letter) => (
                orientation::Orientation::from_letter(letter.trim()),
                "remembered",
            ),
            Err(_) => (orientation::Orientation::detect(), "asked winmgr"),
        },
    };
    eprintln!("orientation: {orientation:?} ({from})");
    orientation
}

fn write_orientation(orientation: orientation::Orientation) {
    let _ = std::fs::write(orientation_file(), orientation.letter().to_string());
}

/// The two ways up a writer asks for, and which way that is to the window
/// manager.
///
/// **Only on a Kindle with no accelerometer**, which is where they are the only
/// way to turn the page over: the Colorsoft reports no tilt from any node in
/// `/proc/bus/input/devices`, so nothing there would ever rotate on its own and
/// a landscape page would be unreachable. Where there is a sensor, turning the
/// device is the control and a settings row would be a second one that argues
/// with it.
///
/// Two rather than four: upside-down portrait and the other landscape are what
/// a sensor gives for free, and neither is a thing a writer sets down a Kindle
/// to go and choose.
const SCREENS: [(&str, orientation::Orientation); 2] = [
    ("Portrait", orientation::Orientation::Up),
    ("Landscape", orientation::Orientation::Right),
];

/// The selected input source, remembered for the same reason as the layout: a
/// writer who left in Chinese comes back to Chinese.
fn language_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/language")
}

/// The candidates on the bar: the page the writer has paged to.
///
/// The pages come from [`ui::candidate_pages`], which is what knows how many
/// fit. Free of the editor for the same reason [`overlay`] is.
fn candidate_page<'a>(candidates: &'a [String], pages: &[usize], page: usize) -> &'a [String] {
    let Some(&from) = pages.get(page) else {
        return &[];
    };
    let to = pages.get(page + 1).copied().unwrap_or(candidates.len());
    &candidates[from.min(candidates.len())..to.min(candidates.len())]
}

/// What floats beside the caret right now.
///
/// Candidates outrank the language notice, though the two cannot really
/// collide: switching language abandons any composition.
///
/// Takes the three things it reads rather than the editor, so that borrowing it
/// does not borrow the window and the faces the same paint is about to write to.
fn overlay(candidates: &[String], announcing: bool, language: Language) -> ui::Overlay<'_> {
    if !candidates.is_empty() {
        ui::Overlay::Candidates(candidates)
    } else if announcing {
        ui::Overlay::Notice(language.label())
    } else {
        ui::Overlay::None
    }
}

/// Which field a keystroke goes to. Free of the editor so the precedence can be
/// stated once and checked without a window.
fn sink_for(naming: bool, finding: bool) -> Sink {
    if naming {
        Sink::Name
    } else if finding {
        Sink::Find
    } else {
        Sink::Page
    }
}

/// The composition as far as the document is concerned.
///
/// The engine holds one composition wherever it is being typed, and only the
/// page splices it into text and shifts every index after the cursor. A
/// composition bound anywhere else is not the page's to show or to count.
fn page_composition(preedit: &str, sink: Sink) -> &str {
    if sink == Sink::Page { preedit } else { "" }
}

/// What the find bar's count cell says.
///
/// Nothing at all until there is something to count, and nothing while a word
/// is being composed **into the query**: that field is then showing the query
/// *and* the half-typed word, while the count is only ever about the query. A
/// number beside text it does not describe is worse than no number.
///
/// A word composed into the *replacement* leaves the query alone, so the count
/// still describes exactly what is on the field beside it and stays up.
/// What the editor knows that the strip has to say: the numbers and the states
/// that are not fixed words.
struct Readouts {
    /// Whether the query is empty, where the search is, and how many hits.
    count: Option<(bool, usize, usize)>,
    /// A half-typed CJK word in the query, which puts the count out of step
    /// with what is beside it.
    composing: bool,
    /// Whether `All` is waiting for a second tap.
    armed: bool,
    /// Which page of a panel's list, and how many there are.
    page: (usize, usize),
}

/// Cut a strip to what the panel can hold.
///
/// The order things are given up in, and why, is on [`Editor::strip_fitted`].
/// Free of the editor so that it runs against a stub metric on the host, the
/// device's faces not existing there.
fn fit_strip(
    width: u16,
    wanted: Vec<Bar>,
    readouts: &Readouts,
    mut measure: impl FnMut(&str) -> u16,
) -> (Vec<Bar>, Vec<String>) {
    for short in [false, true] {
        let labels = cell_words(&wanted, short, readouts);
        if strip_holds(width, &wanted, &labels, &mut measure) {
            return (wanted, labels);
        }
    }
    let bare: Vec<Bar> = wanted.into_iter().filter(|b| !b.is_readout()).collect();
    let labels = cell_words(&bare, true, readouts);
    (bare, labels)
}

/// What each cell says, in the strip's own words or in its shorter ones.
///
/// The fields are left blank: a field is sized by what the other cells leave,
/// so it cannot be written alongside them.
fn cell_words(bars: &[Bar], short: bool, readouts: &Readouts) -> Vec<String> {
    let (at, of) = readouts.page;
    let page = if short {
        format!("{at}/{of}")
    } else {
        format!("{at} of {of}")
    };
    bars.iter()
        .map(|b| match b {
            Bar::PageAt => page.clone(),
            Bar::Query | Bar::With => String::new(),
            Bar::Count => readouts
                .count
                .map_or_else(String::new, |(empty, at, total)| {
                    find_count(empty, readouts.composing, short, at, total)
                }),
            Bar::All if readouts.armed => "All?".to_string(),
            other if short => other.short().to_string(),
            other => other.label().to_string(),
        })
        .collect()
}

/// Whether a strip fits the panel with every cell on it and every field still
/// worth reading.
fn strip_holds(
    width: u16,
    bars: &[Bar],
    labels: &[String],
    mut measure: impl FnMut(&str) -> u16,
) -> bool {
    let fields = stretch_cells(bars);
    let cells: Vec<String> = labels.iter().map(|l| bracket(l)).collect();
    if ui::cell_bounds(width, &cells, &fields, &mut measure).len() < cells.len() {
        return false;
    }
    if fields.is_empty() {
        return true;
    }
    let others: Vec<String> = cells
        .iter()
        .enumerate()
        .filter(|(i, _)| !fields.contains(i))
        .map(|(_, label)| label.clone())
        .collect();
    ui::stretch_room(width, &others, fields.len(), measure) >= ui::FIELD_MIN
}

fn find_count(
    query_empty: bool,
    composing_query: bool,
    short: bool,
    at: usize,
    total: usize,
) -> String {
    if query_empty || composing_query {
        return String::new();
    }
    if total == 0 {
        return if short { "none" } else { "not found" }.to_string();
    }
    // How many, not just where — half of what a search bar is for is telling a
    // writer that the word they think they overuse appears eleven times.
    if short {
        format!("{}/{total}", at + 1)
    } else {
        format!("{} of {total}", at + 1)
    }
}

/// A display index as a document one, given where the preedit sits and how long
/// it is. Free of the editor so the mapping can be tested on its own.
fn document_index(display: usize, cursor: usize, preedit: usize) -> usize {
    if display <= cursor {
        display
    } else {
        display.saturating_sub(preedit).max(cursor)
    }
}

/// Whether the action strip is on screen. Free of the editor so the safety rule
/// below can be tested.
///
/// **The hidden flag is about the writing screen and nothing else.** A panel
/// draws its own strip unconditionally — that strip *is* the panel's controls,
/// not chrome that gets out of the way — so `writing` is the first thing asked.
/// Leaving it out was a real bug: opening Help or Files *from the keyboard* left
/// the flag set by the keystroke that opened it, so the panel drew a strip that
/// nothing would hit-test. Every tap on it was swallowed by the reveal guard in
/// [`Editor::tapped`], and swallowed again on the next tap, because
/// `set_chrome_hidden` declines to change anything outside `Mode::Writing` and
/// so the flag could never clear. `Done`, `Previous` and `Next` were dead until
/// the writer happened to go back, tap the page to reveal the chrome, and enter
/// a panel by finger instead.
///
/// Two more things override the flag while writing. **Without a keyboard the
/// strip is the only way out of the app**, which is safety rather than taste —
/// an early device run that left it unreachable cost a hard reset. And a search
/// puts the bar there: it is what is being typed into, and a field you cannot
/// see is not a field.
fn strip_visible(hidden: bool, keyboard_present: bool, finding: bool, writing: bool) -> bool {
    !writing || finding || !hidden || !keyboard_present
}

/// Whether `action` asks for the surface that is already on screen.
///
/// **Every shortcut that opens something closes it.** The chord that took one
/// keystroke to enter takes the same one to leave, so a hand that reached for
/// `Ctrl`/`⌘`+`O` never has to go and find Esc to undo itself. Focus mode has
/// always worked this way and is the shape the rest now follow.
///
/// The buttons are unaffected: `[ Files ]` opens the list and `[ Done ]` closes
/// it, because a finger has both on screen and nothing to remember.
///
/// `replacing` is the find bar's second field. Revealing it is what
/// `Ctrl`/`⌘`+`Shift`+`F` asks for, so carrying it is what being open means for
/// that chord — pressed on a plain find bar it still opens, rather than closing
/// a bar the writer did not ask to lose.
fn reopens(mode: &Mode, finding: bool, replacing: bool, action: &Action) -> bool {
    match action {
        Action::Files => matches!(mode, Mode::Files(_)),
        Action::Config => matches!(mode, Mode::Config),
        Action::Help => matches!(mode, Mode::Help),
        Action::Outline => matches!(mode, Mode::Outline(_)),
        Action::Find => finding,
        Action::Replace => replacing,
        _ => false,
    }
}

fn languages_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/languages")
}

/// Which input sources the language button cycles through.
///
/// **All five unless told otherwise**, so a writer who never opens Config is
/// exactly where they were. An unreadable or empty file means the same thing:
/// a set with nothing in it would leave no way to type at all, which is worse
/// than ignoring the file.
fn read_languages() -> Vec<Language> {
    let stored = std::fs::read_to_string(languages_file()).unwrap_or_default();
    let chosen: Vec<Language> = Language::ALL
        .into_iter()
        .filter(|l| stored.contains(l.letter()))
        .collect();
    if chosen.is_empty() {
        Language::ALL.to_vec()
    } else {
        chosen
    }
}

fn write_languages(enabled: &[Language]) {
    let letters: String = enabled.iter().map(|l| l.letter()).collect();
    let _ = std::fs::write(languages_file(), letters);
}

/// Which writing systems the Config panel offers a face for.
///
/// **One row per system, not per language.** English and German are drawn by
/// the same Latin faces, so a row each would be two controls over one setting;
/// and a writer who has turned Japanese off should not be offered a Japanese
/// face they will never see. Which is the dependency that put the enabled set
/// first: this list is built from what is on, not from the five karyll can
/// imagine.
fn font_groups(enabled: &[Language]) -> Vec<font::Group> {
    let mut groups: Vec<font::Group> = Vec::new();
    for language in Language::ALL.into_iter().filter(|l| enabled.contains(l)) {
        let group = match language.region() {
            Some(region) => font::Group::Han(region),
            None => font::Group::Latin,
        };
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    groups
}

/// The most chips one settings row carries.
///
/// **[`ui::chip_bounds`] drops a chip that would cross the right margin**, and
/// the same function hit-tests, so an option past the edge is neither drawn nor
/// tappable — a face that is installed and cannot be chosen. The narrowest panel
/// leaves about 990 px for chips once the shared label column is taken, and the
/// Latin names run to some 200 px apiece with their padding: four is close
/// enough to that budget to be riding on the length of the words, which is not a
/// thing to hold a control on the page with.
///
/// A count rather than a fitted row, now that chrome no longer follows the
/// document face and the widths hold still: the arithmetic would have to run in
/// `Editor::config_items`, which is `&self` where measuring wants the faces
/// mutably. Three is under the budget on every supported panel and splits the
/// Latin list where it already divides — the writing faces, then the firmware's
/// serifs.
const CHIPS_PER_ROW: usize = 3;

/// Split a row's options across as many rows as they need, evenly.
///
/// Even rather than filled-then-remainder: four families read as two and two,
/// not as three and a stray. Nothing in, nothing out — a writing system with no
/// family installed contributes no row at all.
fn chip_rows(options: &[usize]) -> Vec<Vec<usize>> {
    if options.is_empty() {
        return Vec::new();
    }
    let rows = options.len().div_ceil(CHIPS_PER_ROW);
    options
        .chunks(options.len().div_ceil(rows))
        .map(<[usize]>::to_vec)
        .collect()
}

fn fonts_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/fonts")
}

/// Which face draws each writing system.
///
/// An unreadable or missing file is the default list, which is what a writer
/// who has never opened Config should get.
fn read_choices() -> font::Choices {
    font::Choices::parse(&std::fs::read_to_string(fonts_file()).unwrap_or_default())
}

fn write_choices(choices: font::Choices) {
    let _ = std::fs::write(fonts_file(), choices.render());
}

fn focus_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/focus")
}

/// Whether focus mode was on when the last session ended.
///
/// Off unless the file says otherwise, so a var directory that has never been
/// written leaves the page plain.
fn read_focus() -> bool {
    std::fs::read_to_string(focus_file()).is_ok_and(|s| s.trim() == "1")
}

fn write_focus(on: bool) {
    let _ = std::fs::write(focus_file(), if on { "1" } else { "0" });
}

/// Say which node the keyboard is on, and whether anything but karyll can read
/// it.
///
/// The second half is [`evdev::Keyboard::tagged_for_x`]: X binds only tagged
/// nodes, so an untagged keyboard types in the editor and nowhere else on the
/// device. A log naming only the node cannot tell that apart from a keyboard
/// that reaches everything, which is the question every report of "it works in
/// karyll but not in kterm" turns on.
fn report_keyboard(keyboard: &evdev::Keyboard, how: &str) {
    let reach = match keyboard.tagged_for_x() {
        Some(true) => "tagged for X",
        Some(false) => "not tagged for X — karyll only",
        None => "no udev record",
    };
    eprintln!("keyboard: {}{how} ({reach})", keyboard.path().display());
}

fn bluetooth_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/bluetooth")
}

/// Whether the Bluetooth stack is to outlive the editor.
///
/// **Off unless the file says otherwise**, because the radio going away with the
/// editor is what the rest of the device is built to expect: the daemon holds
/// `/dev/stpbt`, and Audible and VoiceView have nothing while it does. A writer
/// who has never opened Config must not have that taken from them silently.
fn read_keep_bluetooth() -> bool {
    std::fs::read_to_string(bluetooth_file()).is_ok_and(|s| s.trim() == "1")
}

fn write_keep_bluetooth(on: bool) {
    let _ = std::fs::write(bluetooth_file(), if on { "1" } else { "0" });
}

fn colour_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/colour")
}

/// Whether a colour panel is used as one.
///
/// **On unless the file says otherwise.** Colour costs the rest of the device
/// nothing, so a colour Kindle shows colour before anyone has opened Config and
/// the switch is there to turn it off.
fn read_colour() -> bool {
    std::fs::read_to_string(colour_file()).map_or(true, |s| s.trim() != "0")
}

fn write_colour(on: bool) {
    let _ = std::fs::write(colour_file(), if on { "1" } else { "0" });
}

fn colours_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/colours")
}

/// Which colours the caret and the highlighter are set to, by name.
///
/// Names rather than the indices they resolve to, so the file still says what
/// it means if the picker ever grows a colour in the middle.
fn read_colours() -> window::Inks {
    std::fs::read_to_string(colours_file()).map_or_else(
        |_| window::Inks::default(),
        |text| window::Inks::parse(&text),
    )
}

fn write_colours(inks: window::Inks) {
    let _ = std::fs::write(colours_file(), inks.to_string());
}

fn size_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/size")
}

/// The body size the last session ended at.
///
/// **Stored as the size, not as a rung of the ladder.** An index is a position
/// in a list that will be edited, and inserting a size would silently move
/// every writer onto a different one — the same reason the font families are
/// kept by name. A number that is no longer offered snaps to the nearest that
/// is, so a setting from another build lands somewhere sensible.
fn read_size() -> f32 {
    std::fs::read_to_string(size_file())
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .filter(|px| px.is_finite())
        .map_or(render::DEFAULT_SIZE, render::nearest_size)
}

fn write_size(px: f32) {
    let _ = std::fs::write(size_file(), format!("{px}\n"));
}

fn line_length_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/line-length")
}

/// The line length the last session ended at, in characters.
///
/// Stored as the count for the reason the size is stored as the size: it is a
/// number that means something on its own, where an index into a ladder means
/// whatever the next build's ladder says it does.
fn read_line_length() -> u16 {
    std::fs::read_to_string(line_length_file())
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .map_or(render::DEFAULT_LINE_LENGTH, render::nearest_line_length)
}

fn write_line_length(chars: u16) {
    let _ = std::fs::write(line_length_file(), format!("{chars}\n"));
}

fn read_language() -> Language {
    // Not logged here. `set_language` reports what was actually taken up, and
    // two lines saying the same name would have hidden that they could differ.
    std::fs::read_to_string(language_file())
        .map(|s| Language::from_letter(&s))
        .unwrap_or_default()
}

fn write_language(language: Language) {
    let _ = std::fs::write(language_file(), language.letter().to_string());
}

/// Where each document was last being written.
///
/// **A writer opening a draft wants to carry on, not to travel.** `karyll.sh`
/// already reopens the most recently touched document, so without this karyll
/// remembers *which* draft and forgets *where* — which is the half that shows.
///
/// One line per document, `<index>\t<path>`, most recent first, and character
/// indices throughout like everything else in `karyll-core` — a byte offset
/// would land inside a codepoint in a Chinese draft.
fn positions_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/positions")
}

/// How many documents are remembered. Small on purpose: the list is rewritten
/// whole on every save, and a writer works on a handful of drafts.
const POSITIONS_KEPT: usize = 64;

/// The index comes first so that a path containing a tab still round-trips:
/// everything after the first separator is the path.
fn parse_positions(body: &str) -> Vec<(usize, String)> {
    body.lines()
        .filter_map(|line| {
            let (index, path) = line.split_once('\t')?;
            Some((index.trim().parse().ok()?, path.to_string()))
        })
        .collect()
}

/// Put `path` at the front with its new index, dropping any older entry for it.
///
/// Most-recent-first with a cap, so the file cannot grow without bound as
/// drafts come and go.
fn updated_positions(
    mut entries: Vec<(usize, String)>,
    path: &str,
    cursor: usize,
) -> Vec<(usize, String)> {
    entries.retain(|(_, p)| p != path);
    entries.insert(0, (cursor, path.to_string()));
    entries.truncate(POSITIONS_KEPT);
    entries
}

fn render_positions(entries: &[(usize, String)]) -> String {
    entries
        .iter()
        .map(|(index, path)| format!("{index}\t{path}\n"))
        .collect()
}

fn read_positions() -> Vec<(usize, String)> {
    parse_positions(&std::fs::read_to_string(positions_file()).unwrap_or_default())
}

/// Where `path` was left, if it is remembered.
fn read_position(path: &Path) -> Option<usize> {
    let wanted = path.to_string_lossy();
    read_positions()
        .into_iter()
        .find(|(_, p)| *p == wanted)
        .map(|(index, _)| index)
}

/// Drop what was remembered about a document that no longer exists.
///
/// The list is short and capped, so a stale entry would age out on its own —
/// but a new document can be given a deleted one's name, and inheriting a
/// cursor from prose that is gone would open it somewhere meaningless.
fn forget_position(path: &Path) {
    let wanted = path.to_string_lossy();
    let mut entries = read_positions();
    entries.retain(|(_, p)| *p != wanted);
    let _ = std::fs::write(positions_file(), render_positions(&entries));
}

fn write_position(path: &Path, cursor: usize) {
    let entries = updated_positions(read_positions(), &path.to_string_lossy(), cursor);
    let _ = std::fs::write(positions_file(), render_positions(&entries));
}

/// Where to put the cursor when a document opens.
///
/// **With nothing remembered the answer is the top.** Opening at the end would
/// be a claim that the file is a draft being continued, and nothing supports
/// that for a document karyll has never opened: it may as easily have arrived
/// over USB or been put there by the app itself. The welcome document is the
/// case that settles it — a page written to be read from its first line.
///
/// A stored index is clamped rather than distrusted. The file is plain Markdown
/// on a volume that mounts over USB, so it can have been edited elsewhere and
/// grown shorter — and landing mid-word is only a problem in theory, where
/// refusing to restore at all is one in practice.
fn opening_cursor_from(stored: Option<usize>, len: usize) -> usize {
    stored.unwrap_or(0).min(len)
}

fn opening_cursor(path: &Path, len: usize) -> usize {
    opening_cursor_from(read_position(path), len)
}

/// Where documents live. Outside the extension on purpose: updating karyll
/// replaces that directory wholesale, and prose must not go with it.
const DOCUMENTS: &str = "/mnt/us/karyll";

/// What the keys and the glass do.
///
/// **Laid out to the same grid as Config and Files**: the thing on the left, and
/// what is worth knowing about it in the detail column. Here that is the key,
/// which puts every one of them in a single column a writer can run an eye down
/// rather than reading each line to its end.
///
/// A list of actions rather than a list of keys, and that way round on purpose —
/// a reference is looked at with a job in mind ("how do I find?"), not with a
/// key in hand. Nothing here is tappable; it reports.
///
/// **Both chords are named `Ctrl`/`⌘` throughout** because both are bound, and
/// naming only one would tell half the writers this app has the wrong thing.
///
/// The CJK keys sit in with the rest rather than in a section of their own:
/// `Ctrl+Space` and `Shift+Enter` are shortcuts like any other, and a writer
/// looking for "how do I switch to Chinese" is looking in the shortcut list.
fn help_items() -> Vec<ui::Item> {
    let row = |label: &str, key: &str| ui::Item::Row {
        action: None,
        label: label.to_string(),
        detail: key.to_string(),
        on: false,
    };
    let heading = |text: &str| ui::Item::Heading(text.to_string());

    vec![
        heading("Writing"),
        row("Save now", "Ctrl/⌘ + S"),
        row("Undo, redo", "Ctrl/⌘ + Z,  Shift + Z"),
        row("Bold, italic", "Ctrl/⌘ + B,  I"),
        row("Highlight", "Ctrl/⌘ + Shift + H"),
        row("Heading level", "Ctrl/⌘ + 1 … 6"),
        row("Focus on this sentence", "Ctrl/⌘ + D"),
        row("Larger, smaller type", "Ctrl/⌘ + +,  Ctrl/⌘ + -"),
        heading("Getting around"),
        row("Find, then step through", "Ctrl/⌘ + F,  Enter"),
        row("Step back through matches", "Shift + Enter"),
        row("Find and replace", "Ctrl/⌘ + Shift + F"),
        row("Move between the two fields", "Tab"),
        row("Change this match, change all", "Ctrl/⌘ + Enter,  + Shift"),
        row("Sections of this document", "Ctrl/⌘ + Shift + O"),
        row("Word, line, document", "Ctrl/⌘ + ← → ↑ ↓"),
        row("Select as you go", "Shift + any move"),
        row("Documents, new document", "Ctrl/⌘ + O,  Ctrl/⌘ + N"),
        row("Settings", "Ctrl/⌘ + ,"),
        row("Turn a page of a list", "← →"),
        row("Clear the screen", "Ctrl/⌘ + R"),
        // The rule, said once rather than repeated on every row above.
        row("Close it again", "The same shortcut, or Esc"),
        row("Leave karyll", "Ctrl/⌘ + Q"),
        heading("Writing in Chinese and Japanese"),
        row("Switch input source", "Ctrl + Space"),
        row("Take a candidate", "Space, or 1 … 0"),
        row("Take the letters as typed", "Shift + Enter"),
        row("Drop the half-typed word", "Esc"),
        heading("Touch and pen"),
        // First, because it is the only way through a long document with
        // nothing paired, and the one thing here a reader needs before they
        // need anything else.
        row("Back a screen, on a screen", "Tap the left, right margin"),
        row("Start, end of the document", "Tap the top, the foot"),
        row("Bring the buttons back", "Tap the foot of the page"),
        row("Select a word", "Tap it twice"),
        row("Select a run", "Press at one end, lift at the other"),
        row("Extend a selection", "Shift + tap"),
        // Said plainly, because a Scribe owner will try it and should know
        // what to expect before they do.
        row("The pen", "Places the cursor. It does not write."),
        row("Delete a document", "Its Delete chip, twice"),
        row("Replace every match", "Its All chip, twice"),
        heading("Markdown it understands"),
        row("Headings", "# … ######"),
        row("Bold, italic", "**bold**  *italic*"),
        row("Highlighted", "==mark this=="),
        row("Struck out", "~~cut this~~"),
        row("Lists", "-  *  1."),
        row("Things to do", "- [ ]   done: - [x]"),
        row("Tick the one you are on", "Ctrl/⌘ + Enter"),
        row("Quote, rule", ">  ---"),
        row("Link, code", "[text](url)  `code`"),
        heading("Your writing"),
        row("Documents are kept in", DOCUMENTS),
        row("Saved by itself", "A few seconds after you stop typing"),
        row("Version", env!("CARGO_PKG_VERSION")),
    ]
}

/// Every heading in `chars`, in order.
///
/// Read from [`karyll_core::markdown::analyze`], which is the same pass the
/// renderer labels the page with — so a line the outline calls a heading is a
/// line drawn as one.
///
/// The word count is the prose **under** the heading, running to the next
/// heading of any level: the heading's own words are not in it, and a section
/// and its subsections do not count the same prose twice.
fn sections_of(chars: &[char]) -> Vec<Section> {
    let heads: Vec<(std::ops::Range<usize>, u8, String)> = karyll_core::markdown::analyze(chars)
        .iter()
        .filter_map(|line| match line.block {
            karyll_core::markdown::Block::Heading(level) => Some((
                line.range.clone(),
                level,
                karyll_core::markdown::plain(chars, line),
            )),
            _ => None,
        })
        .collect();
    heads
        .iter()
        .enumerate()
        .map(|(i, (range, level, text))| {
            let until = heads
                .get(i + 1)
                .map_or(chars.len(), |(next, _, _)| next.start);
            Section {
                level: *level,
                text: text.clone(),
                at: range.start,
                words: karyll_core::words::count(&chars[range.end.min(until)..until]),
            }
        })
        .collect()
}

/// The outline as rows, with the section holding `cursor` marked.
///
/// **Indented by level**, which is what makes it an outline rather than a list
/// of headings: the shape of the draft is the thing being looked at, and a flat
/// column of names does not have one.
fn outline_items(sections: &[Section], cursor: usize) -> Vec<ui::Item> {
    // The one the cursor is in: the last heading at or before it. Marked so
    // that opening the outline says where the writer already is before they
    // choose where to go.
    let here = sections.iter().rposition(|s| s.at <= cursor);
    sections
        .iter()
        .enumerate()
        .map(|(i, section)| ui::Item::Row {
            label: format!(
                "{}{}",
                OUTLINE_STEP.repeat(section.level.saturating_sub(1) as usize),
                // A heading with nothing after the hashes has no name, and a
                // blank row is a row that looks broken.
                if section.text.is_empty() {
                    "(untitled)"
                } else {
                    &section.text
                }
            ),
            detail: karyll_core::words::describe(section.words),
            on: here == Some(i),
            action: None,
        })
        .collect()
}

/// A strip label as it is drawn.
///
/// Empty stays empty: a cell with nothing to say is blank rather than a pair of
/// empty brackets, which reads as a button that has lost its label — and, when
/// pressed, flashed a black block where there was no button. The find bar's
/// count is blank until something has been typed.
fn bracket(label: &str) -> String {
    if label.is_empty() {
        String::new()
    } else {
        format!("[ {label} ]")
    }
}

/// Whether a character may go in a filename.
///
/// Only the three that would make a path of it. Han, kana and accented Latin
/// are all perfectly good filenames on this filesystem, and a writer who works
/// in Chinese should be able to say what a document is called in Chinese.
fn in_filename(c: char) -> bool {
    !matches!(c, '/' | '\\' | '\0')
}

/// A search in progress.
///
/// **Not a `Mode`.** Every mode in this editor is a full-screen panel over the
/// document, and a find that covers the document is a find you cannot use: the
/// whole point is watching the page move to the match. So it is a field, the
/// bar takes over the strip, and the writing screen goes on being the writing
/// screen underneath it.
#[derive(Debug, Default)]
struct Find {
    /// What has been typed into the bar.
    query: String,
    /// Every place it occurs, recomputed on each keystroke — see
    /// [`karyll_core::find::matches`] on why all of them and not one.
    hits: Vec<std::ops::Range<usize>>,
    /// Which hit the page is showing, an index into `hits`.
    at: usize,
    /// What to put in place of a match, and whether the bar is asking for it.
    ///
    /// **A state of the find bar rather than a bar of its own**: replacing is
    /// searching with one more thing to say, and the query, the hits and the
    /// stepping are all the same.
    replacing: bool,
    with: String,
    /// Which of the two fields the keys are going into.
    field: Field,
    /// Whether `[ All ]` has been tapped once.
    ///
    /// Two taps, the rule the Delete chip already follows: replacing all changes
    /// places the writer cannot see, and by touch alone there is no undo. The
    /// chip says which tap it is on.
    arming_all: bool,
}

/// Which of the replace bar's two fields is being typed into.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Field {
    #[default]
    Query,
    With,
}

impl Field {
    /// The strip cell a field is drawn in, and the reverse.
    ///
    /// One statement of the correspondence, so nothing has to know *where* on
    /// the strip either field sits — [`Editor::strip_wanted`] alone decides that.
    fn cell(self) -> Bar {
        match self {
            Field::Query => Bar::Query,
            Field::With => Bar::With,
        }
    }

    fn of(cell: Bar) -> Option<Field> {
        match cell {
            Bar::Query => Some(Field::Query),
            Bar::With => Some(Field::With),
            _ => None,
        }
    }
}

/// The cells of a bar that absorb whatever width the rest leave: its fields,
/// wherever [`Editor::strip_wanted`] has put them.
fn stretch_cells(bars: &[Bar]) -> Vec<usize> {
    bars.iter()
        .enumerate()
        .filter(|(_, bar)| Field::of(**bar).is_some())
        .map(|(i, _)| i)
        .collect()
}

/// A document as the Files panel knows it.
///
/// **Read once, when the panel opens.** `panel_items` is asked four times for a
/// single tap — hit-test, invert, restore, dispatch — and counting the words in
/// every document that often is real reading off eMMC. The count is a fact
/// about the file at the moment it was listed, which is what a list is.
#[derive(Debug, Clone)]
struct Listing {
    path: PathBuf,
    words: usize,
    modified: std::time::SystemTime,
}

/// Every `.md` in the documents directory, newest first.
fn list_documents() -> Vec<Listing> {
    let mut files: Vec<Listing> = std::fs::read_dir(DOCUMENTS)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .map(|path| {
            let modified = path
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            let words = std::fs::read_to_string(&path)
                .map(|text| karyll_core::words::count(&text.chars().collect::<Vec<_>>()))
                .unwrap_or(0);
            Listing {
                path,
                words,
                modified,
            }
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.modified));
    files
}

/// How long ago, in the coarsest unit that still says something.
///
/// **No calendar and no timezone.** A civil date needs both, and neither is in
/// `std` — but "3 days ago" is what a writer actually wants to know about a
/// draft, and it needs only a duration. It is also the more useful answer:
/// nobody remembers which of their drafts was 12 August.
fn ago(since: std::time::Duration) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let s = since.as_secs();
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match s {
        0..MINUTE => "just now".into(),
        MINUTE..HOUR => plural(s / MINUTE, "minute"),
        HOUR..DAY => plural(s / HOUR, "hour"),
        // Not "1 day ago", which is a clumsy way to say a word that exists.
        DAY..172_800 => "yesterday".into(),
        172_800..2_592_000 => plural(s / DAY, "day"),
        2_592_000..31_536_000 => plural(s / 2_592_000, "month"),
        _ => plural(s / 31_536_000, "year"),
    }
}

/// What the Files panel says about one document, beside its name.
fn describe_listing(words: usize, modified: std::time::SystemTime, open: bool) -> String {
    let when = std::time::SystemTime::now()
        .duration_since(modified)
        // A file stamped in the future — a device whose clock has just been
        // set, which this one does after every sleep. Not an error worth
        // showing; it was written as recently as anything can have been.
        .map(ago)
        .unwrap_or_else(|_| "just now".into());
    let words = karyll_core::words::describe(words);
    if open {
        // Said in words as well as in bold, because this is the document the
        // strip's Rename acts on and that must not be a guess.
        format!("open  ·  {words}  ·  {when}")
    } else {
        format!("{words}  ·  {when}")
    }
}

/// A path for a new document. Numbered rather than timestamped, because the
/// clock on a device that has been asleep is not to be trusted for naming.
fn new_document() -> PathBuf {
    for n in 1..1000 {
        // `exists` alone. Checking the listing as well asks the same question
        // of the same directory twice — and the listing reads every document to
        // count its words, which is a lot
        // of I/O to choose a filename with.
        let path = PathBuf::from(format!("{DOCUMENTS}/draft-{n}.md"));
        if !path.exists() {
            return path;
        }
    }
    PathBuf::from(format!("{DOCUMENTS}/draft.md"))
}

/// How often to ask the window manager which way the screen is. A subprocess
/// each time, so not every tick — but often enough that a flip does not leave
/// the buttons dead for long.
const ORIENTATION_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// How far a finger may wander and still count as having stayed put.
///
/// At 300 ppi this is about 3.5 mm — generous, deliberately. It separates a
/// drag from a tap and a double-tap from two taps, and the failure it guards
/// against is a shaky finger selecting a span when it meant to place the
/// cursor. Being too forgiving only means a very short drag places the cursor
/// instead, which is the harmless direction.
const TOUCH_SLOP: u16 = 40;

/// How long the second tap of a double-tap may take to arrive.
const DOUBLE_TAP: std::time::Duration = std::time::Duration::from_millis(400);

/// How long an inverted button stays inverted, however briefly it was touched.
///
/// Sized for the panel, not for the code: submitting an update takes about
/// 10 ms, but the ink itself needs roughly a quarter of a second to settle. A
/// shorter hold is drawn and undone before anything visibly moves, which looks
/// exactly like no feedback at all.
const FEEDBACK: std::time::Duration = std::time::Duration::from_millis(300);

/// A button on the action strip.
///
/// The label travels with the action rather than being matched as a string in a
/// second place. Two lists that have to agree are a bug waiting to happen: the
/// touch mapping was written twice and only ever fixed once at a time, and a
/// strip keyed on labels fails the same way — rename one and the button quietly
/// stops doing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bar {
    Files,
    /// Settings. Keyboard pairing is its first section: attaching a keyboard is
    /// configuration, not a screen of its own.
    Config,
    /// Leave the app, from the left corner.
    Exit,
    /// What the keys and the glass do.
    Help,
    /// The headings of the open document. Beside Files, because the pair are
    /// the same question at two scales: which document, and where in it.
    Outline,
    /// Leave a panel.
    Done,
    /// Abandon a name being typed.
    Cancel,
    /// Paging a list too long for the panel: back, where you are, and on.
    ///
    /// **All three or none.** This was one `More` that appeared only while
    /// there was a next page — so on the last page there was no button at all
    /// and no way back, which is a list you can read once and then have to
    /// leave and re-open. The wrap it was documented as doing could never
    /// happen, because the button was gone by the time it would have.
    ///
    /// Named apart from the find bar's `Previous`/`Next`/`Count` rather than
    /// shared with them, though they read the same and mean the same kind of
    /// thing: one `Bar` that dispatches two ways depending on the mode is the
    /// shape of bug this project keeps writing down.
    PageBack,
    PageAt,
    PageOn,
    /// The find bar's own cells: the field, how many hits there are and which
    /// one is showing, and the two steps between them. Their labels are what
    /// has been typed and what was found, so [`Editor::strip_labels`] fills
    /// them in rather than [`Bar::label`].
    Query,
    Count,
    Previous,
    Next,
    /// Ask for the second field as well. On the find bar, because a writer who
    /// has already typed what to look for should not have to type it again to
    /// change it.
    Replace,
    /// The second field, and the two things that can be done with it: this
    /// match, or every match.
    With,
    Change,
    All,
    /// Start a document. On the Files strip, where a list of documents is what
    /// you are looking at when you want another.
    New,
    /// Rename the open document — the one the Files list marks `open`, which is
    /// why it says so in words as well as in bold.
    Rename,
}

impl Bar {
    fn label(self) -> &'static str {
        match self {
            Bar::Files => "Files",
            Bar::Config => "Config",
            Bar::Exit => "Exit",
            Bar::Help => "Help",
            Bar::Outline => "Outline",
            Bar::Done => "Done",
            Bar::Cancel => "Cancel",
            // The find bar's words, because it is the same gesture — step
            // through a sequence, with a readout saying where you are.
            Bar::PageBack => "Previous",
            Bar::PageOn => "Next",
            // Filled in by `strip_labels`, which knows how many pages there are.
            Bar::PageAt => "",
            Bar::New => "New document",
            Bar::Rename => "Rename",
            // Filled in by `strip_labels`, which knows what was typed.
            Bar::Query => "Find:",
            Bar::Count => "",
            Bar::Previous => "Previous",
            Bar::Next => "Next",
            Bar::Replace => "Replace",
            // Filled in by `strip_labels`, like the query it sits beside.
            Bar::With => "With:",
            Bar::Change => "Change",
            // Filled in by `strip_labels`, which knows whether it is armed.
            Bar::All => "All",
        }
    }

    /// The word this cell uses when the panel is too narrow for its usual one.
    ///
    /// Only where there is a shorter word that means the same thing to the same
    /// reader. `Prev` for `Previous` is the one abbreviation an English writer
    /// does not have to think about; `Config` and `Replace` have no such form,
    /// and inventing one would cost more than the pixels it saved.
    fn short(self) -> &'static str {
        match self {
            Bar::New => "New",
            Bar::Previous | Bar::PageBack => "Prev",
            other => other.label(),
        }
    }

    /// Whether this cell reports something rather than doing something.
    ///
    /// **It is what a strip gives up when it cannot fit.** Losing the count of
    /// matches costs a writer information they can get by looking; losing
    /// `[ Done ]` costs them the way out.
    fn is_readout(self) -> bool {
        matches!(self, Bar::Count | Bar::PageAt)
    }
}

/// What a chip in Config's Keyboard section does.
///
/// Built alongside the labels so the two cannot drift: working out what row 3
/// means by arithmetic over three concatenated lists is exactly how a tap ends
/// up forgetting a keyboard it meant to connect.
#[derive(Debug, Clone)]
enum KeyAction {
    /// Drop the link, keeping the pairing.
    Disconnect(hid::Device),
    /// Remove it, and its saved link key, so it can be paired afresh.
    Forget(hid::Device),
    /// Pair with something the scan turned up.
    Pair(hid::Device),
    Scan,
}

/// What a line of the Config panel does. Built alongside its label, for the
/// reason [`KeyAction`] is.
///
/// The two with a list in them carry **the list the chips were drawn from**, so
/// the option a finger landed on is resolved against exactly what was on
/// screen. Looking the list up a second time in the handler is how a panel ends
/// up drawing one thing and doing another.
#[derive(Debug, Clone)]
enum ConfigRow {
    /// A heading. Not tappable, and here only so the list stays one list.
    None,
    /// The language chips: which source each option switches on or off.
    Languages(Vec<Language>),
    /// A writing system's chips, and which family each option is — an index
    /// into [`font::families`], skipping the ones not installed.
    Font(font::Group, Vec<usize>),
    /// The body size chips, which are [`render::SIZES`] in order.
    Size,
    /// The line length chips, which are [`render::LINE_LENGTHS`] in order.
    LineLength,
    /// One keyboard's chips, or the scan's.
    Keyboard(Vec<KeyAction>),
    /// Whether the Bluetooth stack outlives the editor. Option 1 keeps it.
    KeepBluetooth,
    /// Which way up to hold the Kindle, on the ones that cannot tell.
    Screen,
    /// Whether the colour panel is used as one. Option 1 is colour.
    Colour,
    /// Which of [`window::COLOURS`] the caret is drawn in.
    CaretColour,
    /// Which of them a `==highlight==` is. One colour, two values on the page:
    /// the rule takes it and the field takes its wash.
    HighlightColour,
}

/// Something a finger can be on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Strip(usize),
    Row(usize),
    /// A chip on a settings line: which line, and which of its values.
    Option(usize, usize),
}

/// What Tab inserts. Two columns is what Markdown nesting expects.
const INDENT: &str = "  ";

/// One level of the outline's indent.
///
/// Spaces, in the label column, because the rows are already laid out to the
/// panel's one grid. Four reads as a step at the panel's text size without
/// pushing a sixth-level heading off the page.
const OUTLINE_STEP: &str = "    ";

/// Shown when no file is given: enough mixed content to judge the type on a
/// real panel, which is the only place it can be judged.
/// The welcome document, for the one path that has no file: the binary run by
/// hand with no argument.
///
/// **The same file the launcher copies into an empty documents directory**, not
/// a second copy of it. It is the specimen — every kind of formatting karyll
/// understands is in it — and a specimen that has drifted from the document
/// writers actually see is worse than none, because it is the one thing checked
/// when the type looks wrong.
const SPECIMEN: &str = include_str!("../../../device/share/Welcome.md");

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// **The specimen has a job, and this is it.** It is the document a fresh
    /// install opens onto and the one thing looked at when the type is wrong on
    /// device, so a formatting kind missing from it is a kind nobody checks.
    /// Prose gets edited; an assertion does not quietly stop covering something.
    #[test]
    fn the_specimen_exercises_every_block_kind() {
        use karyll_core::markdown::{Block, Style};
        let chars: Vec<char> = SPECIMEN.chars().collect();
        let markup = karyll_core::markdown::analyze(&chars);

        for want in [
            Block::Paragraph,
            Block::Blank,
            Block::Heading(1),
            Block::Heading(2),
            Block::Heading(3),
            Block::Heading(4),
            Block::Quote,
            Block::ListItem { ordered: false },
            Block::ListItem { ordered: true },
            Block::Task { done: false },
            Block::Task { done: true },
            Block::Fence,
            Block::Code,
            Block::Rule,
        ] {
            assert!(
                markup.iter().any(|line| line.block == want),
                "the specimen has no {want:?} in it"
            );
        }

        // And the inline kinds, which are what the faces are actually judged on.
        for want in [
            Style::Emphasis,
            Style::Strong,
            // The specimen says nesting works, so it has to.
            Style::StrongEmphasis,
            Style::Strikethrough,
            Style::Code,
            Style::Link,
            Style::Url,
        ] {
            assert!(
                markup
                    .iter()
                    .flat_map(|line| &line.spans)
                    .any(|span| span.style == want),
                "the specimen has no {want:?} in it"
            );
        }
    }

    /// Every shortcut that opens a surface closes it. The list is easy to get
    /// half-right — one new panel with a chord and no matching arm here and the
    /// rule quietly stops being a rule.
    mod toggling {
        use super::*;

        /// Each opening chord, paired with the mode it opens.
        fn panels() -> Vec<(Action, Mode)> {
            vec![
                (Action::Files, Mode::Files(Vec::new())),
                (Action::Config, Mode::Config),
                (Action::Help, Mode::Help),
                (Action::Outline, Mode::Outline(Vec::new())),
            ]
        }

        #[test]
        fn a_panels_own_shortcut_closes_it() {
            for (action, mode) in panels() {
                assert!(
                    reopens(&mode, false, false, &action),
                    "{action:?} on its own panel has to close it"
                );
            }
        }

        #[test]
        fn from_the_page_every_shortcut_still_opens() {
            for (action, _) in panels() {
                assert!(
                    !reopens(&Mode::Writing, false, false, &action),
                    "{action:?} while writing has to open, not close"
                );
            }
        }

        #[test]
        fn another_panels_shortcut_goes_straight_there() {
            // Config from the Files list is still Config, not a way out. Only
            // the chord that matches the surface closes it.
            for (action, mode) in panels() {
                for (other, _) in panels() {
                    if std::mem::discriminant(&action) == std::mem::discriminant(&other) {
                        continue;
                    }
                    assert!(
                        !reopens(&mode, false, false, &other),
                        "{other:?} from {action:?}'s panel should open it"
                    );
                }
            }
        }

        #[test]
        fn find_closes_the_bar_whichever_field_it_carries() {
            assert!(!reopens(&Mode::Writing, false, false, &Action::Find));
            assert!(reopens(&Mode::Writing, true, false, &Action::Find));
            assert!(reopens(&Mode::Writing, true, true, &Action::Find));
        }

        #[test]
        fn replace_closes_only_a_bar_that_is_already_replacing() {
            // On a plain find bar the chord reveals the second field, which is
            // what it is for. Closing there would lose a query the writer
            // typed.
            assert!(!reopens(&Mode::Writing, false, false, &Action::Replace));
            assert!(!reopens(&Mode::Writing, true, false, &Action::Replace));
            assert!(reopens(&Mode::Writing, true, true, &Action::Replace));
        }

        #[test]
        fn nothing_else_toggles() {
            // Typing must never be read as a request to close something.
            for action in [
                Action::Newline,
                Action::Save,
                Action::Refresh,
                Action::NewDocument,
                Action::Insert('a'),
                Action::Escape,
            ] {
                for mode in [Mode::Writing, Mode::Config, Mode::Help] {
                    assert!(!reopens(&mode, true, true, &action), "{action:?}");
                }
            }
        }
    }

    mod outline {
        use super::*;

        const DRAFT: &str = "\
# The whole thing

An opening line of five words.

## First **part**

Three words here.

### A detail

nine words in this one under the third level

## Second part

";

        fn of(src: &str) -> Vec<Section> {
            sections_of(&src.chars().collect::<Vec<_>>())
        }

        #[test]
        fn every_heading_is_listed_with_its_level_and_its_name() {
            let found = of(DRAFT);
            let named: Vec<(u8, &str)> = found.iter().map(|s| (s.level, s.text.as_str())).collect();
            assert_eq!(
                named,
                vec![
                    (1, "The whole thing"),
                    (2, "First part"),
                    (3, "A detail"),
                    (2, "Second part"),
                ]
            );
        }

        /// The count is the prose under the heading, not the heading's own
        /// words, and it stops at the next heading of **any** level — so a
        /// section and its subsections do not count the same prose twice.
        #[test]
        fn each_section_counts_only_the_prose_below_it() {
            let counts: Vec<usize> = of(DRAFT).iter().map(|s| s.words).collect();
            assert_eq!(counts, vec![6, 3, 9, 0]);
        }

        #[test]
        fn a_jump_lands_on_the_heading_line_itself() {
            let chars: Vec<char> = DRAFT.chars().collect();
            for section in of(DRAFT) {
                assert_eq!(chars[section.at], '#', "{:?}", section.text);
            }
        }

        /// Only real headings. A `#` inside a fence is code, and a line that
        /// merely starts with one is prose.
        #[test]
        fn hashes_that_are_not_headings_are_not_sections() {
            assert!(of("```\n# not a heading\n```\n").is_empty());
            assert!(of("#nospace\n").is_empty());
            assert!(of("Ordinary prose.\n").is_empty());
        }

        #[test]
        fn a_document_with_no_headings_has_no_outline() {
            assert!(of("").is_empty());
        }

        #[test]
        fn the_rows_step_in_by_level() {
            let rows = outline_items(&of(DRAFT), 0);
            let labels: Vec<&str> = rows
                .iter()
                .map(|item| match item {
                    ui::Item::Row { label, .. } => label.as_str(),
                    _ => panic!("every outline line is a row"),
                })
                .collect();
            assert_eq!(labels[0], "The whole thing");
            assert_eq!(labels[1], format!("{OUTLINE_STEP}First part"));
            assert_eq!(labels[2], format!("{OUTLINE_STEP}{OUTLINE_STEP}A detail"));
        }

        /// Opening the outline says where the writer already is, before they
        /// choose where to go.
        #[test]
        fn the_section_the_cursor_is_in_is_the_marked_one() {
            let found = of(DRAFT);
            let marked = |cursor: usize| -> Option<usize> {
                outline_items(&found, cursor)
                    .iter()
                    .position(|item| matches!(item, ui::Item::Row { on: true, .. }))
            };
            assert_eq!(marked(0), Some(0), "on the first heading itself");
            assert_eq!(marked(found[1].at + 1), Some(1), "inside the second");
            // Just before a heading belongs to the section above it, not to the
            // one about to start.
            assert_eq!(marked(found[2].at - 1), Some(1));
            assert_eq!(marked(usize::MAX), Some(3), "past the end is the last");
        }

        /// A heading with nothing after the hashes still gets a row, because a
        /// blank one reads as a fault.
        #[test]
        fn a_nameless_heading_is_still_somewhere_to_go() {
            let found = of("## \n\nsome words here\n");
            assert_eq!(found.len(), 1);
            let rows = outline_items(&found, 0);
            match &rows[0] {
                ui::Item::Row { label, .. } => {
                    assert!(label.trim_start().starts_with('('), "{label:?}")
                }
                _ => panic!("a row"),
            }
        }
    }

    /// **A row's detail is drawn, not fitted**: it starts at the shared column
    /// and runs as far as it runs, so a line too long for the panel is cut off
    /// by the edge of the screen rather than wrapped or shortened. Help is
    /// where the long ones are — a whole gesture described in a phrase — and it
    /// is the page a writer reads when something is already not working.
    #[test]
    fn no_help_line_runs_off_a_narrow_panel() {
        use crate::font::Proportional;
        let items = help_items();
        for panel in [1264u16, 1272, 1860] {
            let measure = |s: &str| ui::label_width(&mut Proportional, s, ui::TEXT_PX);
            let column = ui::chip_column(&items, panel, measure);
            for item in &items {
                let ui::Item::Row { label, detail, .. } = item else {
                    continue;
                };
                let end = column + measure(detail);
                assert!(
                    end <= panel - ui::MARGIN_X,
                    "on a {panel} px panel, {label:?} runs to {end}"
                );
            }
        }
    }

    /// Which candidates are on the bar, given where the pages fall.
    ///
    /// The split itself is [`ui::candidate_pages`]'s and tested there; this is
    /// the other half, and it is the half a digit goes through.
    mod candidates {
        use super::*;

        fn list() -> Vec<String> {
            "你好吗我们".chars().map(|c| c.to_string()).collect()
        }

        #[test]
        fn a_page_is_the_run_between_its_start_and_the_next_ones() {
            let list = list();
            let pages = vec![0, 2];
            assert_eq!(candidate_page(&list, &pages, 0), &list[0..2]);
            assert_eq!(
                candidate_page(&list, &pages, 1),
                &list[2..5],
                "the last page runs to the end of the list"
            );
        }

        /// **A page that is not there is empty, not a panic.** The list is
        /// replaced on every keystroke and can be shorter than it was, and a
        /// slice past the end of it would take the editor down mid-word.
        #[test]
        fn a_page_past_the_end_is_empty() {
            assert!(candidate_page(&list(), &[0, 2], 2).is_empty());
            assert!(candidate_page(&list(), &[], 0).is_empty());
            assert!(candidate_page(&[], &[0], 0).is_empty());
            assert!(candidate_page(&list(), &[0, 99], 1).is_empty());
        }
    }

    /// Every strip [`Editor::strip_wanted`] can build, on every panel karyll targets.
    ///
    /// **A dropped cell is a control that is not there**, and the strip is what
    /// a writer with no keyboard has instead of shortcuts — an early device run
    /// that left `[ Exit ]` unreachable cost a hard reset. The strip was laid
    /// out on a 10.2″ panel, where all of these fit; the smaller ones are 68%
    /// of that width.
    mod strips {
        use super::*;
        use crate::font::Proportional;

        /// The panels karyll targets, narrowest first.
        const PANELS: [u16; 3] = [1264, 1272, 1860];

        /// The strips, as [`Editor::strip_wanted`] builds them. Written out rather
        /// than reached through an `Editor`, which needs a window.
        fn strips() -> Vec<(&'static str, Vec<Bar>)> {
            let paging = [Bar::PageBack, Bar::PageAt, Bar::PageOn];
            let mut out = vec![
                (
                    "writing",
                    vec![Bar::Exit, Bar::Files, Bar::Config, Bar::Help],
                ),
                ("naming", vec![Bar::Cancel]),
                ("files", vec![Bar::Done, Bar::New, Bar::Rename]),
                ("panel", vec![Bar::Done]),
                (
                    "find",
                    vec![
                        Bar::Query,
                        Bar::Count,
                        Bar::Previous,
                        Bar::Next,
                        Bar::Replace,
                        Bar::Done,
                    ],
                ),
                (
                    "replace",
                    vec![
                        Bar::Query,
                        Bar::With,
                        Bar::Count,
                        Bar::Previous,
                        Bar::Next,
                        Bar::Change,
                        Bar::All,
                        Bar::Done,
                    ],
                ),
            ];
            // A list longer than the panel adds all three paging cells.
            for (name, cells) in [("files", 2), ("panel", 3)] {
                let mut paged = out[cells].1.clone();
                paged.extend(paging);
                out.push((name, paged));
            }
            out
        }

        /// The numbers at their longest, which is what the strip has to fit:
        /// a three-figure count, and a document of twelve pages.
        fn readouts() -> Readouts {
            Readouts {
                count: Some((false, 127, 128)),
                composing: false,
                armed: true,
                page: (8, 12),
            }
        }

        fn measure(s: &str) -> u16 {
            ui::label_width(&mut Proportional, s, ui::TEXT_PX)
        }

        /// **Every control stays on the strip, at every width.** What the strip
        /// gives up to manage it is its longer words and then its readouts;
        /// what it must never give up is a button, which is what
        /// [`ui::cell_bounds`] does on its own — from the tail, so the first to
        /// go would be `[ Done ]`.
        #[test]
        fn no_strip_drops_a_control_on_any_panel() {
            for (name, wanted) in strips() {
                for panel in PANELS {
                    let (cells, labels) = fit_strip(panel, wanted.clone(), &readouts(), measure);
                    for bar in &wanted {
                        assert!(
                            bar.is_readout() || cells.contains(bar),
                            "the {name} strip loses {bar:?} on a {panel} px panel"
                        );
                    }
                    let drawn: Vec<String> = labels.iter().map(|l| bracket(l)).collect();
                    let bounds = ui::cell_bounds(panel, &drawn, &stretch_cells(&cells), measure);
                    assert_eq!(
                        bounds.len(),
                        cells.len(),
                        "the {name} strip draws only {} of its {} cells on a {panel} px panel",
                        bounds.len(),
                        cells.len()
                    );
                }
            }
        }

        /// A field with no room to show what is being typed into it is a
        /// control that is present and useless, which the count above would not
        /// notice.
        #[test]
        fn a_find_field_keeps_room_for_what_is_typed_into_it() {
            for (name, wanted) in strips() {
                if stretch_cells(&wanted).is_empty() {
                    continue;
                }
                for panel in PANELS {
                    let (cells, labels) = fit_strip(panel, wanted.clone(), &readouts(), measure);
                    let fields = stretch_cells(&cells);
                    let others: Vec<String> = labels
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !fields.contains(i))
                        .map(|(_, label)| bracket(label))
                        .collect();
                    let room = ui::stretch_room(panel, &others, fields.len(), measure);
                    assert!(
                        room >= ui::FIELD_MIN,
                        "the {name} bar's fields get {room} px each on a {panel} px panel"
                    );
                }
            }
        }

        /// The 10.2″ panel keeps the words it was written with. The narrower
        /// ones are what the shortening is for, and it must not reach back.
        #[test]
        fn the_panel_it_was_written_on_gives_nothing_up() {
            for (name, wanted) in strips() {
                let (cells, labels) = fit_strip(1860, wanted.clone(), &readouts(), measure);
                assert_eq!(cells, wanted, "the {name} strip lost a cell it need not");
                assert!(
                    !labels.iter().any(|l| l == "Prev" || l == "New"),
                    "the {name} strip shortened a word it need not"
                );
            }
        }
    }

    mod replacing {
        use super::*;

        /// The bar's two states, as [`Editor::strip_wanted`] builds them. Written out
        /// here rather than reached through an `Editor`, which needs a window.
        const FINDING: [Bar; 6] = [
            Bar::Query,
            Bar::Count,
            Bar::Previous,
            Bar::Next,
            Bar::Replace,
            Bar::Done,
        ];
        const REPLACING: [Bar; 8] = [
            Bar::Query,
            Bar::With,
            Bar::Count,
            Bar::Previous,
            Bar::Next,
            Bar::Change,
            Bar::All,
            Bar::Done,
        ];

        /// The elastic cells are the fields, found by what they are. Nothing
        /// else may state the bar's order.
        #[test]
        fn the_fields_are_the_cells_that_stretch() {
            assert_eq!(stretch_cells(&FINDING), vec![0]);
            assert_eq!(stretch_cells(&REPLACING), vec![0, 1]);
            // And a bar with no field on it has nothing elastic, so the
            // remainder falls to the status line.
            assert_eq!(
                stretch_cells(&[Bar::Exit, Bar::Files, Bar::Config]),
                Vec::<usize>::new()
            );
        }

        /// Every elastic cell resolves back to the field it draws, whatever
        /// order the bar puts them in.
        #[test]
        fn each_field_finds_its_own_cell_and_no_other() {
            for bars in [&REPLACING[..], &FINDING[..]] {
                for cell in stretch_cells(bars) {
                    let field = Field::of(bars[cell]).expect("an elastic cell is a field");
                    assert_eq!(bars.iter().position(|b| *b == field.cell()), Some(cell));
                }
            }
            // Reordering the bar moves the answer with it rather than leaving
            // a stale index behind.
            let reversed: Vec<Bar> = REPLACING.iter().rev().copied().collect();
            assert_eq!(stretch_cells(&reversed), vec![6, 7]);
        }

        /// A cell that is not a field is not one, so nothing else on the strip
        /// can be typed into by accident.
        #[test]
        fn no_button_is_mistaken_for_a_field() {
            for bar in [Bar::Count, Bar::Change, Bar::All, Bar::Done, Bar::Replace] {
                assert_eq!(Field::of(bar), None, "{bar:?}");
            }
        }
    }

    #[test]
    fn the_strip_never_hides_while_there_is_no_keyboard() {
        // The rule that matters, and the only one here that is safety rather
        // than taste. With nothing paired the strip is the only way out of the
        // app; an early device run that left it unreachable cost a hard reset,
        // and hiding it on a keystroke that cannot arrive would rebuild that.
        assert!(strip_visible(true, false, false, true));
    }

    #[test]
    fn a_documents_age_reads_in_the_coarsest_unit_that_says_something() {
        let secs = |n| ago(Duration::from_secs(n));
        assert_eq!(secs(0), "just now");
        assert_eq!(secs(59), "just now");
        assert_eq!(secs(60), "1 minute ago");
        assert_eq!(secs(3599), "59 minutes ago");
        assert_eq!(secs(3600), "1 hour ago");
        assert_eq!(secs(86_399), "23 hours ago");
        // The one unit with a word of its own, and using it is the point.
        assert_eq!(secs(86_400), "yesterday");
        assert_eq!(secs(172_799), "yesterday");
        assert_eq!(secs(172_800), "2 days ago");
        assert_eq!(secs(2_591_999), "29 days ago");
        assert_eq!(secs(2_592_000), "1 month ago");
        assert_eq!(secs(31_535_999), "12 months ago");
        assert_eq!(secs(31_536_000), "1 year ago");
    }

    /// The device sets its clock after a sleep, so a file can carry a stamp
    /// slightly in the future. That is not an error worth putting on screen.
    #[test]
    fn a_file_stamped_in_the_future_reads_as_just_now() {
        let ahead = std::time::SystemTime::now() + Duration::from_secs(600);
        assert!(describe_listing(3, ahead, false).ends_with("just now"));
    }

    #[test]
    fn the_open_document_says_so_beside_its_name() {
        let now = std::time::SystemTime::now();
        assert!(describe_listing(1_284, now, true).starts_with("open"));
        assert!(describe_listing(1_284, now, true).contains("1,284 words"));
        assert!(!describe_listing(1_284, now, false).contains("open"));
        assert!(describe_listing(0, now, false).contains("empty"));
    }

    #[test]
    fn two_latin_languages_are_one_font_row() {
        use karyll_core::script::Region;
        assert_eq!(
            font_groups(&[Language::English, Language::German]),
            vec![font::Group::Latin],
            "one setting, so one row"
        );
        assert_eq!(
            font_groups(&[Language::German, Language::Japanese]),
            vec![font::Group::Latin, font::Group::Han(Region::Japanese)]
        );
        // Turned off is not offered: the panel is built from the enabled set.
        assert_eq!(
            font_groups(&[Language::Chinese]),
            vec![font::Group::Han(Region::Simplified)]
        );
        // The two conventions are separate settings, because Han unification
        // gives them one code point and two correct glyphs.
        assert_eq!(
            font_groups(&[Language::Chinese, Language::ChineseTraditional]),
            vec![
                font::Group::Han(Region::Simplified),
                font::Group::Han(Region::Traditional)
            ]
        );
    }

    /// Every option reaches a row, in order and exactly once. The chip a finger
    /// lands on is looked up through the list its row was drawn from, so an
    /// option dropped or duplicated by the split would set the wrong family.
    #[test]
    fn splitting_a_row_keeps_every_option_in_order() {
        for count in 0..12usize {
            let options: Vec<usize> = (0..count).collect();
            let rows = chip_rows(&options);
            assert!(
                rows.iter().all(|row| row.len() <= CHIPS_PER_ROW),
                "{count} options put more than {CHIPS_PER_ROW} on a row: {rows:?}"
            );
            assert_eq!(rows.concat(), options, "{count} options came back changed");
        }
    }

    /// Even, not filled-then-remainder: four families read as two and two.
    #[test]
    fn a_split_row_divides_evenly() {
        assert!(chip_rows(&[]).is_empty(), "nothing in, no row out");
        assert_eq!(chip_rows(&[0]), vec![vec![0]]);
        assert_eq!(
            chip_rows(&[0, 1, 2]),
            vec![vec![0, 1, 2]],
            "three still fit"
        );
        assert_eq!(chip_rows(&[0, 1, 2, 3]), vec![vec![0, 1], vec![2, 3]]);
        assert_eq!(
            chip_rows(&[0, 1, 2, 3, 4]),
            vec![vec![0, 1, 2], vec![3, 4]],
            "the Latin list splits into the writing faces and the firmware's"
        );
        assert_eq!(
            chip_rows(&[0, 1, 2, 3, 4, 5]),
            vec![vec![0, 1, 2], vec![3, 4, 5]]
        );
    }

    #[test]
    fn text_before_the_preedit_is_at_the_same_index_in_both_spaces() {
        // Cursor at 5 with three characters composing. Everything up to the
        // cursor is untouched by the splice.
        assert_eq!(document_index(0, 5, 3), 0);
        assert_eq!(document_index(5, 5, 3), 5);
    }

    #[test]
    fn text_after_the_preedit_is_that_much_further_along_on_screen() {
        // Display 8 is the first character after a three-long preedit, and it
        // is document 5 — the character the cursor is sitting before.
        assert_eq!(document_index(8, 5, 3), 5);
        assert_eq!(document_index(12, 5, 3), 9);
    }

    #[test]
    fn the_preedit_itself_collapses_onto_the_cursor() {
        // Its characters are in no document position at all — tapping one has
        // to mean the place the word is being written into.
        for display in 6..=8 {
            assert_eq!(document_index(display, 5, 3), 5, "display {display}");
        }
    }

    #[test]
    fn with_nothing_composing_the_two_spaces_are_the_same() {
        for display in 0..12 {
            assert_eq!(document_index(display, 5, 0), display);
        }
    }

    #[test]
    fn a_field_over_the_page_takes_the_keystrokes() {
        assert_eq!(sink_for(false, false), Sink::Page);
        assert_eq!(sink_for(false, true), Sink::Find);
        assert_eq!(sink_for(true, false), Sink::Name);
        // A panel covers the page, and a bar under a panel is not reachable.
        // They cannot both be open — the find bar takes the strip, so there is
        // no New button on it — but the precedence is stated rather than left
        // to whichever branch happens to be written first.
        assert_eq!(sink_for(true, true), Sink::Name);
    }

    #[test]
    fn a_composition_bound_elsewhere_is_not_in_the_document() {
        // The page splices its composition into the text, which moves every
        // index after the cursor. A word being typed into the find bar must
        // move nothing: the hits are document indices, and shifting them would
        // invert the wrong characters while the next word is spelled out.
        assert_eq!(page_composition("にほん", Sink::Page), "にほん");
        assert_eq!(page_composition("にほん", Sink::Find), "");
        assert_eq!(page_composition("にほん", Sink::Name), "");

        let composing = page_composition("にほん", Sink::Find).chars().count();
        for display in 0..12 {
            assert_eq!(document_index(display, 5, composing), display);
        }
    }

    #[test]
    fn the_count_says_nothing_it_cannot_stand_behind() {
        assert_eq!(find_count(false, false, false, 2, 12), "3 of 12");
        assert_eq!(find_count(false, false, false, 0, 0), "not found");
        // Nothing typed yet: "not found" for an empty search would be an
        // answer to a question nobody asked.
        assert_eq!(find_count(true, false, false, 0, 0), "");
        // And nothing while a word is being composed into the query, because
        // that field is then showing the query plus a half-typed word while the
        // count is still only about the query. A word composed into the
        // replacement leaves the query alone, and the caller says so by passing
        // false — the count stays up.
        assert_eq!(find_count(false, true, false, 2, 12), "");
    }

    #[test]
    fn a_filename_may_be_written_in_any_script() {
        // Only what would make a path of it is barred. A writer who works in
        // Chinese should be able to say what a document is called in Chinese.
        for c in ['日', '本', 'ぬ', 'é', '中', '_', ' ', '.'] {
            assert!(in_filename(c), "{c} is a perfectly good filename");
        }
        for c in ['/', '\\', '\0'] {
            assert!(!in_filename(c));
        }
    }

    #[test]
    fn a_descriptor_that_can_no_longer_deliver_counts_as_ready() {
        // **The bug behind every "the keyboard stopped working until I
        // relaunched".** `evdev_poll` returns `EPOLLHUP | EPOLLERR` and
        // *never* `EPOLLIN` once the device is gone, so a wait that tested
        // `POLLIN` alone never read the node, never saw the read fail, never
        // let go of the descriptor, and never looked for its replacement — for
        // the rest of the session.
        //
        // A pipe cannot stand in for that on the host: closing the write end
        // leaves the read end *readable* at EOF, so `POLLIN` is set as well and
        // the broken rule and the fixed one agree. An invalid descriptor has
        // the shape that matters — `revents` set, `POLLIN` clear — and it is
        // the only discrimination this rule makes.
        const GONE: std::os::unix::io::RawFd = 1_000_000;
        assert_eq!(
            wait(&[GONE], 0).unwrap(),
            vec![true],
            "a descriptor that will never deliver again has to wake the loop"
        );

        // And the other half, which is what makes "any `revents`" safe to act
        // on rather than a permanent false alarm: an open, idle descriptor
        // reports nothing. `POLLOUT` is not asked for, and the kernel masks
        // `revents` down to what was asked for plus the three error bits.
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        assert_eq!(wait(&[pipe[0]], 0).unwrap(), vec![false]);
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }

    #[test]
    fn typing_with_a_keyboard_is_the_one_case_that_hides_it() {
        assert!(!strip_visible(true, true, false, true));
        // And it comes straight back when the flag is cleared.
        assert!(strip_visible(false, true, false, true));
        // A search puts the bar on the strip, and a field you cannot see is
        // not a field — so it stays whatever the chrome flag says.
        assert!(strip_visible(true, true, true, true));
    }

    #[test]
    fn a_panel_always_has_its_strip_whatever_the_writing_screen_was_doing() {
        // Opening a panel *from the keyboard* leaves the hidden flag set by the
        // keystroke that opened it, so the panel draws a strip nothing will
        // hit-test — and `set_chrome_hidden` declines to touch the flag outside
        // `Mode::Writing`, so it cannot clear. Reaching a panel by finger
        // reveals the chrome first, which hides the fault.
        //
        // A panel's strip is its controls, not chrome that gets out of the way.
        assert!(strip_visible(true, true, false, false));
    }

    #[test]
    fn mid_sentence_does_not_autosave() {
        // The whole point of the idle trigger: a pause between keystrokes must
        // not be mistaken for the writer stopping.
        assert!(!autosave_due(
            Some(Duration::from_millis(400)),
            Duration::from_secs(5)
        ));
    }

    #[test]
    fn a_pause_autosaves() {
        assert!(autosave_due(Some(AUTOSAVE_IDLE), Duration::from_secs(5)));
    }

    #[test]
    fn typing_without_pause_still_autosaves_eventually() {
        // Someone who never pauses would otherwise never be written out.
        assert!(autosave_due(Some(Duration::ZERO), AUTOSAVE_MAX));
        assert!(!autosave_due(
            Some(Duration::ZERO),
            AUTOSAVE_MAX - Duration::from_secs(1)
        ));
    }

    #[test]
    fn a_document_that_arrived_dirty_is_written_without_waiting_for_a_keystroke() {
        assert!(autosave_due(None, Duration::ZERO));
    }

    /// The backstop has to be longer than the pause, or the pause never gets a
    /// chance to be the thing that fires and every save lands mid-word.
    #[test]
    fn the_backstop_is_the_looser_of_the_two() {
        assert!(AUTOSAVE_MAX > AUTOSAVE_IDLE);
    }

    mod language {
        use super::*;

        /// A language names its keyboard. Pinyin and romaji are both defined
        /// against the QWERTY letter arrangement, so Chinese and Japanese are
        /// US however the last prose was typed.
        #[test]
        fn each_language_names_its_own_layout() {
            assert_eq!(Language::English.layout(), keymap::Layout::Us);
            assert_eq!(Language::German.layout(), keymap::Layout::German);
            assert_eq!(Language::Chinese.layout(), keymap::Layout::Us);
            assert_eq!(Language::Japanese.layout(), keymap::Layout::Us);
        }

        #[test]
        fn cycling_visits_every_enabled_language_and_returns() {
            let all = Language::ALL;
            let mut seen = Vec::new();
            let mut language = Language::default();
            for _ in 0..all.len() {
                seen.push(language);
                language = language.next(&all);
            }
            assert_eq!(language, Language::default(), "the cycle does not close");
            let mut sorted = seen.clone();
            sorted.dedup();
            assert_eq!(sorted.len(), all.len(), "a language is skipped");
        }

        #[test]
        fn cycling_skips_the_ones_that_are_switched_off() {
            // The point of switching them off: someone who writes two should
            // press Ctrl+Space twice to get back, not five times.
            let two = [Language::English, Language::Japanese];
            assert_eq!(Language::English.next(&two), Language::Japanese);
            assert_eq!(Language::Japanese.next(&two), Language::English);
        }

        #[test]
        fn one_language_on_its_own_cycles_to_itself() {
            let one = [Language::German];
            assert_eq!(Language::German.next(&one), Language::German);
        }

        #[test]
        fn a_language_switched_off_still_has_somewhere_to_go() {
            // Turning off the source in use has to leave the keyboard
            // somewhere the cycle can still reach, and it moves forward from
            // where that source sat rather than back to the start.
            let rest = [Language::English, Language::Japanese];
            assert_eq!(Language::German.next(&rest), Language::Japanese);
            assert_eq!(Language::ChineseTraditional.next(&rest), Language::Japanese);
        }

        /// Only the CJK languages load an engine; the Latin ones must not pay
        /// for one, and must keep working if every plugin is missing.
        #[test]
        fn only_the_cjk_languages_want_an_input_method() {
            assert_eq!(Language::Chinese.script(), Some(ime::Script::Chinese));
            assert_eq!(
                Language::ChineseTraditional.script(),
                Some(ime::Script::Chinese)
            );
            assert_eq!(Language::Japanese.script(), Some(ime::Script::Japanese));
            assert_eq!(Language::English.script(), None);
            assert_eq!(Language::German.script(), None);
        }

        /// Both Chinese entries share an engine — one plugin, one dictionary —
        /// while Japanese is a separate one. Getting this wrong would either
        /// load the plugin twice or ask XT9 for kana.
        #[test]
        fn the_chinese_entries_share_an_engine_and_japanese_does_not() {
            assert_eq!(
                Language::Chinese.script(),
                Language::ChineseTraditional.script()
            );
            assert_ne!(Language::Japanese.script(), Language::Chinese.script());
        }

        /// The Han faces follow the language, and the Latin languages leave
        /// them alone: switching to English to type one word in the middle of a
        /// Japanese paragraph must not re-cut the kanji around it.
        #[test]
        fn each_cjk_language_names_its_own_han_convention() {
            use karyll_core::script::Region;
            assert_eq!(Language::Chinese.region(), Some(Region::Simplified));
            assert_eq!(
                Language::ChineseTraditional.region(),
                Some(Region::Traditional)
            );
            assert_eq!(Language::Japanese.region(), Some(Region::Japanese));
            assert_eq!(Language::English.region(), None);
            assert_eq!(Language::German.region(), None);
        }

        /// Both Chinese entries are the same pinyin engine on the same QWERTY
        /// arrangement — they differ only in whether the candidates are
        /// converted, because the device has exactly one Chinese dictionary.
        #[test]
        fn the_two_chinese_entries_differ_only_in_script() {
            assert_eq!(
                Language::Chinese.layout(),
                Language::ChineseTraditional.layout()
            );
            assert!(!Language::Chinese.traditional());
            assert!(Language::ChineseTraditional.traditional());
        }

        /// Nothing but Traditional asks for conversion — a Latin language
        /// reaching the converter would mean the engine was consulted for text
        /// it never produced.
        #[test]
        fn only_traditional_converts() {
            assert_eq!(Language::ALL.iter().filter(|l| l.traditional()).count(), 1);
        }

        /// The remembered letter has to survive a round trip, or a writer who
        /// left in Chinese comes back to English.
        #[test]
        fn every_language_survives_being_written_down() {
            for language in Language::ALL {
                let letter = language.letter().to_string();
                assert_eq!(Language::from_letter(&letter), language);
            }
        }
    }

    mod position {
        use super::*;

        /// The whole point of the feature: a draft opens where it was left.
        #[test]
        fn a_remembered_place_is_where_the_document_opens() {
            assert_eq!(opening_cursor_from(Some(1200), 5000), 1200);
        }

        /// **The fallback is the top.** The end would be a claim that the file
        /// is a draft being continued, which nothing supports for a document
        /// karyll has never opened.
        #[test]
        fn a_document_never_seen_before_opens_at_its_top() {
            assert_eq!(opening_cursor_from(None, 5000), 0);
            assert_eq!(opening_cursor_from(None, 0), 0);
        }

        /// The file is plain Markdown on a volume that mounts over USB, so it
        /// can have been shortened elsewhere between sessions. Clamping beats
        /// refusing to restore.
        #[test]
        fn a_stale_place_past_the_end_is_clamped() {
            assert_eq!(opening_cursor_from(Some(9000), 40), 40);
        }

        #[test]
        fn positions_survive_a_round_trip() {
            let entries = vec![
                (12, "/mnt/us/karyll/draft.md".to_string()),
                (0, "/mnt/us/karyll/你好.md".to_string()),
            ];
            assert_eq!(parse_positions(&render_positions(&entries)), entries);
        }

        /// A path is allowed to contain the separator, because the index is
        /// written first and everything after it is the path.
        #[test]
        fn a_tab_in_a_path_survives() {
            let entries = vec![(7, "/mnt/us/karyll/od\td.md".to_string())];
            assert_eq!(parse_positions(&render_positions(&entries)), entries);
        }

        #[test]
        fn a_damaged_line_is_skipped_rather_than_taking_the_file_with_it() {
            let parsed = parse_positions("not a position\n12\t/a.md\n\nxx\t/b.md\n");
            assert_eq!(parsed, vec![(12, "/a.md".to_string())]);
        }

        /// Writing a place again replaces the old one instead of stacking, or
        /// the file grows by a line per save and the stale entry is found first.
        #[test]
        fn writing_a_place_again_moves_it_to_the_front() {
            let entries = vec![(1, "/a.md".to_string()), (2, "/b.md".to_string())];
            let after = updated_positions(entries, "/b.md", 99);
            assert_eq!(
                after,
                vec![(99, "/b.md".to_string()), (1, "/a.md".to_string())]
            );
        }

        #[test]
        fn the_list_is_capped_so_it_cannot_grow_forever() {
            let mut entries = Vec::new();
            for i in 0..POSITIONS_KEPT + 20 {
                entries = updated_positions(entries, &format!("/{i}.md"), i);
            }
            assert_eq!(entries.len(), POSITIONS_KEPT);
            // Newest first, so the draft just written is the one kept.
            assert_eq!(entries[0].1, format!("/{}.md", POSITIONS_KEPT + 19));
        }
    }
}
