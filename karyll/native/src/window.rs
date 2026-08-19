//! The display surface: a real WM-managed X11 window.
//!
//! Not raw `/dev/fb0`. A window means the lab126 compositor owns the surface —
//! it shows us fullscreen and recomposites the whole screen when we are torn
//! down, so exiting leaves a live home screen instead of a stuck frame. This is
//! the model kterm uses, and the one proven on this hardware.
//!
//! Two consequences worth knowing before tuning refresh:
//!
//! - **The X server picks the eink waveform, not us.** Drawing through a window
//!   means we cannot ask for DU or GC16 the way an `eips` caller can. What we
//!   still control is how much we dirty, which is why layout produces damage
//!   rectangles and a keystroke presents one line rather than the page.
//! - **The compositor rotates our window to the framework orientation.** We
//!   render identity and never rotate pixels, or it happens twice. Input is
//!   read raw from evdev, which is panel-fixed, so that side re-orients
//!   instead.
//!
//! The backing store is one byte per pixel **on every device, colour included**.
//! Carrying RGB would cost 1860×2480 13.8 MB instead of 4.6 against ~514 MB
//! shared with the framework, and it is not needed: the byte is a grey level
//! everywhere except for the handful of [`ink`] indices, which [`Palette`] turns
//! into colours as the band goes out on the wire. So a colour panel costs the
//! same memory as a grey one, and the two grey Kindles share its code paths.

use std::os::unix::io::{AsRawFd, RawFd};
use std::time::Instant;

use anyhow::{Context, Result};
use x11rb::connection::Connection;

use crate::orientation::Orientation;
// `maximum_request_bytes` is BIG-REQUESTS-aware and lives on this trait.
use x11rb::connection::RequestConnection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt, CreateGCAux, CreateWindowAux, EventMask, Gcontext, ImageFormat,
    PropMode, Visibility, Window as XWindow, WindowClass,
};
use x11rb::rust_connection::RustConnection;
// `change_property8` lives in the wrapper `ConnectionExt`.
use x11rb::wrapper::ConnectionExt as _;

pub const WHITE: u8 = 0xFF;
pub const BLACK: u8 = 0x00;
/// Ink for marks that should recede rather than read: Markdown syntax and URLs.
///
/// **This asks the panel for one extra level, not for a ramp.** Coverage is
/// still thresholded at 0.5, so every pixel is one of two values and the edges
/// stay as hard as the body text's; only the value changes. That is a different
/// request from antialiasing, which is what the two-level partial waveform
/// genuinely cannot represent, and a 16-level panel has fifteen levels spare.
///
/// **Not a dither.** Dither averages over an *area*, and a stem at this size is
/// 2–4 px — about one pattern cell — so it deletes half the mark instead of
/// lightening it, and Han fares worse still.
pub const QUIET: u8 = 0x88;

/// A field behind text, on a panel with no colour to make one with.
///
/// Light enough that black prose on it is still black prose. This is what a
/// `==highlight==` is drawn in on a grey Kindle; a colour one swaps the value
/// for [`ink::FIELD`] and keeps everything else about it.
pub const FIELD: u8 = 0xCC;

/// The same field on a row focus mode has set back.
pub const FIELD_QUIET: u8 = 0xE4;

/// Palette indices — **never a grey level**.
///
/// These are only ever written on a panel that has colour, so their numeric
/// values mean nothing except "not one of the greys above". Every other byte in
/// the backing store is a luminance, and on an 8-bit visual reaches the panel
/// as one without passing through a palette at all.
///
/// Kept low and contiguous so the match in `Palette::pixel` is a small jump
/// table.
pub mod ink {
    /// The caret.
    pub const CARET: u8 = 0x01;
    /// A `==highlight==` field on the focused row.
    pub const FIELD: u8 = 0x02;
    /// The rule along the bottom of one.
    pub const FIELD_RULE: u8 = 0x03;
    /// The first of [`super::COLOURS`] as its own index.
    ///
    /// The three above are whatever the writer has *chosen*; these are the
    /// colours themselves, so the picker in Config can show all six at once
    /// while the caret is only ever one of them.
    pub const SWATCH: u8 = 0x04;

    /// The index a swatch of `COLOURS[at]` is drawn in.
    pub fn swatch(at: usize) -> u8 {
        SWATCH + at as u8
    }
}

/// A colour the caret and the highlighter can be set to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colour {
    pub name: &'static str,
    /// The caret, and the rule along the bottom of a field.
    pub rgb: (u8, u8, u8),
    /// The field itself: the same hue, pale enough to read prose through.
    pub wash: (u8, u8, u8),
}

