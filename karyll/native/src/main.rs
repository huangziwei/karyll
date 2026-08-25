//! karyll — a Markdown writing app for the Kindle Scribe.

mod evdev;
mod font;
mod hid;
mod hyphen;
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
use keymap::{Action, Convention, Mods};

/// Stamped in by `build.sh`. Printed at start-up, naming the binary in a log.
const BUILD: &str = match option_env!("KARYLL_BUILD") {
    Some(stamp) => stamp,
    None => "dev",
};

/// The document named on the command line. A missing file opens empty; any
/// other error is returned, leaving an unreadable draft untouched.
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
    let fonts = font::Fonts::load(read_choices())?;
    // The faces found, named in the log.
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

    // [`DOCUMENTS`] sits outside the extension, and no install step creates it.
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

    // The tag goes in ahead of the daemon that creates the node. karyll reads
    // the node directly; a failure here costs the tag alone — see [`udev`].
    match udev::ensure() {
        Ok(udev::Outcome::Present) => {}
        Ok(udev::Outcome::Installed) => eprintln!("udev: installed {}", udev::PATH),
        Err(err) => eprintln!("udev: {err:#} — the keyboard will reach karyll only"),
    }

    // Spawned and left to come up on its own — see [`hid::Hid::poll_up`].
    // `set_keep_alive` runs ahead of `start`, which reads it.
    let mut bluetooth = hid::Hid::beside_executable()?;
    bluetooth.set_keep_alive(read_keep_bluetooth());
    if let Err(err) = bluetooth.start() {
        eprintln!("bluetooth: {err:#}");
    }

    // A keyboard is not required to open. Bluetooth takes seconds to connect
    // and may not be paired at all. The loop picks one up whenever it appears.
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

    // The way in and out of karyll before a keyboard is paired.
    let touch = match touch::Touchscreen::open() {
        Ok(touch) => Some(touch),
        Err(err) => {
            eprintln!("touch: unavailable ({err:#}) — the menu will be unreachable");
            None
        }
    };

    // A second pointer: it places the cursor between two characters. Optional.
    let pen = match pen::Pen::open() {
        Ok(pen) => Some(pen),
        Err(err) => {
            eprintln!("pen: unavailable ({err:#})");
            None
        }
    };

    // Opened ahead of the window, which opens the way this reads. A device
    // with no accelerometer takes the orientation from Config.
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

    let theme = render::Theme::at(read_size(), read_margin()).breaking(read_rules());

    let orientation = read_orientation(accel.as_ref());
    let mut window = window::Window::open("karyll", orientation)?;
    // A no-op on a panel with no colour.
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
        convention: read_convention(),
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
        panel_focus: None,
        find: None,
        polled: None,
        holding: None,
        touch_orientation: orientation,
        turns_itself: accel.is_some(),
        orientation_checked: std::time::Instant::now(),
        focus: read_focus(),
        hyphenate: read_hyphenate(),
        enabled: read_languages(),
        notice: None,
        chrome_hidden: false,
        scroll: 0,
        keyboard_present: false,
        paired: Vec::new(),
        connected: None,
        last_edit: None,
        dirty_since: None,
        engines: Vec::new(),
        korean: ime::Korean::default(),
        cjk: false,
        typed: String::new(),
        preedit: String::new(),
        candidates: Vec::new(),
        page: 0,
        pages: Vec::new(),
        punctuation: ime::Punctuation::default(),
        // The remembered one is applied below.
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
/// (negative for no timeout), and report which are ready. An interrupting
/// signal reports nothing ready.
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
        // Any `revents`, `POLLIN` included. A destroyed `/dev/input/eventN`
        // reports `POLLHUP`/`POLLERR` and never readable, and the read that
        // follows is what drops the descriptor.
        return Ok(poll.iter().map(|p| p.revents != 0).collect());
    }
    let err = std::io::Error::last_os_error();
    if err.kind() != std::io::ErrorKind::Interrupted {
        return Err(err).context("poll keyboard and display");
    }
    // Nothing is ready, and the caller reads the flag [`on_signal`] set.
    Ok(vec![false; poll.len()])
}

/// Set when the editor has been asked to stop. The run loop reads it, saves,
/// stops the daemon and releases the screensaver latch.
static STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Async-signal-safe by construction: one relaxed store and nothing else.
extern "C" fn on_signal(_: libc::c_int) {
    STOPPING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Ask to be told, in place of being killed outright.
fn catch_signals() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe { libc::signal(signal, on_signal as *const () as libc::sighandler_t) };
    }
}

/// An input source: what `Ctrl+Space` and the language button move between.
/// One cycle carries the layouts and the input methods together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Language {
    #[default]
    /// English. [`Language::layout`] names the layout it is written on.
    English,
    German,
    Chinese,
    /// Traditional Chinese: the Simplified pinyin engine, with its own
    /// converter applied to every candidate.
    ChineseTraditional,
    /// Japanese: romaji typed, kana and kanji out. Omron iWnn behind the same
    /// plugin ABI XT9 uses.
    Japanese,
    /// Korean: 두벌식 typed, Hangul out. A syllable is arithmetic on a code
    /// point, composed in [`crate::ime`].
    Korean,
}

impl Language {
    const ALL: [Language; 6] = [
        Language::English,
        Language::German,
        Language::Chinese,
        Language::ChineseTraditional,
        Language::Japanese,
        Language::Korean,
    ];

    /// What the button says: one character each, in the language's own script.
    fn label(self) -> &'static str {
        match self {
            Language::English => "EN",
            Language::German => "DE",
            Language::Chinese => "简",
            Language::ChineseTraditional => "繁",
            Language::Japanese => "日",
            Language::Korean => "한",
        }
    }

    fn letter(self) -> char {
        match self {
            Language::English => 'e',
            Language::German => 'd',
            Language::Chinese => 'c',
            Language::ChineseTraditional => 't',
            Language::Japanese => 'j',
            Language::Korean => 'k',
        }
    }

    fn from_letter(s: &str) -> Language {
        match s.trim() {
            "d" => Language::German,
            "c" => Language::Chinese,
            "t" => Language::ChineseTraditional,
            "j" => Language::Japanese,
            "k" => Language::Korean,
            _ => Language::English,
        }
    }

    /// Whether the engine should convert its candidates to Traditional.
    fn traditional(self) -> bool {
        matches!(self, Language::ChineseTraditional)
    }

    /// Which input method's rules apply, and which engine to load. `None` for
    /// the languages typed straight onto the page.
    fn script(self) -> Option<ime::Script> {
        match self {
            Language::Chinese | Language::ChineseTraditional => Some(ime::Script::Chinese),
            Language::Japanese => Some(ime::Script::Japanese),
            Language::Korean => Some(ime::Script::Korean),
            Language::English | Language::German => None,
        }
    }

    /// Which regional convention the Han faces follow under this language.
    /// `None` holds the convention the document carries.
    fn region(self) -> Option<karyll_core::script::Region> {
        match self {
            Language::Chinese => Some(karyll_core::script::Region::Simplified),
            Language::ChineseTraditional => Some(karyll_core::script::Region::Traditional),
            Language::Japanese => Some(karyll_core::script::Region::Japanese),
            Language::English | Language::German | Language::Korean => None,
        }
    }

    /// Which font row in Config this language is set from. One row per writing
    /// system: Latin, one per Han convention, and Hangul.
    fn font_group(self) -> font::Group {
        match self.region() {
            Some(region) => font::Group::Han(region),
            None if self == Language::Korean => font::Group::Hangul,
            None => font::Group::Latin,
        }
    }

    /// The next input source among those in `enabled`, in `ALL`'s order. A
    /// source outside `enabled` cycles forward from where it sits in `ALL`.
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

    /// The keyboard arrangement this language is written on. A language names
    /// its layout, and no separate control selects one. Pinyin, romaji and
    /// 두벌식 are defined against the QWERTY letter arrangement.
    fn layout(self) -> keymap::Layout {
        match self {
            Language::German => keymap::Layout::German,
            _ => keymap::Layout::Us,
        }
    }
}

/// Reports an accelerometer code [`orientation::Orientation::from_tilt`] does
/// not name, once per distinct code. Silence is the sensor's resting state:
/// it reports transitions, and a settling burst on power-up.
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

/// What the screen is showing. Every panel is modal and takes the whole
/// surface.
enum Mode {
    Writing,
    /// The file list: every `.md` under [`DOCUMENTS`]. New and Rename sit on
    /// the strip.
    Files(Vec<Listing>),
    /// Typing a name. Holds what has been typed and what it is for.
    Naming {
        for_new: bool,
        name: String,
    },
    /// Settings: the keyboard, the input sources, and the faces they set in.
    /// Pairing is the first section.
    Config,
    /// What the keys and the glass do. Reached from the strip and from
    /// `Ctrl`/`⌘` + `H`.
    Help,
    /// The headings of the open document, in order, to jump between. Read when
    /// the panel opens.
    Outline(Vec<Section>),
}

/// One heading, as the outline lists it.
#[derive(Debug, Clone)]
struct Section {
    /// `#` through `######`, for the indent.
    level: u8,
    /// The heading with its markup taken out, by
    /// [`karyll_core::markdown::plain`].
    text: String,
    /// Where its line starts, in document characters. The jump lands here.
    at: usize,
    /// How many words are under it, up to the next heading of any level.
    words: usize,
}

/// Where typed text lands. One IME, one composition and one candidate box
/// serve the page, the find bar and a filename alike; this names which of them
/// a finished word goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sink {
    /// The document.
    Page,
    /// The find bar's query.
    Find,
    /// The filename being typed.
    Name,
}

/// What offering a keystroke to an input method did with it.
/// [`Composed::Finished`] is Korean's: [`ime::Compose::Finish`] commits a
/// half-typed syllable, and the key goes on to the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Composed {
    /// Handled. The editor must not see this key.
    Took,
    /// Not handled, and the composition was committed on the way out.
    Finished,
    /// Untouched.
    Passed,
}

struct Editor {
    doc: Document,
    /// The word list for the current regional convention. Han selection falls
    /// back to whole runs with `None`.
    dict: Option<Dict>,
    dict_region: Option<Region>,
    path: Option<PathBuf>,
    window: window::Window,
    fonts: font::Fonts,
    theme: render::Theme,
    mods: Mods,
    /// Which of the two keys beside the space bar this keyboard calls ⌘.
    /// Remembered across sessions.
    convention: Convention,
    /// What was last drawn: a keystroke presents one line and not the whole
    /// page. `None` until the first paint.
    frame: Option<render::Frame>,
    /// The faces the last paint found in this document: page movement sizes
    /// its rows the same way the page does.
    roles: Vec<karyll_core::script::Role>,
    /// Column the arrow keys are holding across a run of vertical moves.
    goal: Option<f32>,
    /// Cut and copied text, in memory for the length of the session.
    clipboard: String,
    /// Where a finger went down on the page. There are no motion events: this
    /// and the lift position are the whole of a drag.
    touch_down: Option<(u16, u16)>,
    /// When and where the page was last tapped, for spotting the second tap of
    /// a pair.
    last_tap: Option<(std::time::Instant, (u16, u16))>,
    /// The Bluetooth stack. Stopped when the editor drops: the stock stack
    /// comes back when karyll exits.
    bluetooth: hid::Hid,
    mode: Mode,
    /// When the running scan began, if one is.
    scanning: Option<std::time::Instant>,
    /// What the running scan has turned up outside `paired`. Cleared when
    /// Config opens.
    found: Vec<hid::Device>,
    /// Which page of a long panel list is showing. Reset when a panel opens.
    panel_page: usize,
    /// Which line of the open panel the keyboard is on. `None` until a key is
    /// pressed on the panel; a tap sets it to what was touched.
    panel_focus: Option<PanelFocus>,
    /// The search, while one is open.
    find: Option<Find>,
    /// When the daemon was last asked. The loop ticks five times a second and
    /// the daemon is single-threaded.
    polled: Option<std::time::Instant>,
    /// What is inverted under the finger, and when it was shown.
    holding: Option<(Target, std::time::Instant)>,
    /// How the panel maps onto the window, following what the window manager
    /// reports. The window carries the request.
    touch_orientation: orientation::Orientation,
    /// Whether this device has an accelerometer. [`SCREENS`] appears in Config
    /// where it does not.
    turns_itself: bool,
    /// When that was last checked, since asking costs a subprocess.
    orientation_checked: std::time::Instant,
    /// Whether the page is set back around the sentence being written, drawn
    /// in `window::QUIET`. Remembered across sessions.
    focus: bool,
    /// Whether a word may be divided at the end of a line. The dictionary it
    /// reaches for follows the language.
    hyphenate: bool,
    /// Which input sources `Ctrl+Space` cycles through, in `Language::ALL`'s
    /// order. Never empty — see [`read_languages`].
    enabled: Vec<Language>,
    /// What floats beside the caret until the next keystroke: the input
    /// source, or the case Caps Lock gives.
    notice: Option<&'static str>,
    /// Whether the action strip is out of the way while writing. Never true
    /// without a keyboard — see [`Editor::strip_visible`].
    chrome_hidden: bool,
    /// How far the page is scrolled down, in pixels. Held here, which lets the
    /// caret travel down the page while the text holds its position.
    scroll: i32,
    /// Whether a keyboard is attached, for the panel to report.
    keyboard_present: bool,
    /// Keyboards the daemon knows, refreshed when the panel opens and after
    /// anything changes them. Hit-testing a tap reads this, never the daemon.
    paired: Vec<hid::Device>,
    /// Which of `paired` holds the link, from [`hid::Hid::connected`] and
    /// refreshed with it. `keyboard_present` says a keyboard is attached; this
    /// says which row it is.
    connected: Option<String>,
    /// When the document was last changed, and when it first went unsaved.
    /// Both drive autosave; see [`Editor::poll_autosave`].
    last_edit: Option<std::time::Instant>,
    dirty_since: Option<std::time::Instant>,
    /// The predictor plugins, each loaded the first time its language is asked
    /// for and kept for the session. A plugin that failed to load is absent,
    /// and Latin reaches none of them.
    engines: Vec<(ime::Script, Box<dyn ime::Ime>)>,
    /// The Hangul syllable being typed: a few bytes of state, with no plugin
    /// and no dictionary behind them.
    korean: ime::Korean,
    /// Whether keys are going to an input method at all. Ctrl+Space toggles it.
    cjk: bool,
    /// The keys sent towards the current word, what the engine makes of them,
    /// and what it offers. `preedit` is what the bar shows and what `Enter`
    /// commits; `typed` is what `F10` gives back.
    typed: String,
    preedit: String,
    candidates: Vec<String>,
    /// Which page of them is on the bar, walked by the arrows while composing.
    page: usize,
    /// Where each page starts, from [`ui::candidate_pages`]. A page is as many
    /// as the panel can show, and this decides which candidate a digit picks.
    pages: Vec<usize>,
    /// Which way the next quotation mark faces. Chinese quotes are directional
    /// and share one key.
    punctuation: ime::Punctuation,
    /// What the bottom strip has drawn on it. The strip is repainted where
    /// this differs, which leaves the page's damage rectangle intact.
    strip_drawn: Vec<String>,
    /// Set where the strip changed without a tap asking for it. The next tap
    /// on it is spent looking. See `strip_drawn`.
    strip_changed: bool,
    /// What the status line beside them said. It holds the room the buttons
    /// leave, and nothing hit-tests it.
    status_drawn: String,
    /// When a key or a finger last arrived, and whether the screensaver latch
    /// is held. The pair writes the latch on its two transitions;
    /// `power::prevent_screensaver` shells out to `lipc-set-prop`.
    last_input: std::time::Instant,
    holding_awake: bool,
    /// Whether the next paint lands somewhere chosen from the outline. One
    /// paint's worth, cleared by the paint that honours it.
    landing: bool,
    /// The document whose Delete chip has been tapped once. The second tap is
    /// honoured on the same path, whatever the list has done underneath.
    arming: Option<PathBuf>,
    /// The selected input source: which keyboard, and whether Chinese input is
    /// on. Remembered in `var/language`.
    language: Language,
}

