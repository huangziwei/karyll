//! The display surface: a real WM-managed X11 window.
//!
//! Not raw `/dev/fb0`. A window means the lab126 compositor owns the surface —
//! it shows us fullscreen and recomposites the whole screen when we are torn
//! down, so exiting leaves a live home screen instead of a stuck frame. This is
//! the model kterm uses and the one `sidle/native` proved on this hardware.
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
//! The backing store is one byte per pixel. This panel is grayscale, so unlike
//! the colour-capable devices there is no reason to carry RGB and collapse it
//! on the wire — 1860×2480 costs 4.6 MB here instead of 13.8 MB, against
//! ~514 MB shared with the framework.

use std::os::unix::io::{AsRawFd, RawFd};

use anyhow::{Context, Result};
use x11rb::connection::Connection;

use crate::orientation::Orientation;
// `maximum_request_bytes` is BIG-REQUESTS-aware and lives on this trait.
use x11rb::connection::RequestConnection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt, CreateGCAux, CreateWindowAux, EventMask, Gcontext, ImageFormat,
    PropMode, Window as XWindow, WindowClass,
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
/// It replaced a 50% ordered dither. Dither averages over an *area*, and a stem
/// at this size is 2–4 px — about one pattern cell — so instead of lightening
/// the mark it deleted half of it. Han would have been worse.
pub const QUIET: u8 = 0x88;

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
    width: u16,
    height: u16,
    /// One byte per pixel, row-major, `WHITE` for paper.
    pixels: Vec<u8>,
    app_id: String,
    orientation: Orientation,
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
                .event_mask(EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY),
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
            width,
            height,
            pixels,
            app_id: app_id.to_string(),
            orientation,
        })
    }

    pub fn orientation(&self) -> Orientation {
        self.orientation
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
        // button next to it worked every time. `sidle/native` widens its
        // updates to full rows for the same reason. Rows are cheap — the cost
        // is in how many of them, not how wide.
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
            let data = encode_band(&self.pixels, self.width as usize, band, bpp);
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
        while let Some(event) = self.conn.poll_for_event().context("poll_for_event")? {
            match event {
                Event::Expose(_) => expose = true,
                Event::ConfigureNotify(event) => size = Some((event.width, event.height)),
                Event::UnmapNotify(_) | Event::DestroyNotify(_) => return Ok(Surface::Gone),
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
fn encode_band(pixels: &[u8], stride: usize, band: Rect, bpp: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(band.width as usize * band.height as usize * bpp);
    for y in band.y as usize..(band.y + band.height) as usize {
        let row = &pixels[y * stride + band.x as usize..][..band.width as usize];
        if bpp == 1 {
            out.extend_from_slice(row);
        } else {
            // The pad byte is ignored by the server at depth 24 and is the
            // alpha slot at depth 32.
            for &v in row {
                out.extend_from_slice(&[v, v, v, 0xFF]);
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
        assert_eq!(encode_band(&pixels, 4, band, 1), vec![5, 6, 9, 10]);
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
            encode_band(&pixels, 2, band, 4),
            vec![0x40, 0x40, 0x40, 0xFF, 0x80, 0x80, 0x80, 0xFF]
        );
    }

    #[test]
    fn paper_is_white_and_ink_is_black() {
        // The backing store holds luminance, not coverage: a cleared page is
        // 0xFF, and a rasterizer's coverage has to be inverted into it.
        assert_eq!(WHITE, 0xFF);
        assert_eq!(BLACK, 0x00);
    }
}
