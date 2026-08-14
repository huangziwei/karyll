//! karyll — a Markdown writing app for the Kindle Scribe.

mod evdev;
mod font;
mod hid;
mod ime;
mod keymap;

mod render;
mod screenshot;

mod window;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use karyll_core::Document;

use font::Metrics as _;
use keymap::{Action, Mods};

fn main() -> Result<()> {
    eprintln!("karyll {} build {BUILD}", env!("CARGO_PKG_VERSION"));
    if std::env::args().nth(1).as_deref() == Some("--pair") {
        return pair();
    }
    let fonts = font::Fonts::load(read_choices())?;
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

    let path = std::env::args().nth(1).map(PathBuf::from);
    let mut doc = match &path {
        Some(p) => Document::from_text(
            &std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?,
        ),
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
    // karyll owns its lifetime: starting it displaces the stock stack, and it
    // is stopped again on exit so Audible and VoiceView only go away while the
    // editor is open.
    //
    // A failure here is reported and not fatal. The editor is still worth
    // opening — the document is readable, and a keyboard paired later is
    // picked up without a restart.
    let mut bluetooth = hid::Hid::beside_executable()?;
    match bluetooth.start() {
        Ok(()) => eprintln!("bluetooth: daemon up"),
        Err(err) => eprintln!("bluetooth: {err:#}"),
    }

    // A keyboard is not required to open. Bluetooth takes seconds to connect
    // and may not be paired at all, and refusing to start would leave a tap on
    // the tile doing nothing visible — which is worse than a page you cannot
    // type into yet. The loop picks one up whenever it appears.
    let keyboard = match evdev::Keyboard::open() {
        Ok(keyboard) => {
            eprintln!("keyboard: {}", keyboard.path().display());
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

    // Read but not yet acted on. Which reading means which way up has to come
    // off the hardware — see the UI plan's rotation item — so this session logs
    // what the sensor says and nothing more. A missing accelerometer is not
    // worth a word to the user: nothing depends on it yet.
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

    // Remembered across sessions, so a landscape writer stays in landscape.
    let orientation = read_orientation();
    let window = window::Window::open("karyll", orientation)?;
    eprintln!("window: {}x{}", window.width(), window.height());

    Editor {
        doc,
        path,
        window,
        fonts,
        theme: render::Theme::at(read_size()),
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
        orientation_checked: std::time::Instant::now(),
        focus: read_focus(),
        enabled: read_languages(),
        announcing: false,
        chrome_hidden: false,
        scroll: 0,
        keyboard_present: false,
        paired: Vec::new(),
        last_edit: None,
        dirty_since: None,
        engines: Vec::new(),
        cjk: false,
        typed: String::new(),
        preedit: String::new(),
        candidates: Vec::new(),
        punctuation: ime::Punctuation::default(),
        // The remembered one is applied below rather than assigned here.
        language: Language::English,
        strip_drawn: Vec::new(),
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
/// A signal interrupting the wait is not an error — nothing has happened yet,
/// so it just waits again.
fn wait(fds: &[std::os::unix::io::RawFd], timeout_ms: i32) -> Result<Vec<bool>> {
    let mut poll: Vec<libc::pollfd> = fds
        .iter()
        .map(|fd| libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    loop {
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
    /// にほん is composed. `typed` is what decides whether a word is under way;
    /// `preedit` is what the bar shows and what Enter commits.
    typed: String,
    preedit: String,
    candidates: Vec<String>,
    /// Which way the next quotation mark faces. Chinese quotes are directional
    /// and share one key, so the same keystroke has to alternate.
    punctuation: ime::Punctuation,
    /// What the bottom strip currently has drawn on it, so it is repainted when
    /// — and only when — it would look different. Typing damages the page above
    /// the strip, so redrawing it every keystroke would throw away the damage
    /// rectangle the page just computed.
    strip_drawn: Vec<String>,
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
                if self.contacts(taps, extent)? {
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
                if self.contacts(taps, extent)? {
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
                        eprintln!("keyboard: {} (appeared)", found.path().display());
                        keyboard = Some(found);
                        self.keyboard_present = true;
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
        let labels = ui::Overlay::Candidates(&self.candidates).labels();
        let cells = ui::overlay_cells(&mut self.fonts, rect, self.theme.body_px, &labels);
        // `None` past the last cell: inside the box but beyond the choices
        // belongs to nothing.
        ui::cell_at(&cells, x)
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
            .unwrap_or_else(|| karyll_core::word_at(&chars, self.doc.cursor()));
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
                // **A real toggle, because the chip was never inert.** It read
                // `Connected` and was documented as doing nothing, but the
                // action list beside it said `Connect` in both states — so
                // tapping a keyboard that was already connected asked the
                // daemon to connect it *again*, which tears the link down and
                // builds it back up. That is the worst of the three readings:
                // it looks like a status, it says it does nothing, and it
                // disconnects you. The daemon has had `/disconnect` all along.
                let connected = self.keyboard_present;
                (
                    ui::Item::Choice {
                        label: device.name.clone(),
                        options: vec![
                            if connected { "Disconnect" } else { "Connect" }.into(),
                            "Forget".into(),
                        ],
                        on: vec![connected, false],
                    },
                    ConfigRow::Keyboard(vec![
                        if connected {
                            KeyAction::Disconnect(device.clone())
                        } else {
                            KeyAction::Connect(device.clone())
                        },
                        KeyAction::Forget(device.clone()),
                    ]),
                )
            })
            .collect();

        items.extend(
            self.found
                .iter()
                .filter(|d| !self.paired.iter().any(|p| p.address == d.address))
                .map(|device| {
                    (
                        ui::Item::Choice {
                            label: format!("{}  ({})", device.name, device.protocol),
                            options: vec!["Pair".into()],
                            on: vec![false],
                        },
                        ConfigRow::Keyboard(vec![KeyAction::Pair(device.clone())]),
                    )
                }),
        );

        // Deliberately not started on opening Config. Scanning suspends the
        // daemon — the log says `Connection cancelled (suspend)` — which drops
        // the very keyboard being typed on. What is remembered shows without
        // asking; scanning is a choice.
        items.push((
            ui::Item::Choice {
                label: "Bluetooth".into(),
                options: vec![match self.scanning {
                    Some(started) => format!("Scanning… {}s", started.elapsed().as_secs()),
                    None => "Scan for keyboards".into(),
                }],
                on: vec![self.scanning.is_some()],
            },
            ConfigRow::Keyboard(vec![KeyAction::Scan]),
        ));
        items
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
        if !self.bluetooth.is_up()
            && let Err(err) = self.bluetooth.start()
        {
            self.scanning = None;
            return self.show_status(&format!("Bluetooth would not start: {err:#}"));
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
        if want == self.window.orientation() {
            return Ok(());
        }
        eprintln!("orientation: device turned, asking for {want:?}");
        self.window.set_orientation(want)?;
        self.touch_orientation = want;
        self.orientation_checked = std::time::Instant::now();
        // Kept only as the starting point for the next session's first paint,
        // before the sensor has said anything. It is no longer a setting.
        write_orientation(want);
        // The window manager answers with a resize, which the loop picks up.
        // Repaint anyway: if it declines, nothing else would.
        self.frame = None;
        self.paint()
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

    /// Re-read what the daemon has paired.
    fn refresh_paired(&mut self) {
        self.paired = self.bluetooth.devices().unwrap_or_default();
    }

    /// Ask the daemon to reconnect a keyboard it already knows.
    fn reconnect(&mut self, device: &hid::Device) -> Result<()> {
        self.show_status(&format!("Connecting to {}…", device.name))?;
        match self.bluetooth.connect(device) {
            Ok(()) => self.show_status(&format!("Asked {} to connect.", device.name)),
            Err(err) => self.show_status(&format!("Could not connect: {err:#}")),
        }
    }

    /// Ask the daemon to drop the link, keeping the pairing.
    ///
    /// The node goes with it, and the session notices on the next tick and says
    /// so, which is what makes this safe to offer: without that, dropping the
    /// link would leave the app holding a dead descriptor for good.
    fn disconnect(&mut self, device: &hid::Device) -> Result<()> {
        self.show_status(&format!("Disconnecting {}…", device.name))?;
        match self.bluetooth.disconnect(device) {
            Ok(()) => self.show_status(&format!("Asked {} to disconnect.", device.name)),
            Err(err) => self.show_status(&format!("Could not disconnect: {err:#}")),
        }
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
                    let _ = self.bluetooth.connect(device);
                    self.refresh_paired();
                    self.show_status("Paired. Start typing.")?;
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    self.mode = Mode::Writing;
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
            overlay: overlay(&self.candidates, self.announcing, self.language),
        }
        .paint(&mut self.window, &mut self.fonts, layout)
    }

    fn apply(&mut self, action: Action) -> Result<()> {
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
            Action::DeleteWordBack => self.doc.delete_word_back(),
            Action::DeleteWordForward => self.doc.delete_word_forward(),
            Action::DeleteToLineStart => self.doc.delete_to_line_start(),
            Action::DeleteToLineEnd => self.doc.delete_to_line_end(),
            Action::Left => self.doc.move_left(),
            Action::Right => self.doc.move_right(),
            Action::LineStart => self.doc.move_to_line_start(),
            Action::LineEnd => self.doc.move_to_line_end(),
            Action::WordLeft => self.doc.move_word_left(),
            Action::WordRight => self.doc.move_word_right(),
            Action::DocStart => self.doc.move_to_start(),
            Action::DocEnd => self.doc.move_to_end(),
            Action::ExtendDocStart => self.doc.extend_to_start(),
            Action::ExtendDocEnd => self.doc.extend_to_end(),
            Action::ExtendLeft => self.doc.extend_left(),
            Action::ExtendRight => self.doc.extend_right(),
            Action::ExtendLineStart => self.doc.extend_to_line_start(),
            Action::ExtendLineEnd => self.doc.extend_to_line_end(),
            Action::ExtendWordLeft => self.doc.extend_word_left(),
            Action::ExtendWordRight => self.doc.extend_word_right(),
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

    fn set_language(&mut self, language: Language) {
        self.abandon_composition();
        self.language = language;
        write_language(language);

        if let Some(region) = language.region() {
            self.fonts.set_region(region);
        }

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
                    self.candidates.clear();
                }
            }
            ime::Compose::Feed(c) => {
                self.typed.push(c);
                self.feed(c);
            }
            ime::Compose::Select(n) => self.select_candidate(n),
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
        self.candidates = candidates;
        self.preedit = composed.unwrap_or_else(|| self.typed.clone());
    }

    /// Accept a candidate by position, from the number row or a tap on the bar.
    ///
    /// Out of range does nothing rather than committing something else: the
    /// engine offers fewer than ten candidates often, and pressing 7 for a list
    /// of three should not insert the third.
    fn select_candidate(&mut self, n: usize) {
        let Some(text) = self.candidates.get(n).cloned() else {
            return;
        };
        if let Some(engine) = self.engine() {
            engine.commit(n);
        }
        self.typed.clear();
        self.preedit.clear();
        self.candidates.clear();
        self.insert_committed(&text);
    }

    /// Finish the word under way, the way this language finishes one.
    ///
    /// The two differ, and it is a difference in the languages rather than in
    /// the plugins:
    ///
    /// * **Chinese takes the best candidate.** Pinyin predicts as it goes, so
    ///   the top candidate is what the writer has been watching the whole time
    ///   and is what they mean by typing on.
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
        if script == ime::Script::Chinese && !self.candidates.is_empty() {
            self.select_candidate(0);
            return;
        }
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
        self.candidates.clear();
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
            overlay: overlay(&self.candidates, self.announcing, self.language),
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
            .strip()
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
    eprintln!("Starting the Bluetooth stack. This displaces the stock one until karyll exits.");
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
                // Connect straight away so the keyboard is usable now rather
                // than after the next reconnect cycle.
                if let Err(err) = bluetooth.connect(device) {
                    eprintln!("Paired, but connecting now failed ({err:#}).");
                }
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

/// The selected input source, remembered for the same reason as the layout: a
/// writer who left in Chinese comes back to Chinese.
fn language_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/language")
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

/// The composition as far as the document is concerned.
///
/// The engine holds one composition wherever it is being typed, and only the
/// page splices it into text and shifts every index after the cursor. A
/// composition bound anywhere else is not the page's to show or to count.
fn page_composition(preedit: &str, sink: Sink) -> &str {
    if sink == Sink::Page { preedit } else { "" }
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

/// What a chip in Config's Keyboard section does.
///
/// Built alongside the labels so the two cannot drift: working out what row 3
/// means by arithmetic over three concatenated lists is exactly how a tap ends
/// up forgetting a keyboard it meant to connect.
#[derive(Debug, Clone)]
enum KeyAction {
    /// Reconnect a keyboard the daemon already knows.
    Connect(hid::Device),
    /// Drop the link, keeping the pairing.
    Disconnect(hid::Device),
    /// Remove it, and its saved link key, so it can be paired afresh.
    Forget(hid::Device),
    /// Pair with something the scan turned up.
    Pair(hid::Device),
    Scan,
}

/// What Tab inserts. Two columns is what Markdown nesting expects.
const INDENT: &str = "  ";

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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

}
