//! karyll — a Markdown writing app for the Kindle Scribe.

mod font;

mod render;
mod screenshot;

mod window;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use karyll_core::Document;

use font::Metrics as _;

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

    /// Where the page ends: above the strip, or the foot of the panel when the
    /// strip is out of the way.
    fn page_bottom(&mut self) -> u16 {
        if self.strip_visible() {
            self.layout().strip_top
        } else {
            self.window.height()
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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

}