impl Editor {
    /// Run a writing session, then save, remember the position and release the
    /// screensaver latch. `session` has several ways out and this wraps all of
    /// them; `panic = "abort"` leaves no `Drop` to use.
    fn run(
        mut self,
        keyboard: Option<evdev::Keyboard>,
        touch: Option<touch::Touchscreen>,
        pen: Option<pen::Pen>,
        accel: Option<evdev::Accelerometer>,
    ) -> Result<()> {
        // The keyboard is grabbed, and typing reaches no idle timer.
        self.note_input();
        let result = self.session(keyboard, touch, pen, accel);
        // Autosave fires a couple of seconds after the last keystroke, and a
        // session ending inside that window has not written. Reported after
        // `result` is in hand, which keeps the session's own error.
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
            // Asked to stop — see [`STOPPING`]. Out through `run`'s own exit.
            if STOPPING.load(std::sync::atomic::Ordering::Relaxed) {
                eprintln!("signal: asked to stop");
                return Ok(());
            }
            // Drain X first. x11rb decodes into its own buffer, and an event
            // it has read leaves nothing on the socket for `poll` to report.
            match self.window.drain_events()? {
                window::Surface::Gone => return Ok(()),
                // A rotation arrives as a resize, and the page is laid out
                // again from nothing.
                window::Surface::Live { resized: true, .. } => {
                    eprintln!("window: {}x{}", self.window.width(), self.window.height());
                    self.frame = None;
                    // The candidate bar is paged by the width, which the paint
                    // below leaves alone.
                    let candidates = std::mem::take(&mut self.candidates);
                    self.set_candidates(candidates);
                    self.paint()?;
                }
                window::Surface::Live { expose: true, .. } => self.window.refresh()?,
                window::Surface::Live { .. } => {}
            }

            // A held finger produces no events: waiting has to time out for
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

            // Log-only, and before anything can repaint: this run establishes
            // what the sensor's numbers mean. A read error drops the device.
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

            // A running scan reports itself here, not from a blocking sleep:
            // the panel keeps repainting and taps are not queued behind it.
            self.poll_scan()?;
            self.poll_bluetooth()?;
            self.poll_orientation();
            self.poll_autosave();
            self.poll_sleep();

            // A read that fails drops the descriptor. `wait` reports a hangup
            // as ready, and a descriptor that is ready and keeps failing is a
            // tick that never blocks.
            let mut lost_touch = false;
            if let (Some(slot), Some(device)) = (touch_slot, touch.as_mut())
                && ready.get(slot).copied().unwrap_or(false)
            {
                let extent = (device.x_extent, device.y_extent);
                // A finger resets the framework's own idle timer: this is not
                // what keeps the page up — it is what takes the latch back
                // after an idle spell, and the typing that follows is covered.
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
                // Only for a nib that touched down. A pen hovering over the
                // page reports its position continuously, and a pen sitting in
                // a hand holds the screensaver off.
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
                // On a tick, not on every wake. This reads
                // `/proc/bus/input/devices` and tries to open a node, and a pen
                // hovering reports its position a hundred times a second.
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
                        // Config stays open: pairing succeeded, and its
                        // Keyboard section is where that shows.
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
                    // looking again, in place of dying mid-sentence.
                    eprintln!("keyboard: lost ({err:#}) — looking for another");
                    keyboard = None;
                    self.keyboard_present = false;
                    // A modifier held as the link drops has no release to
                    // follow, and leaves every key reading as a chord.
                    self.mods = keymap::Mods::default();
                    // Say so. Config draws this keyboard's state from the flag
                    // just cleared, and a page reading `Disconnect` for a
                    // keyboard that has gone says the opposite of what happened.
                    self.paint()?;
                    continue;
                }
            };