/// The six colours iA Writer offers, in its own order.
///
/// **Sampled from its picker rather than chosen**, the way every other value
/// here was. What the picker shows is the *saturated* member of the pair: iA
/// Writer draws the chosen colour as the highlight's underline and a pale wash
/// of it as the field, which is why one entry carries two values.
///
/// `wash` is iA Writer's own for the two hues a capture pins down, and for the
/// rest it is the hue taken to the lightness those two share — so a purple
/// field is as pale as a yellow one rather than a slab with text on it.
pub const COLOURS: [Colour; 6] = [
    Colour {
        name: "yellow",
        rgb: (0xff, 0xd6, 0x00),
        wash: (0xfb, 0xec, 0xa2),
    },
    Colour {
        name: "orange",
        rgb: (0xff, 0x7e, 0x42),
        wash: (0xf9, 0xca, 0xb4),
    },
    Colour {
        name: "pink",
        rgb: (0xf2, 0x1b, 0xb1),
        wash: (0xf9, 0xb4, 0xe4),
    },
    Colour {
        name: "purple",
        rgb: (0x7d, 0x26, 0xf2),
        wash: (0xd1, 0xb4, 0xf9),
    },
    Colour {
        name: "blue",
        rgb: (0x00, 0xbf, 0xff),
        wash: (0xc5, 0xeb, 0xf8),
    },
    Colour {
        name: "green",
        rgb: (0x98, 0xe3, 0x00),
        wash: (0xe2, 0xf9, 0xb4),
    },
];

/// Which of [`COLOURS`] the caret and the highlighter are set to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inks {
    pub caret: usize,
    pub highlight: usize,
}

impl Default for Inks {
    /// iA Writer's own pair.
    fn default() -> Self {
        Inks {
            caret: 4,
            highlight: 0,
        }
    }
}

impl Inks {
    /// The pair named in a settings file, falling back a name at a time — an
    /// unreadable half should not cost the half that reads.
    pub fn parse(text: &str) -> Self {
        let mut names = text.split_whitespace();
        let at = |name: Option<&str>, fallback| {
            name.and_then(|name| COLOURS.iter().position(|c| c.name == name))
                .unwrap_or(fallback)
        };
        let default = Inks::default();
        Inks {
            caret: at(names.next(), default.caret),
            highlight: at(names.next(), default.highlight),
        }
    }
}

impl std::fmt::Display for Inks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}",
            COLOURS[self.caret].name, COLOURS[self.highlight].name
        )
    }
}

/// How a backing-store byte becomes a pixel on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    /// Every byte is a luminance: memcpy'd to an 8-bit visual, replicated
    /// across the channels of a deeper one.
    Grey,
    /// A panel with a colour filter array in front of it, on a visual deep
    /// enough to address it.
    ///
    /// **The channel positions are read off the visual, not assumed.** A
    /// TrueColor visual states its own masks, and two of them swapped turns the
    /// caret orange on a panel nothing here can see.
    Colour {
        shifts: (u32, u32, u32),
        /// Bits the visual does not use: the pad at depth 24, alpha at 32. Set,
        /// so a 32-bit visual does not draw the page fully transparent.
        pad: u32,
        lsb_first: bool,
        /// What the writer has picked. Carried here rather than beside the
        /// palette because this is the one place a byte becomes a colour, and
        /// a second copy of the choice is a second answer to that.
        inks: Inks,
    },
}

impl Palette {
    /// The pixel an ink byte becomes.
    ///
    /// Only the handful of [`ink`] indices are colours; every other value is
    /// still the luminance it always was, so prose, syntax marks and paper come
    /// out of the same arm they would on a grey panel.
    fn pixel(self, v: u8) -> [u8; 4] {
        let Palette::Colour {
            shifts,
            pad,
            lsb_first,
            inks,
        } = self
        else {
            return [v, v, v, 0xFF];
        };
        // **One colour drives both halves of a highlight.** The rule along the
        // bottom is the chosen colour and the field is its wash, which is the
        // pairing iA Writer draws: the field is what the prose stays readable
        // through, the rule is what gives the run an edge. A field dark enough
        // to draw its own edge is a slab with text on it.
        let last = ink::swatch(COLOURS.len() - 1);
        let (r, g, b) = match v {
            ink::CARET => COLOURS[inks.caret].rgb,
            ink::FIELD => COLOURS[inks.highlight].wash,
            ink::FIELD_RULE => COLOURS[inks.highlight].rgb,
            at if (ink::SWATCH..=last).contains(&at) => COLOURS[(at - ink::SWATCH) as usize].rgb,
            grey => (grey, grey, grey),
        };
        let (rs, gs, bs) = shifts;
        let value = ((r as u32) << rs) | ((g as u32) << gs) | ((b as u32) << bs) | pad;
        if lsb_first {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        }
    }
}

/// Where the lowest bit of `mask` sits.
fn shift_of(mask: u32) -> u32 {
    if mask == 0 { 0 } else { mask.trailing_zeros() }
}

/// Whether the panel has a colour filter array in front of it.
///
/// **Asked of the panel, not of the visual.** A depth-24 TrueColor visual says
/// the X server can represent a colour, not that the hardware can show one, and
/// on this family those are different claims: the Colorsoft's device tree
/// carries an `epd/cfa_panel` node and the Oasis 2's whole `/sys` has nothing
/// matching `cfa` at all.
///
/// Unreadable means grey: a grey panel handed colour indices draws them as the
/// near-black luminances they numerically are.
fn has_cfa() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/firmware/devicetree/base") else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let mut path = entry.path();
        path.push("epd");
        path.push("cfa_panel");
        path.exists()
    })
}