            // While naming, keys build the name. In any other panel they do
            // nothing — that is a finger's screen.
            if matches!(self.mode, Mode::Naming { .. }) {
                // CJK gets first refusal here too: every letter reaches the
                // engine while the mode is on, and a document can be named in
                // Chinese. See [`Sink`].
                let mut dirty = false;
                for event in batch {
                    let Some(action) = self.pressed_action(&event) else {
                        continue;
                    };
                    match self.compose_key(&action) {
                        Composed::Took => {
                            dirty = true;
                            continue;
                        }
                        Composed::Finished => dirty = true,
                        Composed::Passed => {}
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
            // A panel is worked from the keyboard or from the glass. Up and
            // down walk the lines that do something, left and right move along
            // a line's chips, Enter takes the mark, and Esc is the way out.
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
                        Action::Up => self.move_focus(false)?,
                        Action::Down => self.move_focus(true)?,
                        // These do the work of the page keys, which a compact
                        // Bluetooth keyboard does not carry.
                        Action::Left => self.move_chip(false)?,
                        Action::Right => self.move_chip(true)?,
                        Action::PageUp => self.page_or_nothing(true)?,
                        Action::PageDown => self.page_or_nothing(false)?,
                        Action::Newline => self.take_focus()?,
                        Action::Backspace => self.delete_focus()?,
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
            // mode — the document is on screen and scrolled to the hits — and
            // the keys build the query, not the draft.
            if self.find.is_some() {
                // CJK gets first refusal here as it does on the page, and by
                // the same call: without it the bar takes pinyin letters
                // literally. See [`Sink`].
                let mut dirty = false;
                for event in batch {
                    let Some(action) = self.pressed_action(&event) else {
                        continue;
                    };
                    match self.compose_key(&action) {
                        Composed::Took => {
                            dirty = true;
                            continue;
                        }
                        Composed::Finished => dirty = true,
                        Composed::Passed => {}
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
                // Writing puts the chrome away: a freshly opened document has
                // a toolbar, and a document being typed into has none.
                self.set_chrome_hidden(true);
                // Chinese input gets first refusal. It only takes keys it has
                // a use for: English typing is untouched even while the engine
                // is switched on.
                if self.compose_key(&action) == Composed::Took {
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
    /// bound to nothing. One place: the page, the find bar, the name prompt and
    /// the panels all track the modifier state and resolve the same way.
    fn pressed_action(&mut self, event: &evdev::KeyEvent) -> Option<Action> {
        // Caps Lock latches in `track` and carries on to its binding: no lamp
        // on a Bluetooth keyboard shows the latch.
        let latch = event.code == keymap::code::CAPSLOCK;
        if (self.mods.track(event.code, event.pressed) && !latch) || !event.pressed {
            return None;
        }
        // The writer is here. Every key comes through this, and a keystroke is
        // the one input the framework's own idle timer cannot see: the keyboard
        // is grabbed.
        self.note_input();
        // The bindings are written for ⌘ against the space bar. `resolve` hands
        // them that pair from a keyboard sending either.
        let mods = self.convention.resolve(self.mods);
        let Some(action) = keymap::action(event.code, mods, self.language.layout()) else {
            // Buttons are not keys and name no binding karyll lacks.
            if event.code < keymap::code::BTN_MISC {
                // Named: a key that does nothing reads like one that never
                // arrived. Compact keyboards have no `Home`, and whether
                // `fn`+← arrives as code 102 varies by keyboard.
                eprintln!("key: {} unbound ({mods:?})", event.code);
            }
            return None;
        };
        // A notice goes with the next keystroke: anything raising no new one
        // means the writer has read it. Here, since the panels take keys too
        // and a notice raised on one is cleared here.
        if !matches!(action, Action::CycleLanguage | Action::CapsLock(_)) {
            self.notice = None;
        }
        Some(action)
    }

    /// The strip cells for the current mode, left to right. `&mut self` for one
    /// reason: whether there is a `More` to offer depends on how many rows fit,
    /// which is measured from the face in use.
    fn strip_wanted(&mut self) -> Vec<Bar> {
        // The find bar takes the strip: its cells are the right size for a
        // finger, and a second band pushes the page up and reflows it on every
        // letter typed into the search.
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
            // Ordered by how often a finger reaches for it. The status line
            // reports the autosave, and Outline is `Ctrl`/`⌘`+`Shift`+`O`.
            Mode::Writing => vec![Bar::Exit, Bar::Files, Bar::Config, Bar::Help],
            Mode::Naming { .. } => vec![Bar::Cancel],
            // The Files panel's own actions, on the strip and not among the
            // documents they act on.
            Mode::Files(_) => vec![Bar::Done, Bar::New, Bar::Rename],
            Mode::Config | Mode::Help | Mode::Outline(_) => vec![Bar::Done],
        };
        // Only when there is somewhere to go. Both directions wrap, and both
        // are there together: the strip holds its width under a finger paging
        // through a list.
        if self.pages() > 1 {
            cells.extend([Bar::PageBack, Bar::PageAt, Bar::PageOn]);
        }
        cells
    }

    /// The strip's cells and their words together, cut to the panel width.
    /// Drawing, hit-testing and press feedback all read this one answer. A
    /// narrow panel gives up the longer words, then the readouts.
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

    /// What the editor knows that the strip has to say. Gathered once, which
    /// leaves the fitting below free of the editor and testable against a stub
    /// metric.
    fn readouts(&mut self) -> Readouts {
        Readouts {
            // The two numbers, not a copy of the search. `hits` is one range
            // per occurrence, and cloning it to read a length copies thousands
            // off a common word, on every keystroke.
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
            // Against the cell's own `Bar`: the field a label is written for
            // is the field that cell *is*. A position is a second statement of
            // the bar's order.
            for cell in fields {
                if let Some(which) = Field::of(bars[cell]) {
                    labels[cell] = self.find_field(which, room);
                }
            }
        }
        (bars, labels)
    }

    /// The strip's labels, for drawing. Only the find bar's change: the two
    /// fields say what has been typed, the count says what was found, and `All`
    /// says which tap it is on.
    fn strip_labels(&mut self) -> Vec<String> {
        self.strip_fitted().1
    }

    /// Which strip cells take whatever width the others leave. Only the find
    /// bar has them: a field grows as it is typed into, and packing one like a
    /// label shoves `Previous`, `Next` and `Done` along under the finger.
    fn strip_stretch(&mut self) -> Vec<usize> {
        stretch_cells(&self.strip_fitted().0)
    }

    /// The status line: what this document is, how long it is, and whether it
    /// is on disk. Autosave has no other report. Empty for a panel and for the
    /// find bar, each of which has taken the room.
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
        // A document is written out a couple of seconds after the typing
        // stops; `not yet saved` names the gap.
        let saved = if self.doc.is_dirty() {
            "not yet saved"
        } else {
            "saved"
        };
        format!("{name}  ·  {words}  ·  {saved}")
    }

    /// What the bottom strip says. Drawing, hit-testing and press
    /// feedback all ask here.
    fn strip_cells(&mut self) -> Vec<String> {
        self.strip_labels().iter().map(|l| bracket(l)).collect()
    }

    /// What one of the find bar's fields says. A rule stands for the caret, on
    /// the field taking keys. The composition sits ahead of the caret, shown
    /// and not searched — see [`Editor::research`]. Trimmed into `room`.
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

    /// The panel's geometry, sized from the Latin face alone. Rows are `lh * 2`
    /// with a 96 px floor, which leaves room for a Han label; asking the Han
    /// faces here loads 10 MB of them to open the Files panel.
    fn layout(&mut self) -> ui::Layout {
        let text = self.fonts.line_height(ui::TEXT_PX, font::LATIN_ROW) as u16;
        let title = self.fonts.line_height(ui::TITLE_PX, font::LATIN_ROW) as u16;
        ui::Layout::compute(text, title, self.window.height())
    }

    /// Raw panel coordinates in, window coordinates out. Split out from
    /// `target`: a tap on the page is answered by a character index, and both
    /// need this first.
    fn point(
        &mut self,
        raw_x: i32,
        raw_y: i32,
        extent: (touch::Extent, touch::Extent),
    ) -> (u16, u16) {
        let size = (self.window.width(), self.window.height());
        // Scale into the panel's own pixel space, which is always portrait,
        // before rotating into the window. Scaling against the window stretches
        // one axis and squashes the other whenever the two differ.
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
        // the chrome hidden `page_bottom` is the foot of the panel: nothing is
        // ever on a strip that is not drawn.
        let bottom = self.page_bottom();
        let hit = if y >= bottom && y >= layout.strip_top {
            let cells = self.strip_cells();
            let stretch = self.strip_stretch();
            let fonts = &mut self.fonts;
            let bounds = ui::cell_bounds(size.0, &cells, &stretch, |s| {
                ui::measure(fonts, s, ui::TEXT_PX) as u16
            });
            // `None` past the last cell. The buttons are packed at their own
            // width: most of this band is the status line, and a tap on a
            // line that only reports must not run the button nearest it.
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

    /// Whether a window y is on the page or on the chrome. Only while
    /// writing: with a panel open the same pixels are a list of files or
    /// keyboards, and those rows outrank the document behind them.
    fn writing_area(&mut self, y: u16) -> bool {
        matches!(self.mode, Mode::Writing) && y < self.page_bottom()
    }

    /// Which candidate a point falls on, if the box is on screen and the point
    /// is inside it. Measured against the box the last paint drew, the way the
    /// tap test works for the strip.
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

    /// Whether the action strip is on screen. Without a keyboard the strip is
    /// the only way out of the app, which overrides the hidden flag. Composing
    /// does not: a Chinese word leaves the chrome away.
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
    /// anything. The page grows and shrinks with it: every row moves and there
    /// is no damage rectangle.
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
        // The buffer the frame was laid out from: the glyph under the finger
        // is the one that was drawn there.
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
        .composing(preedit)
        .hyphenating(self.dictionary());
        let index = render::index_at_point(&page, &mut self.fonts, &frame, x as f32, y as f32);
        self.frame = Some(frame);
        // Back to document space: the caller is going to move the cursor with
        // it, and the cursor lives in the document.
        index.map(|i| self.document_index(i))
    }

    /// What a finger — or the nib — lifting off the page means. A contact is
    /// only Down and Up, each carrying a position; pressing at one place and
    /// lifting at another is a drag, painted once on the lift.
    fn tap_text(&mut self, x: u16, y: u16) -> Result<()> {
        let down = self.touch_down.take();
        let far = |a: (u16, u16), b: (u16, u16)| {
            a.0.abs_diff(b.0) > TOUCH_SLOP || a.1.abs_diff(b.1) > TOUCH_SLOP
        };
        let dragged = down.is_some_and(|d| far(d, (x, y)));

        // A tap in a margin moves the page, asked before the character under
        // it. A drag is exempt: a run ending past the last word of a line
        // selects.
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
        // front of the writer, and shift-click is the habit.
        if self.mods.shift {
            self.doc.extend_to(index);
            self.last_tap = None;
            return self.paint();
        }

        // A second tap in the same place selects the word under it. Both the
        // interval and the distance both have to match: a quick tap somewhere
        // else is not the second half of a double-tap.
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

    /// Move the page, the way a tap on a margin asks. The four keys the margins
    /// stand in for, and not a second set of movements: a finger and `PageUp`
    /// leave the document in the same place.
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
    /// them asked to leave. One handler for the glass, whatever touched it: the
    /// finger panel and the pen both arrive here as the same three contacts.
    fn contacts(
        &mut self,
        taps: Vec<touch::Touch>,
        extent: (touch::Extent, touch::Extent),
    ) -> Result<bool> {
        for tap in taps {
            match tap {
                touch::Touch::Down { x, y } => self.pressed(x, y, extent)?,
                touch::Touch::Up { x, y } => {
                    // Restore first, and synchronously: the button is visibly
                    // released before whatever it does repaints over it.
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
        // A finger on the page is remembered, not drawn: where it lands means
        // something once it lifts, and a row of prose is not a control.
        if self.writing_area(y) {
            self.touch_down = Some((x, y));
            return Ok(());
        }
        self.touch_down = None;
        let Some(target) = self.target(x, y) else {
            return Ok(());
        };
        self.draw_target(target, true)?;
        // Timed from after the server has it: the hold is a hold on screen, not
        // on a queued request.
        self.holding = Some((target, std::time::Instant::now()));
        Ok(())
    }

    /// Put back whatever was inverted. A quick tap arrives as Down and Up in
    /// the same read, and the inverted state is held briefly to reach a panel
    /// update.
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
        // Resolved by the same function that decided what to invert. A second
        // copy of the mapping puts the invert on one control and runs another:
        // after a 180° flip the right button lights and the wrong one fires.
        let (x, y) = self.point(raw_x, raw_y, extent);
        // Any touch brings the chrome back — the reveal is the whole glass.
        let waking = !self.strip_visible();
        self.set_chrome_hidden(false);
        // A tap landing where the strip is about to appear reveals it and stops
        // there: with the chrome away those rows are blank page. A strip that
        // changed its own mind is spent here too, and any tap clears it.
        let changed = std::mem::take(&mut self.strip_changed);
        if (waking || changed) && y >= self.layout().strip_top {
            self.touch_down = None;
            return self.paint().map(|()| false);
        }
        // The candidate box floats over the page, and it is asked before the
        // page is: a tap on a candidate is choosing it, not moving the cursor to
        // whatever prose the box happens to be covering.
        if let Some(n) = self.candidate_at(x, y) {
            self.touch_down = None;
            self.select_candidate(n);
            self.paint()?;
            return Ok(false);
        }
        // Anything else ends the word under way, the rule [`ime::compose`]
        // applies to an arrow or a chord. A composition belongs to the field it
        // was started in, and a tap is the only way out of one holding it.
        self.abandon_composition();
        if self.writing_area(y) {
            self.tap_text(x, y)?;
            // A tap that resolved to no character repaints nothing, leaving a
            // revealed strip undrawn. The dropped frame says the reveal has
            // not been honoured.
            if self.frame.is_none() {
                self.paint()?;
            }
            return Ok(false);
        }
        // A finger that went down on the page and lifted on the strip: the
        // press is spent, and left set it makes the *next* tap on the page a
        // drag from wherever that one started.
        self.touch_down = None;
        let Some(target) = self.target(x, y) else {
            return Ok(false);
        };
        let row = match target {
            Target::Strip(cell) => {
                // The cells as drawn, not as wanted: a strip that gave up a
                // readout to fit is a strip whose fourth cell is not the one
                // an unfitted list dispatches.
                let cells = self.strip_fitted().0;
                return self.strip_action(cells[cell.min(cells.len() - 1)]);
            }
            // **Page-relative in, absolute out.** A tap reports the row it
            // landed on within what is drawn; the lists it dispatches against
            // are the whole thing.
            Target::Row(row) => self.page_window().start + row,
            Target::Option(item, option) => {
                let item = self.page_window().start + item;
                self.focus_at(item, option);
                self.take_chip(item, option)?;
                return Ok(false);
            }
        };
        self.focus_at(row, 0);
        self.take_row(row)?;
        Ok(false)
    }

    /// Put the keyboard where the finger just was. One place in the list,
    /// however it is being worked: a writer who taps a document and reaches for
    /// the arrows carries on from the row they touched.
    fn focus_at(&mut self, row: usize, chip: usize) {
        if self.takes_focus().get(row) == Some(&true) {
            self.panel_focus = Some(PanelFocus { row, chip });
        }
    }

    /// Open what a line of the list stands for. Absolute indices: a row is
    /// dispatched against the whole list, not against the page of it drawn.
    fn take_row(&mut self, row: usize) -> Result<()> {
        match &self.mode {
            // Every row is a document: taking one opens it. There is no
            // arithmetic past the end of the list to get wrong: there is
            // nothing past the end of it.
            Mode::Files(files) => {
                if let Some(listing) = files.get(row) {
                    let path = listing.path.clone();
                    self.open(path)?;
                }
            }
            // Every row is a heading, and taking one goes there. The panel
            // holds the list it drew: a jump cannot land on a heading that has
            // moved.
            Mode::Outline(sections) => {
                if let Some(at) = sections.get(row).map(|s| s.at) {
                    self.jump_to(at)?;
                }
            }
            // Every line of Config is a chip: a bare row is a heading or a
            // label — nothing to run. Every line of Help is a fact, with
            // nothing to run either.
            Mode::Config | Mode::Help | Mode::Writing | Mode::Naming { .. } => {}
        }
        Ok(())
    }

    /// Take one chip of a line.
    fn take_chip(&mut self, item: usize, option: usize) -> Result<()> {
        match self.mode {
            Mode::Config => self.config_action(item, option),
            // A file row's only chip is Delete.
            Mode::Files(_) => self.arm_or_delete(item),
            Mode::Help | Mode::Outline(_) | Mode::Writing | Mode::Naming { .. } => Ok(()),
        }
    }

    /// Remove the document the keyboard is on. Twice, the rule the chip
    /// follows, and the chip is what asks.
    fn delete_focus(&mut self) -> Result<()> {
        let Some(focus) = self
            .panel_focus
            .filter(|_| matches!(self.mode, Mode::Files(_)))
        else {
            return Ok(());
        };
        self.arm_or_delete(focus.row)
    }

    /// A document's Delete: arm it, or carry it out. Two presses, with no undo
    /// and no bin. Arming another document disarms the first: the second press
    /// means aiming at the same document twice.
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

        // The open document is disowned before anything else runs: `open` saves
        // a dirty buffer on the way out, and the buffer holds every word of the
        // file just removed.
        if self.path.as_deref() == Some(path) {
            self.path = None;
            self.doc.mark_saved();
            let next = match list_documents().into_iter().next() {
                Some(listing) => listing.path,
                // Nothing left. A fresh one, the rule the launcher follows on
                // an empty directory: a screen with no file behind it cannot be
                // saved into.
                None => {
                    let path = new_document();
                    let _ = std::fs::write(&path, "");
                    path
                }
            };
            self.load(next)?;
        }

        // Read again, in place of dropping the row: the words and ages of
        // everything else are a snapshot from when the panel opened, and one of
        // them may be the document just opened in place of this.
        self.mode = Mode::Files(list_documents());
        self.panel_page = 0;
        self.panel_focus = None;
        self.paint()
    }

    /// Show what the keys and the glass do.
    fn open_help(&mut self) -> Result<()> {
        self.mode = Mode::Help;
        self.panel_page = 0;
        self.panel_focus = None;
        self.paint()
    }

    /// Show the document's headings, on the page holding the one the cursor is
    /// in. Not page 1: forty sections into a draft, the top of the list is the
    /// part behind them.
    fn open_outline(&mut self) -> Result<()> {
        let sections = self.sections();
        let cursor = self.doc.cursor();
        let here = sections.iter().rposition(|s| s.at <= cursor).unwrap_or(0);
        self.mode = Mode::Outline(sections);
        let capacity = self.layout().capacity().max(1);
        self.panel_page = here / capacity;
        self.panel_focus = None;
        self.paint()
    }

    /// Go to a heading, and put it at the top of the page. `landing` says this
    /// paint is an arrival. Out through [`Editor::leave_panel`], `[ Done ]`'s
    /// own path, which tidies up after whatever panel was open.
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
        self.reset_page(px, self.theme.margin)
    }

    /// Set the page to another margin. The white space is the setting and the
    /// text column is the rest of the surface: a wider margin is a shorter
    /// line. How many characters that line holds follows from the type size.
    fn set_margin(&mut self, percent: u16) -> Result<()> {
        if self.theme.margin == percent {
            return Ok(());
        }
        write_margin(percent);
        self.reset_page(self.theme.body_px, percent)
    }

    /// Set a stop to hang past the measure, or to push a character down. The
    /// rule is kept either way: push-out sends the character before the mark
    /// down with it, and hanging sets the mark in the margin.
    fn set_hanging(&mut self, on: bool) -> Result<()> {
        if self.theme.rules.hang == on {
            return Ok(());
        }
        write_hanging(on);
        let mut rules = self.theme.rules;
        rules.hang = on;
        self.theme = render::Theme::at(self.theme.body_px, self.theme.margin).breaking(rules);
        eprintln!(
            "page: {} punctuation",
            if on { "hanging" } else { "push-out" }
        );
        // Every line below the first break can move: there is no smaller
        // rectangle to find.
        self.frame = None;
        self.paint()
    }

    /// Divide words at the end of a line, or leave every one whole. The
    /// firmware's dictionary is read on first use, and the first page after
    /// this goes on pays for building the automaton.
    fn set_hyphenate(&mut self, on: bool) -> Result<()> {
        if self.hyphenate == on {
            return Ok(());
        }
        self.hyphenate = on;
        write_hyphenate(on);
        eprintln!("page: words {}", if on { "divided" } else { "left whole" });
        // Every row from the first divided word down can move.
        self.frame = None;
        self.paint()
    }

    /// Read the two keys beside the space bar the way `convention` sends them.
    /// Every binding takes the pair from [`keymap::Convention::resolve`], and
    /// the next chord is what reports it.
    fn set_convention(&mut self, convention: Convention) -> Result<()> {
        if self.convention == convention {
            return Ok(());
        }
        self.convention = convention;
        write_convention(convention);
        eprintln!("keyboard: {} modifiers", convention.name());
        self.paint()
    }

    /// Lay the page out again at `px` and `percent`, and draw it. A full
    /// repaint: the column, the margin and the leading all move, and the
    /// remembered frame describes a page that is gone.
    fn reset_page(&mut self, px: f32, percent: u16) -> Result<()> {
        self.theme = render::Theme::at(px, percent).breaking(self.theme.rules);
        eprintln!("page: {px} px, {percent}% margins");
        self.frame = None;
        self.paint()
    }

    /// Clear the panel of whatever it is holding onto, and draw the screen
    /// again. Deliberate only: no counter, no idle trigger. Everything
    /// remembered about what is on screen goes with it.
    fn refresh_panel(&mut self) -> Result<()> {
        self.window.flash()?;
        self.frame = None;
        self.strip_drawn.clear();
        self.status_drawn.clear();
        self.paint()
    }

    /// Back to the page, from whichever panel is over it. `[ Done ]`'s own
    /// path, and this editor's answer to [`reopens`]. Naming never reaches
    /// [`Editor::apply`] — see [`Editor::typed_name`].
    fn reopens(&self, action: &Action) -> bool {
        reopens(
            &self.mode,
            self.find.is_some(),
            self.find.as_ref().is_some_and(|find| find.replacing),
            action,
        )
    }

    /// Close what [`Editor::reopens`] reported, by the door that surface's own
    /// Done or Esc uses.
    fn close_reopened(&mut self, action: &Action) -> Result<()> {
        match action {
            Action::Find | Action::Replace => self.close_find(),
            _ => self.leave_panel(),
        }
    }

    fn leave_panel(&mut self) -> Result<()> {
        self.scanning = None;
        // A half-tapped Delete does not survive leaving the list.
        self.arming = None;
        self.panel_focus = None;
        self.mode = Mode::Writing;
        self.paint()
    }

    /// Open the find bar, seeded from the selection if there is one.
    fn open_find(&mut self) -> Result<()> {
        self.open_bar(false)
    }

    /// Open the bar's second field, or open the whole bar with it showing. It
    /// does not reopen: a query typed survives the second field
    /// appearing, and the keys go to the new field.
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
        // search for, and it matches nothing.
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
        // The bar is chrome, and opening it brings the chrome back.
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
        // one held splices half a word into the other.
        self.abandon_composition();
        if let Some(find) = &mut self.find {
            find.field = which;
        }
        self.paint()
    }

    /// Swap between the two fields. Nothing while only one is showing.
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

    /// Change the match on screen, and step to the next. Stepping on is the
    /// point: the hit just changed has stopped matching the query.
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

    /// Take the arm off `[ All ]`, reporting whether it was on. The confirming
    /// tap has to be the next thing the writer does: there is one `[ All ]`
    /// chip, and an arm left standing outlives the text it was armed on.
    fn disarm_all(&mut self) -> bool {
        match &mut self.find {
            Some(find) if find.arming_all => {
                find.arming_all = false;
                true
            }
            _ => false,
        }
    }

    /// A tap on `[ All ]`: arm it, or carry it out. Two taps, the rule the
    /// Delete chip follows.
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

    /// Recompute the hits and go to the one nearest the cursor. Run on every
    /// keystroke in the bar.
    fn research(&mut self) {
        let Some(find) = &self.find else { return };
        let needle: Vec<char> = find.query.chars().collect();
        let chars = self.doc.chars();
        let hits = karyll_core::find::matches(&chars, &needle);
        // From the *start* of the selection, not the cursor: arriving at a hit
        // leaves the cursor at its end, and searching on from there for a
        // longer word skips the hit the writer is looking at.
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
    /// it: a hit is drawn as a selection.
    fn show_hit(&mut self) {
        let Some(find) = &self.find else { return };
        let Some(hit) = find.hits.get(find.at).cloned() else {
            // Nothing matches: nothing is highlighted.
            self.doc.clear_selection();
            return;
        };
        self.doc.select(hit);
    }

    /// Step to the next hit, or the previous one going back. Wraps either way.
    fn step_find(&mut self, back: bool) {
        // From the selection's *start*. `select` leaves the cursor at the end
        // of the range: stepping from the cursor measures forwards from one
        // edge of the hit and backwards from the other.
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

    /// Close the find bar, leaving the cursor on the hit it found. Esc means
    /// "stop searching": the match stays selected, and the next keystroke can
    /// replace it.
    fn close_find(&mut self) -> Result<()> {
        if self.find.take().is_none() {
            return Ok(());
        }
        self.frame = None;
        self.paint()
    }

    /// A keystroke the IME did not want, while the find bar is open. True once
    /// the bar has closed. Reached after [`Editor::compose_key`] has passed:
    /// every arm here is the case with nothing being composed.
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
            // from the page. `Ctrl`/`⌘`+`Shift`+`F` closes a bar showing the
            // second field, and reveals it on a plain one.
            Action::Find => {
                self.close_find()?;
                return Ok(true);
            }
            Action::Replace if self.reopens(&Action::Replace) => {
                self.close_find()?;
                return Ok(true);
            }
            // Enter steps on, Shift+Enter steps back. Shift+Enter arrives as
            // `CommitTyped`: mid-composition it means the letters, not the
            // conversion, and out of one it is Enter with Shift held.
            Action::Newline | Action::CommitTyped => self.step_find(self.mods.shift),
            // Enter keeps one meaning in both fields: stepping is what it does
            // in a find bar. Changing is its own chord.
            Action::Change => return self.change_one().map(|()| false),
            Action::ChangeAll => return self.change_all().map(|()| false),
            // Tab moves between the two fields; an indent has no meaning in a
            // one-line field.
            Action::Indent => return self.swap_field().map(|()| false),
            Action::Replace => return self.open_replace().map(|()| false),
            Action::Backspace => {
                if self.edit_field(|text| {
                    text.pop();
                }) {
                    self.research();
                }
            }
            // The way back to Latin with a CJK engine switched on. Every letter
            // goes to the engine while the mode is on, and Ctrl+Space is the
            // binding everywhere else in the app.
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
    /// it was the query. One place: a keystroke cannot land in the query while
    /// the caret is on the replacement.
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
            // It paints itself: the chip that moves is on the page doing the
            // painting. Chrome is set at [`ui::TEXT_PX`] and stays there.
            Some(ConfigRow::Size) => {
                return match render::SIZES.get(option) {
                    Some(px) => self.set_size(*px),
                    None => Ok(()),
                };
            }
            Some(ConfigRow::Margins) => {
                return match render::MARGINS.get(option) {
                    Some((percent, _)) => self.set_margin(*percent),
                    None => Ok(()),
                };
            }
            Some(ConfigRow::Hanging) => return self.set_hanging(option == 1),
            Some(ConfigRow::Hyphenation) => return self.set_hyphenate(option == 1),
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
            Some(ConfigRow::Modifiers) => {
                return self.set_convention(if option == 1 {
                    Convention::Pc
                } else {
                    Convention::Mac
                });
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

    /// Break the line, carrying a list or quote marker onto the next one. The
    /// list the cursor is in is the line it sits on: this reads the text
    /// before the cursor, not the whole line.
    fn newline(&mut self) {
        let chars = self.doc.chars();
        let start = self.doc.line_start(self.doc.cursor());
        let line = &chars[start..self.doc.cursor().min(chars.len())];
        match karyll_core::continues(line) {
            karyll_core::Continue::Break => self.doc.insert_char('\n'),
            karyll_core::Continue::Marker(marker) => {
                self.doc.insert(&format!("\n{marker}"));
            }
            // The empty marker goes with the break: Enter on a bare bullet
            // leaves a clean blank line, not a stranded `- `.
            karyll_core::Continue::End(back) => {
                for _ in 0..back {
                    self.doc.backspace();
                }
                self.doc.insert_char('\n');
            }
        }
    }

    /// Tick the task the cursor is on, or untick it. One character, in place,
    /// and the cursor holds: a mark against a line being read. A line that is
    /// not a task is left alone.
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

    /// Wrap the selection, or the word under the cursor, in `marker`. With
    /// nothing selected it takes the word, and the wrapped text is left
    /// selected: the same key is a round trip.
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
        // Where the text lands, markers excluded. The next press reads back
        // this span to undo the wrap.
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

    /// The Config panel, paired with what each control does. One list: drawing
    /// and hit-testing both come from here, and every value is on screen, with
    /// picking one a tap on the value itself.
    fn config_items(&self) -> Vec<(ui::Item, ConfigRow)> {
        // Pairing comes first: there is nothing to type on until it is done,
        // and nothing else on this page can be reached from a keyboard that
        // does not exist yet.
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

        // A writing system with nothing installed is left off entirely. One
        // with exactly one keeps its row: the chip takes no tap, and it says
        // what the writing on screen is set in.
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
                                // is. A second name reads as a second setting,
                                // and there is one face per system.
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
        // Size ahead of the faces. It applies to every writing system at once.
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
        // Directly under Size: the two are read together, deciding how much of
        // the page is text and how much is white.
        items.push((
            ui::Item::Choice {
                label: "Margins".into(),
                options: render::MARGINS
                    .iter()
                    .map(|(_, name)| (*name).to_string())
                    .collect(),
                on: render::MARGINS
                    .iter()
                    .map(|(percent, _)| *percent == self.theme.margin)
                    .collect(),
                inert: Vec::new(),
            },
            ConfigRow::Margins,
        ));
        // Third of the three that decide how the column reads, and last: the
        // other two are the page, this is a preference about one mark.
        let hang = self.theme.rules.hang;
        items.push((
            ui::Item::Choice {
                label: "Hanging".into(),
                options: vec!["Off".into(), "On".into()],
                on: vec![!hang, hang],
                inert: Vec::new(),
            },
            ConfigRow::Hanging,
        ));
        // Only where there is a dictionary to divide by. On Chinese or
        // Japanese the row is a control that does nothing.
        if hyphen::load(self.language).is_some() {
            let on = self.hyphenate;
            items.push((
                ui::Item::Choice {
                    label: "Hyphenation".into(),
                    options: vec!["Off".into(), "On".into()],
                    on: vec![!on, on],
                    inert: Vec::new(),
                },
                ConfigRow::Hyphenation,
            ));
        }
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
            // Only while there is colour to pick. Off, the swatches draw
            // through the grey palette: six near-black circles.
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
        // page over has the better control, and it is the device.
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
    /// Remembered keyboards first, then anything a scan has turned up, then the
    /// scan itself. Each keyboard is one line with its actions beside it.
    fn keyboard_items(&self) -> Vec<(ui::Item, ConfigRow)> {
        let mut items: Vec<(ui::Item, ConfigRow)> = self
            .paired
            .iter()
            .map(|device| {
                // The first chip answers for this keyboard, not for the radio:
                // with several remembered, one holds the link. `Connect` is
                // grey — the daemon waits on every one and takes the first.
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

        // Named for the keycap a writer can look down and read, which is what
        // tells the two conventions apart on a keyboard carrying one set. A
        // keyboard carrying both switches itself and repaints its own legends.
        let mac = self.convention == Convention::Mac;
        items.push((
            ui::Item::Choice {
                label: "Beside the space bar".into(),
                options: vec!["⌘".into(), "Alt".into()],
                on: vec![mac, !mac],
                inert: Vec::new(),
            },
            ConfigRow::Modifiers,
        ));

        // Not started on opening Config. Scanning suspends the daemon — the log
        // says `Connection cancelled (suspend)` — which drops the keyboard
        // being typed on. The chip says what it is doing, including nothing.
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

        // Last in the section: it is about the keyboard, not any one of them.
        // This row is the only place to ask for it outside karyll. What it does
        // is in [`hid::Hid::set_keep_alive`].
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

    /// Draw a writing system in the face that was tapped. The page is laid out
    /// again on the way back to it: two families are not the same height, every
    /// row moves, and [`Editor::paint`] drops the frame under a panel.
    fn set_family(&mut self, group: font::Group, family: usize) {
        if self.fonts.choices().get(group) == family {
            return;
        }
        self.fonts.set_family(group, family);
        write_choices(self.fonts.choices());
        self.frame = None;
        eprintln!(
            "font: {} in {}",
            group.label(),
            self.fonts.family(group).name
        );
    }

    /// Switch an input source on or off. The last one cannot be switched off,
    /// leaving no way to type. Switching off the one in use moves to the next
    /// that is on.
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
            // only one and it has them; on the replace bar this is how
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
                self.panel_focus = None;
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
                // keyboards were in the room at the time of the scan.
                self.refresh_paired();
                self.found.clear();
                self.panel_page = 0;
                self.panel_focus = None;
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
    /// Split out for the one caller with somewhere else to go: deleting the
    /// open document puts another one behind the Files list.
    fn load(&mut self, path: PathBuf) -> Result<()> {
        if self.doc.is_dirty() {
            self.save()?;
        }
        // The outgoing draft's place is kept before it is replaced, or
        // switching away and back loses it.
        self.remember_position();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        self.doc = Document::from_text(&text);
        self.doc.set_cursor(opening_cursor(&path, self.doc.len()));
        self.path = Some(path);
        Ok(())
    }

    /// A key or a finger just arrived: the writer is here. Holds the
    /// screensaver off.
    fn note_input(&mut self) {
        self.last_input = std::time::Instant::now();
        if !self.holding_awake {
            power::prevent_screensaver(true);
            self.holding_awake = true;
        }
    }

    /// Let the device sleep once the writer has been away long enough. The
    /// latch is on writing, not on the app being open: it holds WiFi awake too.
    /// `wait` reports the hangup a suspend leaves on the keyboard's node.
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

    /// Record where the cursor is in the open document. Called wherever a
    /// document is left — saved, switched away from, or quit. Not in `Drop`:
    /// this binary is built with `panic = "abort"`.
    fn remember_position(&self) {
        if let Some(path) = &self.path {
            write_position(path, self.doc.cursor());
        }
    }

    /// Move the keyboard to the next line of the panel that can take it. The
    /// page follows the keyboard, and walking off the foot turns to the next
    /// one. Where no line takes focus, the arrows turn the page.
    fn move_focus(&mut self, down: bool) -> Result<()> {
        let next = match self.panel_focus {
            Some(focus) => next_focusable(&self.takes_focus(), Some(focus.row), down),
            None => self.first_focus(down),
        };
        let Some(row) = next else {
            return self.page_or_nothing(down);
        };
        let was = self.panel_focus;
        self.panel_focus = Some(PanelFocus {
            row,
            chip: self.chip_on(row),
        });
        let page = row / self.layout().capacity().max(1);
        if page != self.panel_page {
            self.panel_page = page;
            return self.paint();
        }
        self.repaint_focus(was)
    }

    /// Where the keyboard lands the first time it is used on a panel: the
    /// nearest line of the page on screen. The outline opens at the section
    /// being written, and a first press jumping to the top undoes that.
    fn first_focus(&mut self, down: bool) -> Option<usize> {
        let takes = self.takes_focus();
        let window = self.page_window();
        let on_page: Vec<usize> = (window.start..window.end.min(takes.len()))
            .filter(|at| takes[*at])
            .collect();
        let first = if down {
            on_page.first()
        } else {
            on_page.last()
        };
        first
            .copied()
            .or_else(|| next_focusable(&takes, None, down))
    }

    /// Which lines of the open panel the keyboard can land on. What a line
    /// means is the editor's to say: a row with no chips is a document in the
    /// Files list, a heading in the outline, and a fact on the Help page.
    fn takes_focus(&self) -> Vec<bool> {
        self.panel_items()
            .iter()
            .map(|item| line_takes_focus(&self.mode, item))
            .collect()
    }

    /// Which chip the keyboard sits on when it arrives at `row`: the value the
    /// setting stands on. The arrows move from there.
    fn chip_on(&self, row: usize) -> usize {
        let items = self.panel_items();
        let Some(item) = items.get(row) else {
            return 0;
        };
        let takeable = ui::takeable(item);
        ui::current(item)
            .filter(|at| takeable.contains(at))
            .or_else(|| takeable.first().copied())
            .unwrap_or(0)
    }

    /// Move along the chips of the line the keyboard is on. A line with no
    /// chips has nothing to move along, and there the left and right keys turn
    /// the page, which is what the strip says they do.
    fn move_chip(&mut self, right: bool) -> Result<()> {
        let items = self.panel_items();
        let takeable = self
            .panel_focus
            .and_then(|focus| items.get(focus.row))
            .map(ui::takeable)
            .unwrap_or_default();
        let (Some(focus), false) = (self.panel_focus, takeable.is_empty()) else {
            return self.page_or_nothing(!right);
        };
        let at = takeable
            .iter()
            .position(|chip| *chip == focus.chip)
            .unwrap_or(0);
        let next = if right {
            (at + 1) % takeable.len()
        } else {
            (at + takeable.len() - 1) % takeable.len()
        };
        let was = self.panel_focus;
        self.panel_focus = Some(PanelFocus {
            chip: takeable[next],
            ..focus
        });
        self.repaint_focus(was)
    }

    /// Turn a page, or do nothing when there is only the one.
    fn page_or_nothing(&mut self, back: bool) -> Result<()> {
        if self.pages() > 1 {
            return self.turn_page(back);
        }
        Ok(())
    }

    /// Carry out whatever the keyboard is on: a chip if the line has any, and
    /// the line itself if it has none. The same two calls a tap makes.
    fn take_focus(&mut self) -> Result<()> {
        let Some(focus) = self.panel_focus else {
            return Ok(());
        };
        let items = self.panel_items();
        let Some(item) = items.get(focus.row) else {
            return Ok(());
        };
        if ui::takeable(item).is_empty() {
            self.take_row(focus.row)
        } else {
            self.take_chip(focus.row, focus.chip)
        }
    }

    /// Redraw the line the keyboard left and the line it arrived on, and
    /// nothing else. The rest of the list is unchanged, and two rows of ink is
    /// what changed.
    fn repaint_focus(&mut self, was: Option<PanelFocus>) -> Result<()> {
        let window = self.page_window();
        let items = self.visible_items();
        let layout = self.layout();
        let now = self.panel_focus;
        let mut plan: Vec<(usize, Option<usize>)> = Vec::new();
        if let Some(focus) = was.filter(|focus| window.contains(&focus.row)) {
            plan.push((focus.row - window.start, None));
        }
        if let Some(focus) = now.filter(|focus| window.contains(&focus.row)) {
            let row = focus.row - window.start;
            // The arrival wins where the two are the same line, which is every
            // move along a row of chips.
            plan.retain(|(at, _)| *at != row);
            plan.push((row, Some(focus.chip)));
        }
        for (row, chip) in plan {
            let rect =
                ui::paint_focus_row(&mut self.window, &mut self.fonts, layout, &items, row, chip);
            self.window.present(rect)?;
        }
        Ok(())
    }

    /// Which slice of the current panel's list is on screen. The one place the
    /// page offset is turned into indices, and it clamps in place, which is
    /// what `&mut self` is for.
    fn page_window(&mut self) -> std::ops::Range<usize> {
        let capacity = self.layout().capacity().max(1);
        let pages = self.panel_len().div_ceil(capacity).max(1);
        self.panel_page = self.panel_page.min(pages - 1);
        let start = self.panel_page * capacity;
        start..start + capacity
    }

    /// Where the keyboard is on the page being drawn, if it is on that page.
    /// The focus indexes the list and a page of it is drawn, and a row that has
    /// gone since stops being marked.
    fn visible_focus(&mut self) -> Option<ui::Focus> {
        let focus = self.panel_focus?;
        let window = self.page_window();
        window.contains(&focus.row).then_some(ui::Focus {
            row: focus.row - window.start,
            chip: focus.chip,
        })
    }

    /// The lines of the current panel that are on screen. Everything that
    /// draws or hits the list comes through here, not through
    /// [`Editor::panel_items`]. Nothing can see a line the writer cannot.
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
            // A document with no headings has the one line saying so.
            Mode::Outline(sections) => sections.len().max(1),
            Mode::Writing | Mode::Naming { .. } => 0,
        }
    }

    /// How many pages the current panel takes.
    fn pages(&mut self) -> usize {
        let capacity = self.layout().capacity().max(1);
        self.panel_len().div_ceil(capacity).max(1)
    }

    /// Turn a page, wrapping either way. The pair is always both there, and
    /// fitted cells hold their places under a finger.
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
            // A list of documents, and nothing that is not one: New and Rename
            // are on the strip. Each line says how long a document is and how
            // lately it was written.
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
                        // Two taps, and the chip says which one it is on.
                        // Deleting prose cannot be undone and this device has
                        // no bin: the first tap arms it.
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
            // Derived from `keyboard_rows`, never rebuilt. Each remembered
            // keyboard adds two rows, and a second listing draws one list while
            // taps dispatch against another.
            Mode::Config => self
                .config_items()
                .into_iter()
                .map(|(item, _)| item)
                .collect(),
            Mode::Help => help_items(),
            // Indented by level, which is what makes it an outline: the shape
            // of the draft is the thing being looked at. A document with no
            // headings gets a heading item, which [`ui::hit`] leaves untappable.
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
    /// loop's tick. Not a blocking sleep: holding the loop for twenty seconds
    /// stops the panel repainting and queues taps behind it.
    fn start_scan(&mut self) -> Result<()> {
        // Tapping the chip again while it counts restarts the ten seconds.
        if self.scanning.is_some() {
            return Ok(());
        }
        if self.keyboard_present {
            // The keyboard goes quiet for the duration and comes back on its
            // own.
            self.show_status("Scanning disconnects the keyboard for a moment…")?;
        }
        // Coming up is not a failure. The editor does not wait for the radio:
        // this chip can be tapped seconds before anything is listening, and
        // the daemon is on its way.
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

    /// Turn the page to match the way the device is being held. Not a Rotate
    /// button, which is a mode. An unrecognised code holds the current
    /// orientation, the sensor emitting a settling burst on power-up.
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
    /// on a Kindle that has none. One place: the request goes in the window's
    /// name, the answer comes back as a resize, and the touch mapping moves.
    fn turn_to(&mut self, want: orientation::Orientation) -> Result<()> {
        if want == self.window.orientation() {
            return Ok(());
        }
        self.window.set_orientation(want)?;
        self.touch_orientation = want;
        self.orientation_checked = std::time::Instant::now();
        // Only where the orientation is a setting. A device with an
        // accelerometer opens on its reading.
        if !self.turns_itself {
            write_orientation(want);
        }
        // The window manager answers with a resize the loop picks up. This
        // paint covers a request it declines.
        self.frame = None;
        self.paint()
    }

    /// Notice when the Bluetooth stack finished coming up, and refresh the
    /// Keyboard section with what the daemon remembers.
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

    /// Follow the framework when it has turned the screen. The compositor
    /// rotates the window's pixels, and the touchscreen is panel-fixed: the
    /// mapping is what changes.
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

    /// Where words may be divided: the writer's setting, and the dictionary
    /// for the source they are writing in. Every `Page` asks here: the frame
    /// on screen and the frame a hit test reads agree.
    fn dictionary(&self) -> Option<&'static karyll_core::Hyphenator> {
        self.hyphenate
            .then(|| hyphen::load(self.language))
            .flatten()
    }

    /// The document as it should appear, with any preedit spliced in
    /// at the cursor, and where that preedit sits. Every `Page` in the app is
    /// built from this; [`Editor::document_index`] is the only way back.
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

    /// Where a keystroke goes. Naming outranks the find bar, the
    /// order the key loop routes in: a panel covers the page. The two cannot
    /// both be open — the find bar takes the strip, and there is no `New` on it.
    fn sink(&self) -> Sink {
        sink_for(
            matches!(self.mode, Mode::Naming { .. }),
            self.find.is_some(),
        )
    }

    /// The composition as far as the page is concerned: empty unless the page
    /// is what is being typed into. The hits are document indices, and a
    /// preedit bound for the find bar must not move the text after the cursor.
    fn page_preedit(&self) -> &str {
        page_composition(&self.preedit, self.sink())
    }

    /// Where the caret goes in display space: past the preedit, which is where
    /// the next keystroke will land.
    fn display_cursor(&self) -> usize {
        self.doc.cursor() + self.page_preedit().chars().count()
    }

    /// Turn a display index back into a document one. The preedit's own
    /// characters collapse onto the cursor.
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

    /// Turn the page's focus on or off, and remember which. A full repaint:
    /// every row changes ink at once.
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
            // Asked for but not begun. Waiting is the whole job here: calling
            // it finished ends the scan before the radio has done anything.
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

    /// A keystroke the IME did not want, while a name is being typed. True
    /// once the name is settled. Reached after [`Editor::compose_key`] has
    /// passed: `Enter` here is the name and `Esc` is never mind.
    fn typed_name(&mut self, action: &Action) -> Result<bool> {
        let Mode::Naming { for_new, name } = &mut self.mode else {
            return Ok(false);
        };
        let (for_new, mut name) = (*for_new, std::mem::take(name));

        match action {
            // `CommitTyped` as well: that is what Shift+Enter is with no
            // composition to commit, and a name field takes either.
            Action::Newline | Action::CommitTyped => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    // Enter on an empty name means "make me one" for a new
                    // document, and cancels a rename, where there is nothing
                    // to invent.
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
            // opening shortcut follows — see [`Editor::reopens`]. Only for a
            // new document: a rename is opened from the Files strip.
            Action::NewDocument if for_new => {
                self.mode = Mode::Writing;
                self.paint()?;
                return Ok(true);
            }
            Action::Backspace => {
                name.pop();
            }
            // As in the find bar: with a CJK engine on, every letter goes to
            // the engine. This is the only way back to a Latin filename.
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

    /// Whether `device` is the keyboard being typed on. Both halves: the daemon
    /// names the keyboard it has a link to, and the evdev node is what a
    /// keystroke arrives on.
    fn is_connected(&self, device: &hid::Device) -> bool {
        self.keyboard_present
            && self
                .connected
                .as_deref()
                .is_some_and(|address| hid::same_address(address, &device.address))
    }

    /// Ask the daemon to drop the link, keeping the pairing. The node goes with
    /// it, and the session notices on the next tick and says so.
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

    /// Choose whether the Bluetooth stack outlives the editor. It takes effect
    /// on the way out: the keyboard being typed on is the daemon's keyboard.
    /// The status line carries the cost, paid where nothing points back here.
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

    /// Switch the colour panel on or off. A full refresh: the backing store
    /// holds the bytes the old setting wrote, and a partial update over ink
    /// changing hue leaves a ghost.
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

    /// Take a colour for the caret or the highlighter. A full refresh, the one
    /// [`Editor::set_colour`] takes: the ink on the panel is about to change
    /// hue.
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

    /// Remove a paired keyboard: it can be paired afresh.
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
        // Read the daemon's log from here on. A BLE keyboard has no display:
        // the host shows a passkey and the daemon prints it to its log. Every
        // attempt makes a fresh one.
        let mark = self.bluetooth.log_mark();
        if let Err(err) = self.bluetooth.pair(device) {
            self.show_status(&format!("Could not pair: {err:#}"))?;
            return Ok(());
        }
        let mut asked = false;
        for _ in 0..PAIR_TICKS {
            std::thread::sleep(std::time::Duration::from_secs(1));
            // Repaint only when the passkey first appears: this is a panel on
            // an e-ink screen, and once a second is a flashing mess.
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
                    // Out of Config: a keyboard that works wants typing on. The
                    // strip changes from `[ Done ]` to the writing row as it
                    // goes, and the next tap there is spent looking.
                    self.mode = Mode::Writing;
                    self.strip_changed = true;
                    return self.paint();
                }
                Ok(Some(false)) | Err(_) => {
                    // The device stays. A pair reporting failure may have
                    // completed, and deleting it throws away a saved link key.
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
        let focus = self.visible_focus();
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
                self.notice,
            ),
            focus,
        }
        .paint(&mut self.window, &mut self.fonts, layout)
    }

    fn apply(&mut self, action: Action) -> Result<()> {
        // Before anything else: the arms below all *open* things, and a
        // shortcut aimed at the surface on screen closes it.
        if self.reopens(&action) {
            return self.close_reopened(&action);
        }
        // Every editing action comes through here, the one place that notices
        // the document was touched. Cursor moves stamp it too, and autosave
        // fires on a dirty document alone.
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
            // Copying nothing leaves the clipboard alone: a stray Ctrl+C must
            // not empty it.
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
            // One `Edit` for the whole string: a paste undoes as one step —
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
            // renderer owns. Without that seam they do nothing.
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
            // On the page this ticks a task off. In the replace bar it carries
            // out the replacement, and [`Editor::typed_query`] takes it first.
            Action::Change => self.toggle_task(),
            Action::ChangeAll => {}
            // Through the strip's own handlers: a key and its button cannot
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
            Action::ResetSize => self.set_size(render::DEFAULT_SIZE)?,
            // The panel is untouched: the light sits behind it.
            Action::Brightness(up) => power::step_frontlight(up),
            // The two cases, shown as themselves.
            Action::CapsLock(on) => self.notice = Some(if on { "AB" } else { "ab" }),
            Action::CycleMargins => self.set_margin(render::step_margin(self.theme.margin))?,
            // Only means anything mid-composition, where `compose_key` takes
            // it. Reaching here is Shift+Enter with nothing being converted,
            // which is a line break.
            Action::CommitTyped => self.apply(Action::Newline)?,
            Action::Quit => {}
        }
        // Any movement that is not vertical abandons the column the arrow keys
        // were holding: the next Up or Down takes its column from here.
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
    /// the same action: they cannot disagree about what is selected.
    fn cycle_language(&mut self) {
        self.set_language(self.language.next(&self.enabled));
        // Say which one, beside the caret. The strip is hidden while writing:
        // nothing else answers it and `Ctrl+Space` cycles blind.
        self.notice = Some(self.language.label());
    }

    /// Take up the language the last session ended in, through
    /// [`Editor::set_language`]: a language is an IME, a keyboard layout and a
    /// set of Han faces, and that function applies all four.
    fn resume_language(mut self, language: Language) -> Self {
        self.set_language(language);
        self
    }

    /// Load the word list the Han faces are set for, unless it is the one
    /// loaded. It follows the faces, not the keyboard: a writer on the English
    /// keyboard may be editing Chinese they typed yesterday.
    fn set_lexicon(&mut self) {
        let region = self.fonts.region();
        if self.dict_region == Some(region) {
            return;
        }
        self.dict = lexicon::load(region);
        self.dict_region = self.dict.is_some().then_some(region);
    }

    /// Select an input source: its keyboard, its input method, and the regional
    /// convention its Han faces follow. Each engine is loaded the first time it
    /// is asked for, and a failure leaves the source selected without one.
    fn set_language(&mut self, language: Language) {
        self.abandon_composition();
        self.language = language;
        write_language(language);

        // A convention change repaints the page whole. Han unification draws
        // the same characters differently and the emphasis mark changes sides,
        // where `Frame::unchanged` compares the text.
        if let Some(region) = language.region() {
            if self.fonts.region() != region {
                self.frame = None;
            }
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
            // [`Editor::korean`] composes Hangul in this process. No plugin to
            // open, and nothing here that can fail.
            ime::Script::Korean => return true,
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

    /// The engine for the selected language, if it has one and it loaded.
    fn engine(&mut self) -> Option<&mut Box<dyn ime::Ime>> {
        let script = self.language.script()?;
        self.engines
            .iter_mut()
            .find(|(s, _)| *s == script)
            .map(|(_, e)| e)
    }

    /// Offer a keystroke to the selected input method. What a key means lives
    /// in [`ime::compose`]. [`Editor::compose_plugin`] drives a session with
    /// candidates; [`Editor::compose_hangul`] drives the state machine here.
    fn compose_key(&mut self, action: &Action) -> Composed {
        let Some(script) = self.language.script() else {
            return Composed::Passed;
        };
        if !self.cjk {
            return Composed::Passed;
        }
        // Asked of [`Editor::composing`] and not of `typed`. One answer
        // drives the rules, the bar and the hit-testing. Japanese swallows a
        // space as a conversion request, and the two can disagree.
        let compose = ime::compose(action, self.composing(), script);
        if script == ime::Script::Korean {
            self.compose_hangul(compose)
        } else {
            self.compose_plugin(compose, script)
        }
    }

    /// Korean: three cases, and no engine to reach for. [`ime::Korean::key`]
    /// hands back whatever the keystroke finished — the empty string for most,
    /// and the previous syllable when a vowel has taken its 받침 away.
    fn compose_hangul(&mut self, compose: ime::Compose) -> Composed {
        match compose {
            ime::Compose::Jamo(key) => {
                let done = self.korean.key(key);
                self.preedit = self.korean.preedit();
                self.insert_committed(&done);
                Composed::Took
            }
            ime::Compose::Decompose => {
                self.korean.backspace();
                self.preedit = self.korean.preedit();
                Composed::Took
            }
            // The key is not consumed: space is a space, Enter breaks the
            // line, an arrow moves the cursor past what was written.
            ime::Compose::Finish => {
                let text = self.korean.take();
                self.preedit.clear();
                self.insert_committed(&text);
                Composed::Finished
            }
            _ => Composed::Passed,
        }
    }

    /// Chinese and Japanese, which are a session with one of Amazon's plugins:
    /// candidates on a bar, a page to turn, and a commit that may convert only
    /// the front of the reading.
    fn compose_plugin(&mut self, compose: ime::Compose, script: ime::Script) -> Composed {
        match compose {
            ime::Compose::Pass => return Composed::Passed,
            // Backspace is fed to the engine, which drops one unit and
            // re-predicts: what is shown follows the engine.
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
            // leaves the half-typed word behind. Japanese passes through here
            // at the start of every syllable.
            ime::Compose::NextPage | ime::Compose::PreviousPage if self.candidates.is_empty() => {
                self.abandon_composition()
            }
            // Consumed at either end of the list whether or not the page moves.
            // An arrow let through from the last page moves the cursor out
            // from under the word being written.
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
            // The letters as struck, not the kana they turned into.
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
            // kana. Nothing is fed to the engine: it is told to reset.
            ime::Compose::CommitRaw => {
                let text = std::mem::take(&mut self.preedit);
                self.abandon_composition();
                self.insert_committed(&text);
            }
            ime::Compose::Cancel => self.abandon_composition(),
            // [`Editor::compose_hangul`] takes these three.
            ime::Compose::Jamo(_) | ime::Compose::Decompose | ime::Compose::Finish => {
                return Composed::Passed;
            }
        }
        Composed::Took
    }

    /// Send one key to the engine, and take back the candidate list and the
    /// composition as the engine reads it. The composition is asked for: for
    /// Japanese `nihon` composes にほん, and only the engine holds that.
    fn feed(&mut self, key: char) {
        let Some(engine) = self.engine() else {
            return;
        };
        let candidates = engine.key(key);
        let composed = engine.preedit();
        self.preedit = composed.unwrap_or_else(|| self.typed.clone());
        self.set_candidates(candidates);
    }

    /// Take a new list of candidates, and work out how it pages. Everything
    /// that changes the list comes through here: how it pages is what the panel
    /// can hold at the size being written in.
    fn set_candidates(&mut self, candidates: Vec<String>) {
        self.candidates = candidates;
        self.pages = ui::candidate_pages(
            &mut self.fonts,
            self.window.width(),
            self.theme.body_px,
            &self.candidates,
            ime::WANTED,
        );
        // A new list is read from its first page. An old page leaves the bar
        // empty on the keystroke that shortened the list.
        self.page = 0;
    }

    /// Accept a candidate by its place on the bar, from the number row or a
    /// tap. Out of range does nothing. `n` is looked up on the bar: going to
    /// the list lets a digit past a short page commit the next page's first.
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
            // The word is not over. The candidate converted the front of the
            // reading and the engine is composing the rest: the bar goes on
            // carrying it and the next keystroke belongs to it.
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

    /// Finish the word under way. Chinese takes the best candidate, and again
    /// while one covers only the front of the reading. Japanese takes the
    /// composition, which is raw kana until space asks for a conversion.
    fn commit_composition(&mut self, script: ime::Script) {
        if !self.composing() {
            return;
        }
        if script == ime::Script::Chinese {
            // Bounded by the reading, which every pass shortens by at least a
            // syllable. An engine handing back what it was given spins here.
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

    /// Put committed text where it is being typed, as one undo step. Every
    /// path out of the IME ends here: this is the only place that has to know
    /// which field is taking text. See [`Sink`].
    fn insert_committed(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.sink() {
            Sink::Page => {
                self.doc.insert(text);
                self.last_edit = Some(std::time::Instant::now());
            }
            // Whichever of the bar's fields is taking keys: a writer can say
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

    /// Throw away the half-typed word, in the engine as well as here.
    fn abandon_composition(&mut self) {
        if let Some(engine) = self.engine() {
            engine.clear();
        }
        self.korean.clear();
        self.typed.clear();
        self.preedit.clear();
        self.set_candidates(Vec::new());
    }

    /// Whether a word is being composed, which is what swaps the action strip
    /// for the candidate bar. A composition with no candidates counts: the
    /// letters typed towards a word never reach the document.
    fn composing(&self) -> bool {
        !self.preedit.is_empty() || !self.candidates.is_empty()
    }

    fn save(&mut self) -> Result<()> {
        self.write_document("saved")
    }

    /// Write the document out. `why` names the reason in the log: an autosave
    /// and a deliberate save are told apart when reading `karyll.log` after
    /// something went wrong.
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

    /// Write the document out on its own: a crash cannot cost prose. CJK input
    /// runs Amazon's closed predictor plugin inside this process.
    /// [`AUTOSAVE_MAX`] is the backstop for someone who never pauses.
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
        // A failed autosave must not end the session: the document is on
        // screen and Ctrl+S is there. Say so and carry on.
        if let Err(err) = self.write_document("autosaved") {
            eprintln!("autosave failed: {err:#}");
            // Back off a full interval, against a full or read-only
            // filesystem.
            self.dirty_since = Some(now);
            self.last_edit = Some(now);
        }
    }

    fn paint(&mut self) -> Result<()> {
        if !matches!(self.mode, Mode::Writing) {
            // A panel covers the page, and what was last drawn describes none
            // of it. Dropping the frame forces a full repaint on the way back.
            self.frame = None;
            let status = match &self.mode {
                // The composition counts as typing: a writer three keystrokes
                // into 日本語 has typed no name as far as `name` is concerned.
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
                        // fitting: `More` is a button, and this says what it
                        // brings.
                        n if n > window.len() => format!(
                            "{}–{} of {n} documents in {DOCUMENTS}",
                            window.start + 1,
                            window.end.min(n)
                        ),
                        n => format!("{n} documents in {DOCUMENTS}"),
                    }
                }
                // There is no Save on this page and nothing to confirm.
                Mode::Config => "Changes apply at once.".to_string(),
                // The two keys that are not in the list below: a list of what
                // the keys do cannot name the key that opened it or the key
                // that closes it.
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
        // A panel covered the page: the next paint starts from scratch — and
        // that is also exactly when the bar needs drawing again.
        let fresh = self.frame.is_none();
        let (chars, preedit) = self.display();
        let markup = karyll_core::markdown::analyze(&chars);
        // The page reaches the foot of the panel while the chrome is away:
        // both the text and the centring measure against that, not against a
        // strip that is not there.
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
        .composing(preedit)
        .hyphenating(self.dictionary());
        // Kept: page movement measures the same row the page is drawn with.
        // A document with Han in it has taller rows, and paging by Latin rows
        // in one steps past a line every screen.
        self.roles = page.roles.clone();
        let mut lines = render::layout(&page, &mut self.fonts, self.theme.margin_y as i32);
        // Normal writing lets the caret run to the foot of the page; focus mode
        // holds the sentence in the middle of it. See `render::Scroll`.
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
        // A selection cannot survive a composition in the page — typing over
        // one replaces it. A composition bound for the find bar is different:
        // the selection is the hit, and it stays inverted.
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
                self.notice,
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
        // The strip is chrome: always present, and always the way out. It needs
        // drawing when something covered it — a panel, or the first paint, or a
        // change in what it says. While composing it carries candidates.
        if !self.strip_visible() {
            self.strip_drawn.clear();
            self.status_drawn.clear();
            return Ok(());
        }
        let cells = self.strip_cells();
        // The status is compared alongside the buttons: it shares their band
        // and their repaint, and it is the half that changes. The buttons say
        // the same three words all session, while this counts and reports.
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

    /// What the candidate box hangs off, or `None` for the caret. The find
    /// bar's own cell, when that is where the typing is going: the caret is
    /// over at the last match by then.
    fn overlay_anchor(&mut self) -> Option<window::Rect> {
        if self.sink() != Sink::Find {
            return None;
        }
        // The field taking the keys, found by what it *is* and not by where the
        // bar puts it.
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
        // Measured from the cells drawn: the box sits over its own field.
        let bounds = ui::cell_bounds(width, &cells, &stretch, |s| {
            ui::measure(fonts, s, ui::TEXT_PX) as u16
        });
        Some(ui::strip_cell_rect(layout, &bounds, cell))
    }

    /// Move the cursor by whole visual lines, dragging the selection along if
    /// `extend`. Reads the wrapped layout off the last frame.
    fn move_vertical(&mut self, delta: i32, extend: bool) {
        let Some(frame) = self.frame.take() else {
            return;
        };
        // The same buffer the frame was laid out from, or the rows measured
        // here are a different shape from the rows on screen.
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
        .composing(preedit)
        .hyphenating(self.dictionary());
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

/// Scan for Bluetooth keyboards and pair with one. Needs a terminal — kterm or
/// ssh — to print a list and read a choice. A paired keyboard's link key is
/// kept: this is one step per keyboard and the editor never needs it again.
fn pair() -> Result<()> {
    let mut bluetooth = hid::Hid::beside_executable()?;
    // The same setting the editor reads: pairing from a terminal leaves the
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

/// The daemon scans for a fixed 10 seconds (`controller._do_scan`), BLE and
/// Classic concurrently. The safety net for a scan that never reports itself
/// finished; the UI follows the daemon's own `scanning` flag.
const SCAN_SECONDS: u64 = 14;
/// Seconds to wait for pairing to settle. Longer than the SMP pairing timeout
/// of 30 seconds plus the few the daemon spends suspending, powering the chip
/// and connecting.
const PAIR_TICKS: usize = 45;

/// How often the loop wakes when nothing has happened. A held finger produces
/// no events: the long press needs a tick to be noticed; the same tick looks
/// for a keyboard that has not arrived yet.
const TICK_MS: i32 = 200;

/// How long a pause in typing before the document is written by itself. Long
/// enough not to fire between words, short enough that a crash costs a sentence
/// at most.
const AUTOSAVE_IDLE: std::time::Duration = std::time::Duration::from_secs(3);
/// The longest a change is ever left unwritten, however continuously the writer
/// types. It is the backstop for a writer who never pauses.
const AUTOSAVE_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// Is an autosave due, given how long since the last keystroke and how long
/// the document has been unsaved? `idle_for` is `None` before anything has
/// been typed this session. Pure: the timing is tested off the device.
fn autosave_due(idle_for: Option<std::time::Duration>, dirty_for: std::time::Duration) -> bool {
    idle_for.is_none_or(|idle| idle >= AUTOSAVE_IDLE) || dirty_for >= AUTOSAVE_MAX
}

/// The orientation a Kindle with no sensor was last set to. Beside the logs,
/// where it survives an update.
fn orientation_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/orientation")
}

/// Which way up to open. Where there is a sensor the way the device is being
/// held wins, and [`evdev::Accelerometer::position`] asks it. A Kindle with no
/// sensor remembers, the orientation there being a setting a writer chose.
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
/// manager. Only on a Kindle with no accelerometer, where turning the device is
/// not a control. Two of them: a sensor gives the other two free.
const SCREENS: [(&str, orientation::Orientation); 2] = [
    ("Portrait", orientation::Orientation::Up),
    ("Landscape", orientation::Orientation::Right),
];

/// The selected input source, remembered for the same reason as the layout: a
/// writer who left in Chinese comes back to Chinese.
fn language_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/language")
}

/// The candidates on the bar: the page `page` names, from the starts
/// [`ui::candidate_pages`] worked out.
fn candidate_page<'a>(candidates: &'a [String], pages: &[usize], page: usize) -> &'a [String] {
    let Some(&from) = pages.get(page) else {
        return &[];
    };
    let to = pages.get(page + 1).copied().unwrap_or(candidates.len());
    &candidates[from.min(candidates.len())..to.min(candidates.len())]
}

/// What floats beside the caret. Candidates outrank a notice. Takes the two
/// values it reads, leaving the window and the faces free for the paint around
/// it.
fn overlay<'a>(candidates: &'a [String], notice: Option<&'a str>) -> ui::Overlay<'a> {
    if !candidates.is_empty() {
        ui::Overlay::Candidates(candidates)
    } else if let Some(text) = notice {
        ui::Overlay::Notice(text)
    } else {
        ui::Overlay::None
    }
}

/// Which field a keystroke goes to, in precedence order.
fn sink_for(naming: bool, finding: bool) -> Sink {
    if naming {
        Sink::Name
    } else if finding {
        Sink::Find
    } else {
        Sink::Page
    }
}

/// The composition as far as the document is concerned. Only [`Sink::Page`]
/// splices a preedit into the text and shifts the indices after the cursor.
fn page_composition(preedit: &str, sink: Sink) -> &str {
    if sink == Sink::Page { preedit } else { "" }
}

/// What the find bar's count cell says. Nothing until there is something to
/// count, and nothing while a word is being composed into the query: that
/// field then shows the query and the half-typed word.
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

/// Cut a strip to what the panel can hold. The order things are given up in is
/// on [`Editor::strip_fitted`]. Free of the editor: it runs against a stub
/// metric on the host.
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

/// What each cell says, in the strip's own words or in its shorter ones. The
/// fields are left blank: a field is sized by what the other cells leave.
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

/// Whether a strip fits the panel with every cell on it and every field wide
/// enough to read.
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
/// it is. Free of the editor: the mapping is tested on its own.
fn document_index(display: usize, cursor: usize, preedit: usize) -> usize {
    if display <= cursor {
        display
    } else {
        display.saturating_sub(preedit).max(cursor)
    }
}

/// Whether the action strip is on screen. Free of the editor, which leaves the
/// safety rule below testable: without a keyboard the strip is the only way
/// out, and a search puts the bar there.
fn strip_visible(hidden: bool, keyboard_present: bool, finding: bool, writing: bool) -> bool {
    !writing || finding || !hidden || !keyboard_present
}

/// Whether `action` asks for the surface on screen. Every
/// shortcut that opens something closes it. `replacing` is the find bar's
/// second field, which `Ctrl`/`⌘`+`Shift`+`F` asks to reveal.
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

/// Which input sources the language button cycles through. All five by
/// default, an unreadable or empty file included: a set with nothing in it
/// leaves no way to type at all.
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

/// Which writing systems the Config panel offers a face for: one row per
/// system, built from `enabled`.
fn font_groups(enabled: &[Language]) -> Vec<font::Group> {
    let mut groups: Vec<font::Group> = Vec::new();
    for language in Language::ALL.into_iter().filter(|l| enabled.contains(l)) {
        let group = language.font_group();
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    groups
}

/// The most chips one settings row carries. [`ui::chip_bounds`] drops a chip
/// crossing the right margin, and hit-tests the same bounds. The narrowest
/// panel leaves about 990 px for chips, against some 200 px a Latin name.
const CHIPS_PER_ROW: usize = 3;

/// Split a row's options across as many rows as they need, in even shares.
/// No options is no rows.
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

/// Which face draws each writing system. An unreadable or missing file is the
/// default list.
fn read_choices() -> font::Choices {
    font::Choices::parse(&std::fs::read_to_string(fonts_file()).unwrap_or_default())
}

fn write_choices(choices: font::Choices) {
    let _ = std::fs::write(fonts_file(), choices.render());
}

fn focus_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/focus")
}

/// Whether focus mode was on when the last session ended. Off unless the file
/// says so.
fn read_focus() -> bool {
    std::fs::read_to_string(focus_file()).is_ok_and(|s| s.trim() == "1")
}

fn write_focus(on: bool) {
    let _ = std::fs::write(focus_file(), if on { "1" } else { "0" });
}

fn hanging_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/hanging")
}

/// The line-breaking rules the writer last chose. Push-out by default: a mark
/// never leaves the column, and a line is a character shorter where one has.
fn read_rules() -> karyll_core::wrap::Rules {
    karyll_core::wrap::Rules {
        hang: std::fs::read_to_string(hanging_file()).is_ok_and(|s| s.trim() == "1"),
        ..Default::default()
    }
}

fn write_hanging(on: bool) {
    let _ = std::fs::write(hanging_file(), if on { "1" } else { "0" });
}

fn hyphenate_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/hyphenate")
}

/// Whether words were being divided when the last session ended. Off unless
/// the file says so.
fn read_hyphenate() -> bool {
    std::fs::read_to_string(hyphenate_file()).is_ok_and(|s| s.trim() == "1")
}

fn write_hyphenate(on: bool) {
    let _ = std::fs::write(hyphenate_file(), if on { "1" } else { "0" });
}

/// Say which node the keyboard is on, and whether anything but karyll can read
/// it. The second half is [`evdev::Keyboard::tagged_for_x`]: X binds only
/// tagged nodes, and an untagged keyboard types in the editor and nowhere else.
fn report_keyboard(keyboard: &evdev::Keyboard, how: &str) {
    let reach = match keyboard.tagged_for_x() {
        Some(true) => "tagged for X",
        Some(false) => "not tagged for X — karyll only",
        None => "no udev record",
    };
    eprintln!("keyboard: {}{how} ({reach})", keyboard.path().display());
}

fn modifiers_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/modifiers")
}

/// Which of the two keys beside the space bar the keyboard calls ⌘. Mac by
/// default, the convention the Help page names.
fn read_convention() -> Convention {
    let name = std::fs::read_to_string(modifiers_file()).unwrap_or_default();
    Convention::from_name(name.trim())
}

fn write_convention(convention: Convention) {
    let _ = std::fs::write(modifiers_file(), convention.name());
}

fn bluetooth_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/bluetooth")
}

/// Whether the Bluetooth stack is to outlive the editor. Off by default: the
/// daemon holds `/dev/stpbt`, and Audible and VoiceView have nothing while it
/// does.
fn read_keep_bluetooth() -> bool {
    std::fs::read_to_string(bluetooth_file()).is_ok_and(|s| s.trim() == "1")
}

fn write_keep_bluetooth(on: bool) {
    let _ = std::fs::write(bluetooth_file(), if on { "1" } else { "0" });
}

fn colour_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/colour")
}

/// Whether a colour panel is used as one. On by default: colour costs the rest
/// of the device nothing, and the switch is there to turn it off.
fn read_colour() -> bool {
    std::fs::read_to_string(colour_file()).map_or(true, |s| s.trim() != "0")
}

fn write_colour(on: bool) {
    let _ = std::fs::write(colour_file(), if on { "1" } else { "0" });
}

fn colours_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/colours")
}

/// Which colours the caret and the highlighter are set to, by name. A name
/// holds its meaning through a colour added to the middle of the picker.
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

/// The body size the last session ended at, stored as the size and not as a
/// rung of the ladder. A number the ladder has dropped snaps to the nearest.
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

fn margin_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/margins")
}

/// The margin the last session ended at, as a percentage of the panel's width.
/// A percentage means something on its own, where an index into a ladder means
/// whatever the next build's ladder says.
fn read_margin() -> u16 {
    std::fs::read_to_string(margin_file())
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .map_or(render::DEFAULT_MARGIN, render::nearest_margin)
}

fn write_margin(percent: u16) {
    let _ = std::fs::write(margin_file(), format!("{percent}\n"));
}

fn read_language() -> Language {
    // Not logged here. `set_language` reports what was taken up, and the two
    // can differ.
    std::fs::read_to_string(language_file())
        .map(|s| Language::from_letter(&s))
        .unwrap_or_default()
}

fn write_language(language: Language) {
    let _ = std::fs::write(language_file(), language.letter().to_string());
}

/// Where each document was last being written. One line per document,
/// `<index>\t<path>`, most recent first, in character indices: a byte offset
/// lands inside a codepoint in a Chinese draft.
fn positions_file() -> PathBuf {
    PathBuf::from("/mnt/us/extensions/karyll/var/positions")
}

/// How many documents are remembered. Small on purpose: the list is rewritten
/// whole on every save, and a writer works on a handful of drafts.
const POSITIONS_KEPT: usize = 64;

/// The index comes first, and everything after the first separator is the
/// path: a path containing a tab round-trips.
fn parse_positions(body: &str) -> Vec<(usize, String)> {
    body.lines()
        .filter_map(|line| {
            let (index, path) = line.split_once('\t')?;
            Some((index.trim().parse().ok()?, path.to_string()))
        })
        .collect()
}

/// Put `path` at the front with its new index, dropping any older entry for
/// it. Most-recent-first with a cap: the file cannot grow without bound.
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

/// Drop what was remembered about a document that has gone. A new
/// document can be given a deleted one's name, and inheriting a cursor from
/// prose that is gone opens it somewhere meaningless.
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

/// Where to put the cursor when a document opens. With nothing remembered the
/// answer is the top. A stored index is clamped: the file is plain Markdown on
/// a volume that mounts over USB.
fn opening_cursor_from(stored: Option<usize>, len: usize) -> usize {
    stored.unwrap_or(0).min(len)
}

fn opening_cursor(path: &Path, len: usize) -> usize {
    opening_cursor_from(read_position(path), len)
}

/// Where documents live. Outside the extension: updating karyll replaces
/// that directory wholesale, and prose must not go with it.
const DOCUMENTS: &str = "/mnt/us/karyll";

/// What the keys and the glass do, on the grid Config and Files use: the thing
/// on the left, the key for it in the detail column. A list of actions, not of
/// keys. Both chords are named `Ctrl`/`⌘` throughout, both being bound.
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
        row("Back to the usual size", "Ctrl/⌘ + 0"),
        row("Margins", "Ctrl/⌘ + M"),
        heading("Getting around"),
        row("Find, then step through", "Ctrl/⌘ + F,  Enter"),
        row("Step back through matches", "Shift + Enter"),
        row("Find and replace", "Ctrl/⌘ + Shift + F"),
        row("Move between the two fields", "Tab"),
        row("Change this match, change all", "Ctrl/⌘ + Enter,  + Shift"),
        row("Sections of this document", "Ctrl/⌘ + Shift + O"),
        row("Word, line, document", "Ctrl + ←,  ⌘ + ←,  ⌘ + ↑"),
        row("Select as you go", "Shift + any move"),
        row("Documents, new document", "Ctrl/⌘ + O,  Ctrl/⌘ + N"),
        row("Settings", "Ctrl/⌘ + ,"),
        row("Through a list, and take one", "↑ ↓,  Enter"),
        row("Values on a setting, pages on a list", "← →"),
        row("Delete the document you are on", "Backspace, twice"),
        row("Clear the screen", "Ctrl/⌘ + R"),
        row("Close it again", "The same shortcut, or Esc"),
        row("Leave karyll", "Ctrl/⌘ + Q"),
        heading("Writing in Chinese, Japanese and Korean"),
        row("Switch input source", "Ctrl/⌘ + Space"),
        row("Take a candidate", "Space, or 1 … 0"),
        row("Take the letters as typed", "Shift + Enter"),
        row("Drop the half-typed word", "Esc"),
        row("Take one jamo back, in Korean", "Backspace"),
        row("Emphasis, marked not slanted", "Ctrl/⌘ + I"),
        heading("Your keyboard"),
        row("Ctrl and ⌘", "The same. Every shortcut takes either"),
        row("Find", "The magnifier key"),
        row("The reading light", "The sun keys"),
        row("Caps Lock", "AB or ab, beside the cursor"),
        row("Beside the space bar", "Settings says which it sends"),
        row("Both printed on that key", "The keyboard chooses which"),
        row("Changing what it sends", "The keyboard's own shortcut"),
        row("Settings, from any keyboard", "Ctrl + ,"),
        heading("Touch and pen"),
        row("Back a screen, on a screen", "Tap the left, right margin"),
        row("Start, end of the document", "Tap the top, the foot"),
        row("Bring the buttons back", "Tap the foot of the page"),
        row("Select a word", "Tap it twice"),
        row("Select a run", "Press at one end, lift at the other"),
        row("Extend a selection", "Shift + tap"),
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

/// Every heading in `chars`, in order, from
/// [`karyll_core::markdown::analyze`]. The word count is the prose under the
/// heading, to the next heading of any level.
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

/// The outline as rows, with the section holding `cursor` marked. Indented by
/// level, which is what makes it an outline: the shape of the draft is the
/// thing being looked at.
fn outline_items(sections: &[Section], cursor: usize) -> Vec<ui::Item> {
    // The one the cursor is in: the last heading at or before it. Marked:
    // opening the outline says where the writer is.
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

/// Whether one line of the panel `mode` is showing can take the keyboard. What
/// a line means is the mode's to say: a row with no chips is a document in the
/// Files list, a heading in the outline, and a fact on the Help page.
fn line_takes_focus(mode: &Mode, item: &ui::Item) -> bool {
    match mode {
        Mode::Files(_) | Mode::Outline(_) => matches!(item, ui::Item::Row { .. }),
        Mode::Config => !ui::takeable(item).is_empty(),
        Mode::Help | Mode::Writing | Mode::Naming { .. } => false,
    }
}

/// The next line that can take the keyboard, `down` or up from `from`, wrapping
/// at the ends. `None` when no line can take it. Over the whole list, not the
/// page on screen: `from` is `None` before the first press.
fn next_focusable(takes: &[bool], from: Option<usize>, down: bool) -> Option<usize> {
    let n = takes.len();
    if n == 0 {
        return None;
    }
    let start = from.unwrap_or(if down { n - 1 } else { 0 });
    (1..=n)
        .map(|step| {
            if down {
                (start + step) % n
            } else {
                (start + n - step % n) % n
            }
        })
        .find(|at| takes[*at])
}

/// A strip label as it is drawn. Empty stays empty: a cell with nothing to say
/// is blank, not a pair of empty brackets. The find bar's count is blank
/// until something has been typed.
fn bracket(label: &str) -> String {
    if label.is_empty() {
        String::new()
    } else {
        format!("[ {label} ]")
    }
}

/// Whether a character may go in a filename. Only the three that make a path of
/// it are barred. Han, kana and accented Latin are all good filenames here.
fn in_filename(c: char) -> bool {
    !matches!(c, '/' | '\\' | '\0')
}

/// A search in progress. Not a `Mode`: every mode is a full-screen panel over
/// the document, and the point of a find is watching the page move to the
/// match. The bar takes over the strip.
#[derive(Debug, Default)]
struct Find {
    /// What has been typed into the bar.
    query: String,
    /// Every place it occurs, recomputed on each keystroke — see
    /// [`karyll_core::find::matches`] on why all of them and not one.
    hits: Vec<std::ops::Range<usize>>,
    /// Which hit the page is showing, an index into `hits`.
    at: usize,
    /// What to put in place of a match, and whether the bar is asking for it. A
    /// state of the find bar and not a bar of its own: the query, the hits
    /// and the stepping are all the same.
    replacing: bool,
    with: String,
    /// Which of the two fields the keys are going into.
    field: Field,
    /// Whether `[ All ]` has been tapped once. Two taps, the rule the Delete
    /// chip follows: replacing all changes places the writer cannot see, and
    /// the chip says which tap it is on.
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
    /// The strip cell a field is drawn in, and the reverse. One statement of
    /// the correspondence — [`Editor::strip_wanted`] alone decides where on the
    /// strip either field sits.
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

/// A document as the Files panel knows it. Read once, when the panel opens:
/// `panel_items` is asked four times for a single tap, and the count is a fact
/// about the file at the moment it was listed.
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

/// How long ago, in the coarsest unit that says something. No calendar
/// and no timezone: neither is in `std`, and "3 days ago" is what a writer
/// wants to know about a draft.
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
        // The one unit with a word of its own.
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
        // Said in words as well as in bold: this is the document the strip's
        // Rename acts on.
        format!("open  ·  {words}  ·  {when}")
    } else {
        format!("{words}  ·  {when}")
    }
}

/// A path for a new document, numbered. The clock on a device that has been
/// asleep is not to be trusted for naming.
fn new_document() -> PathBuf {
    for n in 1..1000 {
        // `exists` alone. Checking the listing asks the same question of the
        // same directory twice, and the listing reads every document to count
        // its words.
        let path = PathBuf::from(format!("{DOCUMENTS}/draft-{n}.md"));
        if !path.exists() {
            return path;
        }
    }
    PathBuf::from(format!("{DOCUMENTS}/draft.md"))
}

/// How often to ask the window manager which way the screen is. A subprocess
/// each time: not every tick — but often enough that a flip does not leave the
/// buttons dead for long.
const ORIENTATION_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// How far a finger may wander and count as having stayed put. At 300 ppi this
/// is about 3.5 mm, separating a drag from a tap and a double-tap from two
/// taps. Too forgiving means a very short drag places the cursor.
const TOUCH_SLOP: u16 = 40;

/// How long the second tap of a double-tap may take to arrive.
const DOUBLE_TAP: std::time::Duration = std::time::Duration::from_millis(400);

/// How long an inverted button stays inverted, however briefly it was touched.
/// Sized for the panel: an update takes about 10 ms, and the ink needs roughly
/// a quarter of a second to settle.
const FEEDBACK: std::time::Duration = std::time::Duration::from_millis(300);

/// A button on the action strip. The label travels with the action, not matched
/// as a string in a second place: a strip keyed on labels stops doing anything
/// the moment one is renamed.
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
    /// The headings of the open document. Beside Files: the pair are one
    /// question at two scales — which document, and where in it.
    Outline,
    /// Leave a panel.
    Done,
    /// Abandon a name being typed.
    Cancel,
    /// Paging a list too long for the panel: back, where you are, and on. All
    /// three or none — a lone `More` leaves the last page with no way back.
    /// Named apart from the find bar's `Previous`/`Next`/`Count`.
    PageBack,
    PageAt,
    PageOn,
    /// The find bar's own cells: the field, how many hits there are and which
    /// one is showing, and the two steps between them. Their labels are what
    /// has been typed: [`Editor::strip_labels`] fills them in.
    Query,
    Count,
    Previous,
    Next,
    /// Ask for the second field as well. On the find bar, where the query is
    /// typed: a writer changing it types it once.
    Replace,
    /// The second field, and the two things that can be done with it: this
    /// match, or every match.
    With,
    Change,
    All,
    /// Start a document. On the Files strip, beside the list it adds to.
    New,
    /// Rename the open document — the one the Files list marks `open`, in
    /// words as well as in bold.
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
            // The find bar's words: the same stepping gesture, with a readout.
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
    /// Only where a shorter word means the same thing: `Prev` for `Previous`.
    /// `Config` and `Replace` have no such form.
    fn short(self) -> &'static str {
        match self {
            Bar::New => "New",
            Bar::Previous | Bar::PageBack => "Prev",
            other => other.label(),
        }
    }

    /// Whether this cell reports something or does something. It is
    /// what a strip gives up when it cannot fit: the count of matches before
    /// `[ Done ]`.
    fn is_readout(self) -> bool {
        matches!(self, Bar::Count | Bar::PageAt)
    }
}

/// What a chip in Config's Keyboard section does. Built alongside the
/// labels, holding the two together.
#[derive(Debug, Clone)]
enum KeyAction {
    /// Drop the link, keeping the pairing.
    Disconnect(hid::Device),
    /// Remove it, and its saved link key: it can be paired afresh.
    Forget(hid::Device),
    /// Pair with something the scan turned up.
    Pair(hid::Device),
    Scan,
}

/// What a line of the Config panel does. Built alongside its label, as
/// [`KeyAction`] is. The two with a list in them carry the list the chips
/// were drawn from.
#[derive(Debug, Clone)]
enum ConfigRow {
    /// A heading. Not tappable. One list holds every row.
    None,
    /// The language chips: which source each option switches on or off.
    Languages(Vec<Language>),
    /// A writing system's chips, and which family each option is — an index
    /// into [`font::families`], skipping the ones not installed.
    Font(font::Group, Vec<usize>),
    /// The body size chips, which are [`render::SIZES`] in order.
    Size,
    /// The margin chips, which are [`render::MARGINS`] in order.
    Margins,
    /// Whether a stop hangs past the measure. Option 1 hangs.
    Hanging,
    /// Whether words are divided at the end of a line. Option 1 divides.
    Hyphenation,
    /// One keyboard's chips, or the scan's.
    Keyboard(Vec<KeyAction>),
    /// Which key beside the space bar carries ⌘. Option 1 is Alt.
    Modifiers,
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

/// Where the keyboard is in a panel: which line of the whole list, and which of
/// that line's chips. Absolute, where [`ui::Focus`] is page-relative: the list
/// is what the keyboard walks, and the page follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PanelFocus {
    row: usize,
    chip: usize,
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

/// One level of the outline's indent. Spaces, in the label column, the rows
/// being laid out to the panel's one grid. Four reads as a step without pushing
/// a sixth-level heading off the page.
const OUTLINE_STEP: &str = "    ";

/// The welcome document, for the one path that has no file: the binary run by
/// hand with no argument. The same file the launcher copies into an empty
/// documents directory, and the specimen every formatting kind is in.
const SPECIMEN: &str = include_str!("../../../device/extensions/karyll/share/Welcome.md");

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A line takes the keyboard where taking it does something, a question
    /// about the panel and not about the row: the outline's rows carry no chip
    /// and go somewhere, Help's carry none and do not.
    #[test]
    fn what_takes_the_keyboard_depends_on_the_panel() {
        let row = ui::Item::Row {
            label: "draft.md".into(),
            detail: String::new(),
            on: false,
            action: None,
        };
        let heading = ui::Item::Heading("Type".into());
        let choice = ui::Item::Choice {
            label: "Size".into(),
            options: vec!["46".into()],
            on: vec![true],
            inert: Vec::new(),
        };
        for mode in [Mode::Files(Vec::new()), Mode::Outline(Vec::new())] {
            assert!(line_takes_focus(&mode, &row));
            assert!(!line_takes_focus(&mode, &heading));
        }
        assert!(line_takes_focus(&Mode::Config, &choice));
        assert!(!line_takes_focus(&Mode::Config, &heading));
        assert!(
            !line_takes_focus(&Mode::Config, &row),
            "a bare row on Config is a label"
        );
        assert!(
            !line_takes_focus(&Mode::Help, &row),
            "Help is a page of facts"
        );
    }

    /// Walking the list steps over the lines that do nothing and comes round.
    /// A list whose last line is a dead end has to be read to be left.
    #[test]
    fn walking_a_list_skips_the_lines_that_do_nothing() {
        let takes = [false, true, false, true, false];
        assert_eq!(next_focusable(&takes, None, true), Some(1));
        assert_eq!(next_focusable(&takes, Some(1), true), Some(3));
        assert_eq!(next_focusable(&takes, Some(3), true), Some(1), "wraps");
        assert_eq!(next_focusable(&takes, None, false), Some(3));
        assert_eq!(next_focusable(&takes, Some(3), false), Some(1));
        assert_eq!(next_focusable(&takes, Some(1), false), Some(3), "wraps");
        // A page where nothing can be taken — Help — says so.
        assert_eq!(next_focusable(&[false, false], None, true), None);
        assert_eq!(next_focusable(&[], None, true), None);
    }

    /// The specimen has a job: it is the document a fresh install opens onto
    /// and the one thing looked at when the type is wrong on device. A
    /// formatting kind missing from it is a kind nobody checks.
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
            // The specimen says nesting works: it has to.
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

    /// Every shortcut that opens a surface closes it.
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
            // Config from the Files list is Config, not a way out. The chord
            // matching the surface is the one that closes it.
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
            // On a plain find bar the chord reveals the second field.
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
        /// words, and it stops at the next heading of **any** level. A section
        /// and its subsections do not count the same prose twice.
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

        /// Opening the outline says where the writer is, before they choose
        /// where to go.
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

        /// A heading with nothing after the hashes gets a row of its own. A
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

    /// Help is where the long lines are, and it is the page a writer reads when
    /// something is not working. Both orientations of all three panels:
    /// a portrait panel is a third narrower than the landscape beside it.
    #[test]
    fn no_help_line_runs_into_another_on_any_panel() {
        use crate::font::Proportional;
        let items = help_items();
        for panel in [1264u16, 1272, 1680, 1696, 1860, 2480] {
            let mut measure = |s: &str| ui::label_width(&mut Proportional, s, ui::TEXT_PX);
            let column = ui::chip_column(&items, panel, &mut measure);
            for item in &items {
                let ui::Item::Row { label, detail, .. } = item else {
                    continue;
                };
                let end = column + measure(detail);
                assert!(
                    end <= panel - ui::MARGIN_X,
                    "on a {panel} px panel, the detail of {label:?} runs to {end}"
                );
                let room = ui::label_room(item, column, panel, &mut measure);
                let drawn = ui::elided(label, room, &mut measure);
                assert!(
                    ui::ROW_INSET + measure(&drawn) < column,
                    "on a {panel} px panel, {drawn:?} runs under its own detail"
                );
            }
        }
    }

    /// Which candidates are on the bar, given where the pages fall. The split
    /// itself is [`ui::candidate_pages`]'s and tested there.
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
        /// slice past the end of it takes the editor down mid-word.
        #[test]
        fn a_page_past_the_end_is_empty() {
            assert!(candidate_page(&list(), &[0, 2], 2).is_empty());
            assert!(candidate_page(&list(), &[], 0).is_empty());
            assert!(candidate_page(&[], &[0], 0).is_empty());
            assert!(candidate_page(&list(), &[0, 99], 1).is_empty());
        }
    }

    /// Every strip [`Editor::strip_wanted`] can build, on every panel karyll
    /// targets. A dropped cell is a control that is not there, and the strip is
    /// what a writer with no keyboard has in place of shortcuts.
    mod strips {
        use super::*;
        use crate::font::Proportional;

        /// The panels karyll targets, narrowest first.
        const PANELS: [u16; 3] = [1264, 1272, 1860];

        /// The strips, as [`Editor::strip_wanted`] builds them. Written out
        /// here; an `Editor` needs a window.
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

        /// Every control stays on the strip, at every width. The strip gives up
        /// its longer words and then its readouts; [`ui::cell_bounds`] drops
        /// from the tail, where `[ Done ]` sits.
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
        /// control that is present and useless. The count above passes it.
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

        /// The bar's two states, as [`Editor::strip_wanted`] builds them.
        /// Written out here: an `Editor` needs a window.
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
            // And a bar with no field on it has nothing elastic: the remainder
            // falls to the status line.
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
            // Reordering the bar moves the answer with it, leaving no stale
            // index behind.
            let reversed: Vec<Bar> = REPLACING.iter().rev().copied().collect();
            assert_eq!(stretch_cells(&reversed), vec![6, 7]);
        }

        /// A cell that is not a field is not one: nothing else on the strip
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
        // With nothing paired the strip is the only way out of the app.
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
        // The one unit with a word of its own.
        assert_eq!(secs(86_400), "yesterday");
        assert_eq!(secs(172_799), "yesterday");
        assert_eq!(secs(172_800), "2 days ago");
        assert_eq!(secs(2_591_999), "29 days ago");
        assert_eq!(secs(2_592_000), "1 month ago");
        assert_eq!(secs(31_535_999), "12 months ago");
        assert_eq!(secs(31_536_000), "1 year ago");
    }

    /// The device sets its clock after a sleep: a file can carry a stamp
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
        // The two conventions are separate settings: Han unification gives
        // them one code point and two correct glyphs.
        assert_eq!(
            font_groups(&[Language::Chinese, Language::ChineseTraditional]),
            vec![
                font::Group::Han(Region::Simplified),
                font::Group::Han(Region::Traditional)
            ]
        );
        // Korean is a row of its own: the Korean faces carry no Hanja and the
        // Han faces carry no Hangul.
        assert_eq!(font_groups(&[Language::Korean]), vec![font::Group::Hangul]);
        assert_eq!(
            font_groups(&[Language::English, Language::Japanese, Language::Korean]),
            vec![
                font::Group::Latin,
                font::Group::Han(Region::Japanese),
                font::Group::Hangul
            ]
        );
    }

    /// Every option reaches a row, in order and exactly once. The chip a finger
    /// lands on is looked up through the list its row was drawn from. An option
    /// dropped or duplicated by the split sets the wrong family.
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
        // The two cannot both be open — the find bar takes the strip — and the
        // precedence is stated, not left to a branch order.
        assert_eq!(sink_for(true, true), Sink::Name);
    }

    #[test]
    fn a_composition_bound_elsewhere_is_not_in_the_document() {
        // The page splices its composition into the text, which moves every
        // index after the cursor. A word typed into the find bar moves nothing:
        // the hits are document indices.
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
        // Nothing typed yet: "not found" for an empty search answers a
        // question nobody asked.
        assert_eq!(find_count(true, false, false, 0, 0), "");
        // And nothing while a word is being composed into the query, that field
        // then showing the query plus a half-typed word. A word composed into
        // the replacement leaves the query alone, and the caller passes false.
        assert_eq!(find_count(false, true, false, 2, 12), "");
    }

    #[test]
    fn a_filename_may_be_written_in_any_script() {
        // Only what makes a path of it is barred. A writer who works in
        // Chinese names a document in Chinese.
        for c in ['日', '本', 'ぬ', 'é', '中', '_', ' ', '.'] {
            assert!(in_filename(c), "{c} is a perfectly good filename");
        }
        for c in ['/', '\\', '\0'] {
            assert!(!in_filename(c));
        }
    }

    #[test]
    fn a_descriptor_that_can_no_longer_deliver_counts_as_ready() {
        // `evdev_poll` returns `EPOLLHUP | EPOLLERR` and never `EPOLLIN` once
        // the device is gone, and an invalid descriptor has the same shape. A
        // pipe does not: its read end is readable at EOF.
        const GONE: std::os::unix::io::RawFd = 1_000_000;
        assert_eq!(
            wait(&[GONE], 0).unwrap(),
            vec![true],
            "a descriptor that will never deliver again has to wake the loop"
        );

        // And the other half, which makes "any `revents`" safe to act on: an
        // open, idle descriptor reports nothing. `POLLOUT` is not asked for,
        // and the kernel masks `revents` to what was asked plus the error bits.
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
        // A search puts the bar on the strip, and a hidden field is not a
        // field: the strip stays whatever the chrome flag says.
        assert!(strip_visible(true, true, true, true));
    }

    #[test]
    fn a_panel_always_has_its_strip_whatever_the_writing_screen_was_doing() {
        // Opening a panel from the keyboard leaves the hidden flag set by the
        // keystroke that opened it, and `set_chrome_hidden` declines to touch
        // the flag outside `Mode::Writing`. A panel's strip is its controls.
        assert!(strip_visible(true, true, false, false));
    }

    #[test]
    fn mid_sentence_does_not_autosave() {
        // A pause between keystrokes is not the writer stopping.
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
        // Someone who never pauses is written out on this backstop.
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

    /// The backstop is the looser of the two; the pause fires first.
    #[test]
    fn the_backstop_is_the_looser_of_the_two() {
        assert!(AUTOSAVE_MAX > AUTOSAVE_IDLE);
    }

    mod language {
        use super::*;

        /// A language names its keyboard. Pinyin and romaji are both defined
        /// against the QWERTY letter arrangement: Chinese and Japanese are US
        /// however the last prose was typed.
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
            // Two enabled sources cycle in two presses.
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
            // Turning off the source in use leaves the keyboard somewhere the
            // cycle reaches, moving forward from where that source sat.
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
        /// while Japanese is a separate one. The failures are loading the
        /// plugin twice and asking XT9 for kana.
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
        /// converted: the device has exactly one Chinese dictionary.
        #[test]
        fn the_two_chinese_entries_differ_only_in_script() {
            assert_eq!(
                Language::Chinese.layout(),
                Language::ChineseTraditional.layout()
            );
            assert!(!Language::Chinese.traditional());
            assert!(Language::ChineseTraditional.traditional());
        }

        /// Nothing but Traditional asks for conversion. A Latin language
        /// reaching the converter is the engine consulted for text it never
        /// produced.
        #[test]
        fn only_traditional_converts() {
            assert_eq!(Language::ALL.iter().filter(|l| l.traditional()).count(), 1);
        }

        /// The remembered letter survives a round trip.
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

        /// A draft opens where it was left.
        #[test]
        fn a_remembered_place_is_where_the_document_opens() {
            assert_eq!(opening_cursor_from(Some(1200), 5000), 1200);
        }

        /// The fallback is the top.
        #[test]
        fn a_document_never_seen_before_opens_at_its_top() {
            assert_eq!(opening_cursor_from(None, 5000), 0);
            assert_eq!(opening_cursor_from(None, 0), 0);
        }

        /// The file is plain Markdown on a volume that mounts over USB: it
        /// can have been shortened elsewhere between sessions.
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

        /// A path is allowed to contain the separator: the index is written
        /// first, and everything after it is the path.
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

        /// Writing a place again replaces the old one. Stacking grows the file
        /// by a line per save and finds the stale entry first.
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
            // Newest first: the draft just written is the one kept.
            assert_eq!(entries[0].1, format!("/{}.md", POSITIONS_KEPT + 19));
        }
    }
}