/// What the server had to say about our window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Still mapped. `expose` is set when the server asked for a repaint, and
    /// `resized` when the window changed shape — which is how a rotation
    /// arrives.
    Live { expose: bool, resized: bool },
    /// The window is gone and the app should exit.
    Gone,
}

/// A rectangle of the backing store, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

pub struct Window {
    conn: RustConnection,
    win: XWindow,
    gc: Gcontext,
    depth: u8,
    /// The palette in force. `Grey` whenever colour is switched off, so a byte
    /// left in the backing store by the previous setting cannot come out as a
    /// colour after it.
    palette: Palette,
    /// What the panel could do, kept across a switch-off so it can be switched
    /// back on without asking `/sys` again.
    capable: Palette,
    width: u16,
    height: u16,
    /// One byte per pixel, row-major, `WHITE` for paper.
    pixels: Vec<u8>,
    app_id: String,
    orientation: Orientation,
    burial: Burial,
}

/// Whether the framework's screen is over the editor's, and since when.
///
/// **Watched, not fought.** A `configure_window` raise and a re-set of the
/// window name — the only levers an app has on this manager — are ignored while
/// it is burying us: measured on the device, six burials answered three times
/// each inside the first four seconds came back after 10.3, 12.2, 19.8, 15.5,
/// 22.9 and 10.2 seconds, which is the manager's own schedule and not a reply.
/// So the editor waits it out, and what it does with this is stay off the
/// touchscreen while it cannot be seen — see [`Window::buried`].
#[derive(Default)]
struct Burial {
    /// Whether the last thing the server said was that the window is wholly
    /// covered. A burial is a state, not an event: it is reported once and then
    /// holds until something uncovers us.
    under: bool,
    /// When the window went under, so that coming back can report how long it
    /// took. Nothing else in the log carries a duration, and this is the only
    /// measure of a delay a writer waits through.
    since: Option<Instant>,
}

impl Burial {
    /// Fold in what the server said about the window.
    fn saw(&mut self, state: Visibility, now: Instant) {
        match state {
            Visibility::FULLY_OBSCURED => {
                if self.since.is_none() {
                    self.since = Some(now);
                    eprintln!("window: buried by the framework");
                }
                self.under = true;
            }
            Visibility::UNOBSCURED => {
                if let Some(since) = self.since.take() {
                    eprintln!(
                        "window: back in front after {:.1}s",
                        now.duration_since(since).as_secs_f32()
                    );
                }
                self.under = false;
            }
            // Any of it showing is enough for a tap to have been meant for us.
            _ => self.under = false,
        }
    }
}

/// Decide the palette from the screen's own visual and the panel in front of it.
///
/// Logged either way. A Colorsoft that came up grey because `/sys` was not
/// readable is a silent, puzzling loss of every colour on the device, and the
/// one line that says so costs nothing.
fn palette_for(conn: &RustConnection, screen: &x11rb::protocol::xproto::Screen) -> Palette {
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|d| d.visuals.iter())
        .find(|v| v.visual_id == screen.root_visual);
    let cfa = has_cfa();
    match visual {
        Some(v) if cfa && screen.root_depth > 8 && v.red_mask != 0 => {
            let shifts = (
                shift_of(v.red_mask),
                shift_of(v.green_mask),
                shift_of(v.blue_mask),
            );
            let used = v.red_mask | v.green_mask | v.blue_mask;
            let pad = !used & mask_for(screen.root_depth.max(24));
            eprintln!(
                "window: colour panel, depth {} masks {:06x}/{:06x}/{:06x}",
                screen.root_depth, v.red_mask, v.green_mask, v.blue_mask
            );
            Palette::Colour {
                shifts,
                pad,
                lsb_first: conn.setup().image_byte_order
                    == x11rb::protocol::xproto::ImageOrder::LSB_FIRST,
                inks: Inks::default(),
            }
        }
        _ => {
            eprintln!(
                "window: grey panel, depth {} cfa {}",
                screen.root_depth, cfa
            );
            Palette::Grey
        }
    }
}

/// All the bits a visual of this depth can address.
fn mask_for(depth: u8) -> u32 {
    if depth >= 32 {
        u32::MAX
    } else {
        (1u32 << depth) - 1
    }
}

/// The lab126 window manager reads the window name as a layout spec rather than
/// as a title: application layer, no chrome, fullscreen, and an orientation.
/// This is the shape booklets use, and what gets a window shown undecorated.
fn set_name(
    conn: &RustConnection,
    win: XWindow,
    app_id: &str,
    orientation: Orientation,
) -> Result<()> {
    let name = format!(
        "L:A_N:application_ID:{app_id}_PC:N_O:{}",
        orientation.letter()
    );
    conn.change_property8(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        name.as_bytes(),
    )
    .context("set WM_NAME")?;
    Ok(())
}

impl Window {
    /// Map a fullscreen window and clear it to paper white.
    pub fn open(app_id: &str, orientation: Orientation) -> Result<Self> {
        let (conn, screen_num) = x11rb::connect(None).context("connect to X ($DISPLAY)")?;
        let screen = conn.setup().roots[screen_num].clone();
        let (width, height) = (screen.width_in_pixels, screen.height_in_pixels);
        let depth = screen.root_depth;
        let palette = palette_for(&conn, &screen);

        let win = conn.generate_id().context("generate_id window")?;
        conn.create_window(
            depth,
            win,
            screen.root,
            0,
            0,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new()
                .background_pixel(screen.white_pixel)
                // **Visibility, because being covered is not being unmapped.**
                // The manager stacks the home screen over the editor rather
                // than taking its window away, so `STRUCTURE_NOTIFY` stays
                // silent through the one event a writer actually notices.
                .event_mask(
                    EventMask::EXPOSURE
                        | EventMask::STRUCTURE_NOTIFY
                        | EventMask::VISIBILITY_CHANGE,
                ),
        )
        .context("create_window")?;

        set_name(&conn, win, app_id, orientation)?;
        conn.map_window(win).context("map_window")?;

        let gc = conn.generate_id().context("generate_id gc")?;
        conn.create_gc(
            gc,
            win,
            &CreateGCAux::new()
                .foreground(screen.black_pixel)
                .background(screen.white_pixel),
        )
        .context("create_gc")?;
        conn.flush().context("flush after map")?;

        let pixels = vec![WHITE; width as usize * height as usize];
        Ok(Self {
            conn,
            win,
            gc,
            depth,
            palette,
            capable: palette,
            width,
            height,
            pixels,
            app_id: app_id.to_string(),
            orientation,
            burial: Burial::default(),
        })
    }

    pub fn orientation(&self) -> Orientation {
        self.orientation
    }

    /// Whether the framework's screen is currently over the editor's.
    ///
    /// **A tap is not ours while this is true.** The touchscreen and the pen
    /// are read, not grabbed, so the framework sees the same contacts the
    /// editor does — which is how a tap on the tile still reaches it while
    /// karyll is buried. Acting on those too means a writer tapping at the
    /// home screen is silently working an editor they cannot see: moving the
    /// caret, opening panels, and eventually finding `[ Exit ]`. The keyboard
    /// is grabbed exclusively and so is nobody else's; what is typed still
    /// belongs to the document and is kept.
    pub fn buried(&self) -> bool {
        self.burial.under
    }

    /// Ask the window manager to turn the window.
    ///
    /// The `_O:` field of the window name is the only lever an app has here —
    /// there is no X request for it, and the accelerometer only ever flips
    /// 180°. The manager answers by resizing us, which arrives as a configure
    /// event and is picked up by [`Window::drain_events`].
    pub fn set_orientation(&mut self, orientation: Orientation) -> Result<()> {
        self.orientation = orientation;
        set_name(&self.conn, self.win, &self.app_id, orientation)?;
        self.conn.flush().context("flush after rotate")?;
        Ok(())
    }

    /// Take on a new size, discarding the backing store, which no longer
    /// describes anything.
    fn resize(&mut self, width: u16, height: u16) {
        eprintln!(
            "window: resized {}x{} -> {width}x{height}",
            self.width, self.height
        );
        self.width = width;
        self.height = height;
        self.pixels = vec![WHITE; width as usize * height as usize];
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn full(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        }
    }

    /// Whether this panel *can* show colour. Config asks, so that the setting
    /// only appears on a Kindle where it means something — the same rule the
    /// Screen section follows for a device that has no tilt sensor.
    pub fn colour_capable(&self) -> bool {
        matches!(self.capable, Palette::Colour { .. })
    }

    /// Whether colour is being drawn right now: the panel has it *and* the
    /// writer has left it on.
    pub fn colour(&self) -> bool {
        matches!(self.palette, Palette::Colour { .. })
    }

    /// Switch colour on or off. Costs a full repaint at the call site, which is
    /// what makes the backing store's old bytes irrelevant.
    pub fn set_colour(&mut self, on: bool) {
        self.palette = if on { self.capable } else { Palette::Grey };
    }

    /// Which of [`COLOURS`] the caret and the highlighter are set to.
    ///
    /// Read off `capable` rather than off the palette in force, so switching
    /// colour off and back on returns the pair the writer picked rather than
    /// the default.
    pub fn colours(&self) -> Inks {
        match self.capable {
            Palette::Colour { inks, .. } => inks,
            Palette::Grey => Inks::default(),
        }
    }

    /// Take a new pair. Costs a full refresh at the call site: a partial update
    /// over ink that is changing hue is what leaves a ghost of the old one.
    pub fn set_colours(&mut self, inks: Inks) {
        let repaint = |palette: Palette| match palette {
            Palette::Colour {
                shifts,
                pad,
                lsb_first,
                ..
            } => Palette::Colour {
                shifts,
                pad,
                lsb_first,
                inks,
            },
            Palette::Grey => Palette::Grey,
        };
        self.capable = repaint(self.capable);
        self.palette = repaint(self.palette);
    }

    /// The value a caret is drawn in: black on a grey panel, the chosen colour
    /// where there is a panel to show one.
    pub fn caret_ink(&self) -> u8 {
        if self.colour() { ink::CARET } else { BLACK }
    }

    /// The value a `==highlight==` field is filled with.
    ///
    /// **`quiet` outranks colour.** Focus mode sets a row back whatever the
    /// panel can do, so a field off the focused sentence is grey even where
    /// there is colour to draw it in.
    pub fn field_ink(&self, quiet: bool) -> u8 {
        match (quiet, self.colour()) {
            (true, _) => FIELD_QUIET,
            (false, true) => ink::FIELD,
            (false, false) => FIELD,
        }
    }

    /// The value of the rule along the bottom of a field. One step down from
    /// the field itself, so the run has an edge without having an outline.
    pub fn field_rule_ink(&self, quiet: bool) -> u8 {
        match (quiet, self.colour()) {
            (true, _) => FIELD,
            (false, true) => ink::FIELD_RULE,
            (false, false) => QUIET,
        }
    }

    pub fn put_pixel(&mut self, x: u16, y: u16, value: u8) {
        if x < self.width && y < self.height {
            self.pixels[y as usize * self.width as usize + x as usize] = value;
        }
    }

    pub fn fill(&mut self, rect: Rect, value: u8) {
        let stride = self.width as usize;
        let x0 = rect.x.min(self.width) as usize;
        let x1 = (rect.x + rect.width).min(self.width) as usize;
        let y1 = (rect.y + rect.height).min(self.height) as usize;
        for y in rect.y as usize..y1 {
            self.pixels[y * stride + x0..y * stride + x1].fill(value);
        }
    }

    /// Send `rect` to the server.
    ///
    /// Split into horizontal bands that each fit one request, so the server
    /// sees whole rows rather than a partially transferred image. Presenting
    /// the smallest rectangle that changed is the only refresh control a
    /// windowed client has.
    pub fn present(&mut self, rect: Rect) -> Result<()> {
        // **Widened to full rows.** The panel does not reliably refresh a
        // narrow column: a quarter-width button could be inverted in the
        // backing store, sent, and never visibly change, while the full-width
        // button next to it worked every time. Rows are cheap — the cost is in
        // how many of them, not how wide.
        let rect = Rect {
            x: 0,
            width: self.width,
            ..rect
        };
        let rect = self.clip(rect);
        if rect.is_empty() {
            return Ok(());
        }
        let bpp = wire_bytes_per_pixel(self.depth);
        let budget = self.conn.maximum_request_bytes();
        let rows_per_band = band_rows(budget, rect.width as usize * bpp);

        let mut y = rect.y;
        let bottom = rect.y + rect.height;
        while y < bottom {
            let rows = rows_per_band.min((bottom - y) as usize) as u16;
            let band = Rect {
                x: rect.x,
                y,
                width: rect.width,
                height: rows,
            };
            let data = encode_band(&self.pixels, self.width as usize, band, bpp, self.palette);
            self.conn
                .put_image(
                    ImageFormat::Z_PIXMAP,
                    self.win,
                    self.gc,
                    band.width,
                    band.height,
                    band.x as i16,
                    band.y as i16,
                    0,
                    self.depth,
                    &data,
                )
                .context("put_image")?;
            y += rows;
        }
        self.conn.flush().context("flush after present")?;
        Ok(())
    }

    fn clip(&self, rect: Rect) -> Rect {
        clip_rect(rect, self.width, self.height)
    }

    /// The connection's socket, so a caller can wait on it alongside the
    /// keyboard instead of choosing one to block on.
    pub fn fd(&self) -> RawFd {
        self.conn.stream().as_raw_fd()
    }

    /// Take whatever the server has sent without blocking.
    ///
    /// Call this before waiting on [`Window::fd`]: x11rb buffers events
    /// internally, so an event already decoded leaves nothing on the socket for
    /// `poll` to report and waiting first would miss it.
    pub fn drain_events(&mut self) -> Result<Surface> {
        let mut expose = false;
        let mut size = None;
        let now = Instant::now();
        while let Some(event) = self.conn.poll_for_event().context("poll_for_event")? {
            match event {
                Event::Expose(_) => expose = true,
                Event::ConfigureNotify(event) => size = Some((event.width, event.height)),
                Event::UnmapNotify(_) | Event::DestroyNotify(_) => return Ok(Surface::Gone),
                Event::VisibilityNotify(event) => self.burial.saw(event.state, now),
                _ => {}
            }
        }
        let resized = match size {
            Some(size) if size != (self.width, self.height) => {
                self.resize(size.0, size.1);
                true
            }
            _ => false,
        };
        Ok(Surface::Live { expose, resized })
    }

    /// Present `rect` and wait for the server to have taken it.
    ///
    /// A plain [`Window::present`] only writes to the socket. Two updates to
    /// the same region queued back to back — which is what an invert and its
    /// restore are — get coalesced, and the panel only ever shows the second
    /// one. The round trip here forces the first to be accepted before the
    /// caller holds it on screen.
    pub fn present_sync(&mut self, rect: Rect) -> Result<()> {
        self.present(rect)?;
        // Any request with a reply is a round trip; this is the cheapest.
        self.conn
            .get_input_focus()
            .context("sync")?
            .reply()
            .context("sync reply")?;
        Ok(())
    }

    /// Re-send the whole backing store, for when the server asks for a repaint
    /// and the content has not changed.
    pub fn refresh(&mut self) -> Result<()> {
        let full = self.full();
        self.present(full)
    }

    /// Drive every pixel to black and hold it there, so the caller can paint the
    /// real content over it.
    ///
    /// **This is what a flashing refresh is, done by hand.** A windowed client
    /// cannot ask the X server for a waveform by name, and re-sending the same
    /// image does not clear anything — the panel has no reason to move a pixel
    /// that is not changing. Ghosting is residue left in cells that have been
    /// nudged one way many times; the cure is to drive them fully the other way
    /// and back, which is exactly a black frame followed by the content.
    ///
    /// **Presented synchronously, and that is the whole trick.** Two updates in
    /// quick succession are coalesced by the server, so without the round trip
    /// the black frame and the content would arrive as one and the panel would
    /// show only the content — a refresh key that did nothing at all. This is
    /// the same coalescing that [`Window::present_sync`] was written for when
    /// press feedback kept vanishing.
    ///
    /// `/usr/sbin/eips -f` is on the device and would do it in one call, but it
    /// writes straight to `/dev/fb0` under an X server that owns the display,
    /// and its flags are unverified here. This needs neither.
    pub fn flash(&mut self) -> Result<()> {
        let full = self.full();
        self.fill(full, BLACK);
        self.present_sync(full)
    }
}

impl Drop for Window {
    /// Tear the window down explicitly so the compositor recomposites the
    /// screen underneath. Leaving it to process exit is what strands a dead
    /// frame and a dead status bar on this firmware.
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.win);
        let _ = self.conn.flush();
    }
}

/// Trim `rect` to the surface, so a damage rectangle computed from a layout
/// that ran against stale geometry cannot index past the backing store.
fn clip_rect(rect: Rect, width: u16, height: u16) -> Rect {
    let x = rect.x.min(width);
    let y = rect.y.min(height);
    Rect {
        x,
        y,
        width: rect.width.min(width - x),
        height: rect.height.min(height - y),
    }
}

/// Bytes the server expects per pixel in a `Z_PIXMAP` image.
///
/// Depth 8 takes the backing store's byte as-is. A deeper visual takes the same
/// grey replicated across the channels — four bytes per pixel on the wire, but
/// it keeps this app working unchanged on a colour panel.
fn wire_bytes_per_pixel(depth: u8) -> usize {
    if depth <= 8 { 1 } else { 4 }
}

/// How many rows of `row_bytes` fit in one request, leaving room for the
/// header. Always at least one, so an oversized row still makes progress
/// rather than looping forever on a zero-row band.
fn band_rows(budget: usize, row_bytes: usize) -> usize {
    (budget.saturating_sub(64) / row_bytes.max(1)).max(1)
}

/// Pack one band of the backing store into `Z_PIXMAP` wire format.
///
/// The 8-bit case is still the memcpy it always was. Both grey devices take it,
/// and neither pays anything for the colour path existing.
fn encode_band(pixels: &[u8], stride: usize, band: Rect, bpp: usize, palette: Palette) -> Vec<u8> {
    let mut out = Vec::with_capacity(band.width as usize * band.height as usize * bpp);
    for y in band.y as usize..(band.y + band.height) as usize {
        let row = &pixels[y * stride + band.x as usize..][..band.width as usize];
        if bpp == 1 {
            out.extend_from_slice(row);
        } else {
            for &v in row {
                out.extend_from_slice(&palette.pixel(v));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rect_with_no_area_is_empty() {
        assert!(
            Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 10
            }
            .is_empty()
        );
        assert!(
            Rect {
                x: 0,
                y: 0,
                width: 10,
                height: 0
            }
            .is_empty()
        );
        assert!(
            !Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 1
            }
            .is_empty()
        );
    }

    #[test]
    fn clipping_keeps_a_rect_inside_the_surface() {
        let inside = Rect {
            x: 10,
            y: 10,
            width: 20,
            height: 20,
        };
        assert_eq!(clip_rect(inside, 100, 100), inside);

        // Overhanging right and bottom edges are trimmed, not wrapped.
        let over = Rect {
            x: 90,
            y: 95,
            width: 50,
            height: 50,
        };
        assert_eq!(
            clip_rect(over, 100, 100),
            Rect {
                x: 90,
                y: 95,
                width: 10,
                height: 5
            }
        );

        // An origin past the edge collapses to empty rather than underflowing.
        let outside = Rect {
            x: 200,
            y: 200,
            width: 10,
            height: 10,
        };
        assert!(clip_rect(outside, 100, 100).is_empty());
    }

    #[test]
    fn wire_width_follows_the_visual_depth() {
        assert_eq!(wire_bytes_per_pixel(8), 1, "the panel this targets");
        assert_eq!(wire_bytes_per_pixel(24), 4);
        assert_eq!(wire_bytes_per_pixel(32), 4);
    }

    #[test]
    fn banding_always_makes_progress() {
        // A full-width row on this panel, at one byte per pixel.
        assert!(band_rows(262_144, 1860) > 1);
        // A row larger than the whole budget still sends one row at a time
        // rather than dividing to zero and spinning.
        assert_eq!(band_rows(64, 100_000), 1);
        assert_eq!(band_rows(0, 1860), 1);
    }

    #[test]
    fn encoding_depth_8_passes_the_backing_store_through() {
        let pixels: Vec<u8> = (0..16).collect();
        let band = Rect {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
        };
        // 4x4 surface: rows 1 and 2, columns 1 and 2.
        assert_eq!(
            encode_band(&pixels, 4, band, 1, Palette::Grey),
            vec![5, 6, 9, 10]
        );
    }

    #[test]
    fn encoding_a_deeper_visual_replicates_the_grey() {
        let pixels = vec![0x40u8, 0x80];
        let band = Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        assert_eq!(
            encode_band(&pixels, 2, band, 4, Palette::Grey),
            vec![0x40, 0x40, 0x40, 0xFF, 0x80, 0x80, 0x80, 0xFF]
        );
    }

    /// A depth-24 TrueColor visual as the Colorsoft's Xorg log describes it.
    fn colorsoft() -> Palette {
        Palette::Colour {
            shifts: (16, 8, 0),
            pad: 0xFF00_0000,
            lsb_first: true,
            inks: Inks::default(),
        }
    }

    #[test]
    fn a_grey_is_still_a_grey_on_a_colour_panel() {
        // Everything that is not one of the handful of colour indices is still
        // a luminance, so prose and paper draw the same on either panel.
        for v in [BLACK, QUIET, FIELD, FIELD_QUIET, WHITE, 0x37] {
            assert_eq!(
                colorsoft().pixel(v),
                [v, v, v, 0xFF],
                "{v:#04x} should still be grey"
            );
        }
    }

    #[test]
    fn the_caret_is_the_blue_that_was_asked_for() {
        // #00bfff, little-endian into an 0xRRGGBB visual: B, G, R, then pad.
        assert_eq!(colorsoft().pixel(ink::CARET), [0xff, 0xbf, 0x00, 0xFF]);
    }

    #[test]
    fn the_field_is_paler_than_the_rule_that_edges_it() {
        // The highlighter is a wash under a line, not a slab. Inverted, the
        // prose would be sitting on the saturated one.
        //
        // Read back through the palette rather than from the literals, so this
        // fails if the colours move.
        let light = |v: u8| {
            let [b, g, r, _] = colorsoft().pixel(v);
            r as u32 + g as u32 + b as u32
        };
        assert!(
            light(ink::FIELD) > light(ink::FIELD_RULE),
            "the field is the paler of the two"
        );
        // And the field stays well clear of the prose drawn on it.
        assert!(light(ink::FIELD) > light(BLACK) + 500);
    }

    #[test]
    fn the_channels_follow_the_visuals_masks_rather_than_a_guess() {
        // The same ink on a BGR visual has to come out as the same *colour*,
        // which means different bytes. Swapping these is how a blue caret
        // becomes an orange one, and one visual's worth of test would not see
        // it.
        let bgr = Palette::Colour {
            shifts: (0, 8, 16),
            pad: 0,
            lsb_first: true,
            inks: Inks::default(),
        };
        assert_eq!(bgr.pixel(ink::CARET), [0x00, 0xbf, 0xff, 0x00]);
        let msb = Palette::Colour {
            shifts: (16, 8, 0),
            pad: 0,
            lsb_first: false,
            inks: Inks::default(),
        };
        assert_eq!(msb.pixel(ink::CARET), [0x00, 0x00, 0xbf, 0xff]);
    }

    #[test]
    fn a_colour_index_is_never_a_grey_karyll_draws() {
        // The indices are only safe because nothing else writes those values.
        let swatches = (0..COLOURS.len()).map(ink::swatch);
        for index in [ink::CARET, ink::FIELD, ink::FIELD_RULE]
            .into_iter()
            .chain(swatches)
        {
            assert!(![BLACK, QUIET, FIELD, FIELD_QUIET, WHITE].contains(&index));
        }
    }

    #[test]
    fn the_caret_and_the_field_follow_what_was_picked() {
        // The three dynamic indices are the *choice*, so setting one has to
        // move the ink that reads it and leave the other alone.
        let picked = |inks| match colorsoft() {
            Palette::Colour {
                shifts,
                pad,
                lsb_first,
                ..
            } => Palette::Colour {
                shifts,
                pad,
                lsb_first,
                inks,
            },
            grey => grey,
        };
        let green = COLOURS.iter().position(|c| c.name == "green").unwrap();
        let p = picked(Inks {
            caret: green,
            highlight: green,
        });
        let rgb = |v: u8| {
            let [b, g, r, _] = p.pixel(v);
            (r, g, b)
        };
        assert_eq!(rgb(ink::CARET), COLOURS[green].rgb);
        assert_eq!(rgb(ink::FIELD_RULE), COLOURS[green].rgb);
        assert_eq!(rgb(ink::FIELD), COLOURS[green].wash);
        // And a swatch is its own colour whatever is picked, or the picker
        // would show six of whatever the caret already is.
        for (at, colour) in COLOURS.iter().enumerate() {
            assert_eq!(rgb(ink::swatch(at)), colour.rgb, "{}", colour.name);
        }
    }

    #[test]
    fn every_field_is_paler_than_the_rule_it_carries() {
        // The pairing has to hold for all six, not just the yellow it was
        // measured on: a wash darker than its own rule is a slab.
        for colour in COLOURS {
            let light = |(r, g, b): (u8, u8, u8)| r as u32 + g as u32 + b as u32;
            assert!(
                light(colour.wash) > light(colour.rgb),
                "{} washes paler than it rules",
                colour.name
            );
            assert!(
                light(colour.wash) > 500,
                "{} is light enough to read black prose on",
                colour.name
            );
        }
    }

    #[test]
    fn a_settings_file_round_trips_a_pair() {
        let inks = Inks {
            caret: 2,
            highlight: 5,
        };
        assert_eq!(Inks::parse(&inks.to_string()), inks);
        // A file that says nothing, or says something that is no longer a
        // colour, leaves the default rather than panicking on an index.
        assert_eq!(Inks::parse(""), Inks::default());
        assert_eq!(Inks::parse("chartreuse mauve"), Inks::default());
        // Half a file keeps the half that reads.
        assert_eq!(Inks::parse("green").caret, 5);
        assert_eq!(Inks::parse("green").highlight, Inks::default().highlight);
    }

    #[test]
    fn a_grey_panel_leaves_every_index_alone() {
        // On the Scribe and the Oasis 2 the byte is a luminance and nothing
        // consults a palette, which is what keeps their present path a memcpy.
        assert_eq!(Palette::Grey.pixel(ink::CARET), [1, 1, 1, 0xFF]);
    }

    #[test]
    fn masks_report_where_their_channel_starts() {
        assert_eq!(shift_of(0x00FF_0000), 16);
        assert_eq!(shift_of(0x0000_FF00), 8);
        assert_eq!(shift_of(0x0000_00FF), 0);
        assert_eq!(shift_of(0), 0);
    }

    #[test]
    fn paper_is_white_and_ink_is_black() {
        // The backing store holds luminance, not coverage: a cleared page is
        // 0xFF, and a rasterizer's coverage has to be inverted into it.
        assert_eq!(WHITE, 0xFF);
        assert_eq!(BLACK, 0x00);
    }

    #[test]
    fn a_burial_lasts_until_the_window_is_wholly_visible() {
        // What the timing in the log measures. A burial that is reported again
        // while it is still going has not restarted, or the duration would
        // read as the gap between the last two reports.
        let start = Instant::now();
        let mut burial = Burial::default();
        burial.saw(Visibility::FULLY_OBSCURED, start);
        let first = burial.since;
        burial.saw(
            Visibility::FULLY_OBSCURED,
            start + std::time::Duration::from_secs(5),
        );
        assert_eq!(burial.since, first);
        burial.saw(
            Visibility::UNOBSCURED,
            start + std::time::Duration::from_secs(9),
        );
        assert_eq!(burial.since, None);
    }

    #[test]
    fn a_window_in_front_is_not_buried() {
        // What gates the touchscreen: a false positive here makes the editor
        // deaf to every tap.
        let now = Instant::now();
        let mut burial = Burial::default();
        assert!(!burial.under);
        burial.saw(Visibility::PARTIALLY_OBSCURED, now);
        assert!(!burial.under);
        burial.saw(Visibility::FULLY_OBSCURED, now);
        assert!(burial.under);
        burial.saw(Visibility::UNOBSCURED, now);
        assert!(!burial.under);
    }

    #[test]
    fn a_field_is_light_enough_to_read_black_prose_on() {
        // Both grey fields sit nearer paper than ink, and the focused one is
        // the darker of the two so that focus mode reads as a step back.
        // Checked at compile time: these are the constants themselves.
        const { assert!(FIELD > QUIET && FIELD < WHITE) };
        const { assert!(FIELD_QUIET > FIELD && FIELD_QUIET < WHITE) };
    }
}
