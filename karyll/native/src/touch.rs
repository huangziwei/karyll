//! Reading the touchscreen.
//!
//! Without this karyll is unusable before a keyboard exists — and unescapable,
//! since the only way out was a key chord. Touch is what makes the app reachable
//! on a device that has never had a keyboard attached.
//!
//! Protocol B multitouch. A contact begins with `ABS_MT_TRACKING_ID >= 0` and
//! ends with `-1`; **the positions follow it in the same packet**, and
//! `SYN_REPORT` closes each
//! packet. Only the first contact is tracked, because nothing here needs a
//! gesture — a tap and a long press are the whole vocabulary.
//!
//! **Orientation is deliberately not relied upon.** The compositor rotates our
//! window to the framework's orientation while touch is panel-fixed, and the
//! exact transform has not been confirmed on this device. So the long press,
//! which is the way into the menu, ignores position entirely, and menu rows span
//! the full width so only one axis can be wrong. Raw coordinates are logged on
//! the first contact of a session, which is what will settle it.

use std::fs::File;
use std::io::Read;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::evdev::{EVENT_BYTES, decode_raw};

/// `EVIOCGABS(axis)` = `_IOR('E', 0x40 + axis, struct input_absinfo)`, and
/// `input_absinfo` is six `__s32`. Asking the panel for its own range is what
/// removes the guesswork about whether raw units are screen pixels.
const fn eviocgabs(axis: u16) -> libc::c_int {
    (2 << 30) | (24 << 16) | (0x45 << 8) | (0x40 + axis as libc::c_int)
}

/// The panel's coordinate range, so raw positions can be mapped onto the window.
#[derive(Debug, Clone, Copy)]
pub struct Extent {
    pub min: i32,
    pub max: i32,
}

impl Extent {
    /// Map a raw coordinate onto `0..span`.
    pub fn to_pixels(self, value: i32, span: u16) -> i32 {
        let range = (self.max - self.min).max(1);
        ((value - self.min) as i64 * span as i64 / range as i64) as i32
    }
}

const EV_SYN: u16 = 0x00;
const SYN_REPORT: u16 = 0x00;
const EV_ABS: u16 = 0x03;
const ABS_MT_SLOT: u16 = 0x2F;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;

/// Newer firmware — the Scribe at 5.19.4.0.1 included — ships this symlink next
/// to the `eventN` nodes. When it exists it outranks anything we could infer,
/// because it is the device saying which node is the finger panel.
const TOUCH_ALIAS: &str = "/dev/input/touch";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Touch {
    /// A finger landed. Reported immediately so the target can be inverted
    /// while the finger is still down — without it, a tap that registered looks
    /// exactly like one that missed until the screen catches up.
    Down { x: i32, y: i32 },
    /// The finger lifted. This is what resolves an action, at the position it
    /// lifted from.
    Up { x: i32, y: i32 },
}

pub struct Touchscreen {
    file: File,
    /// The panel's own axis ranges. Read from the device rather than assumed
    /// equal to the screen, which is the difference between taps landing where
    /// they were aimed and landing nowhere near.
    pub x_extent: Extent,
    pub y_extent: Extent,
    pending: Vec<u8>,
    slot: usize,
    /// Where the current contact started, and when.
    origin: Option<(i32, i32, Instant)>,
    x: i32,
    y: i32,
    /// Set once the current contact has already been reported as a long press,
    /// so holding does not repeat it.
    fired: bool,
    /// A contact began or ended in the packet being read. Both are reported at
    /// `SYN_REPORT` rather than as they arrive, because the tracking id comes
    /// *before* the coordinates — reporting on it carries the position of the
    /// previous touch, which is exactly one behind.
    began: bool,
    ended: bool,
    logged: bool,
}

impl Touchscreen {
    pub fn open() -> Result<Self> {
        let path = if Path::new(TOUCH_ALIAS).exists() {
            PathBuf::from(TOUCH_ALIAS)
        } else {
            find_by_scan()?
        };
        let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;

        // **The touchscreen is deliberately NOT grabbed.**
        //
        // EVIOCGRAB would give us taps exclusively, and it also starves the
        // framework of them — including the power-off dialog. A user who long
        // presses power then cannot touch "Restart", and the only way out is
        // holding the button for thirty seconds. No editor is worth making the
        // device unrecoverable, so we share the panel and accept that the
        // framework sees the same taps we do.
        //
        // It is also why karyll takes no screenshots of its own: the firmware's
        // opposite-corners gesture reads the framebuffer directly, so it
        // captures the editor's own drawing and writes it to
        // `/mnt/us/screenshots` regardless of which process painted.
        let x_extent = read_extent(&file, ABS_MT_POSITION_X, 1859);
        let y_extent = read_extent(&file, ABS_MT_POSITION_Y, 2479);
        eprintln!(
            "touch: {} x={}..{} y={}..{}",
            path.display(),
            x_extent.min,
            x_extent.max,
            y_extent.min,
            y_extent.max
        );
        Ok(Self {
            file,
            x_extent,
            y_extent,
            pending: Vec::with_capacity(EVENT_BYTES * 32),
            slot: 0,
            origin: None,
            x: 0,
            y: 0,
            fired: false,
            began: false,
            ended: false,
            logged: false,
        })
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Read whatever is ready and return the gestures it completed.
    pub fn read_batch(&mut self) -> Result<Vec<Touch>> {
        let mut buf = [0u8; EVENT_BYTES * 64];
        let n = self.file.read(&mut buf).context("read touchscreen")?;
        if n == 0 {
            bail!("touchscreen closed");
        }
        self.pending.extend_from_slice(&buf[..n]);

        let mut out = Vec::new();
        let whole = self.pending.len() / EVENT_BYTES;
        for i in 0..whole {
            let at = i * EVENT_BYTES;
            if let Some((kind, code, value)) = decode_raw(&self.pending[at..at + EVENT_BYTES]) {
                self.feed(kind, code, value, &mut out);
            }
        }
        self.pending.drain(..whole * EVENT_BYTES);
        Ok(out)
    }

    /// Feed one input event through the contact state machine, appending
    /// whatever it completes.
    fn feed(&mut self, kind: u16, code: u16, value: i32, out: &mut Vec<Touch>) {
        match (kind, code) {
            (EV_ABS, ABS_MT_SLOT) => self.slot = value.max(0) as usize,
            (EV_ABS, ABS_MT_TRACKING_ID) if self.slot == 0 => {
                if value >= 0 {
                    self.began = true;
                } else {
                    self.ended = true;
                }
            }
            (EV_ABS, ABS_MT_POSITION_X) if self.slot == 0 => self.x = value,
            (EV_ABS, ABS_MT_POSITION_Y) if self.slot == 0 => self.y = value,
            (EV_SYN, SYN_REPORT) => {
                if self.began {
                    self.began = false;
                    self.origin = Some((self.x, self.y, Instant::now()));
                    self.fired = false;
                    out.push(Touch::Down {
                        x: self.x,
                        y: self.y,
                    });
                }
                if self.ended {
                    self.ended = false;
                    if self.origin.take().is_some() && !self.fired {
                        out.push(Touch::Up {
                            x: self.x,
                            y: self.y,
                        });
                    }
                }
                // One line per session, so the panel's coordinate space can be
                // read off a log rather than guessed at.
                if !self.logged && self.origin.is_some() {
                    self.logged = true;
                    eprintln!("touch: first contact at raw ({}, {})", self.x, self.y);
                }
            }
            _ => {}
        }
    }
}

/// Ask a device for an axis range, falling back to `fallback` when the ioctl is
/// refused — a wrong range still beats no touch at all.
///
/// Shared with [`crate::pen`], which asks the same question of a different
/// device about different axes. The fallback is the caller's because only the
/// caller knows which axis it is asking about.
pub fn read_extent(file: &File, axis: u16, fallback: i32) -> Extent {
    let mut info = [0i32; 6];
    let ok = unsafe { libc::ioctl(file.as_raw_fd(), eviocgabs(axis) as _, info.as_mut_ptr()) } == 0;
    if !ok || info[2] <= info[1] {
        return Extent {
            min: 0,
            max: fallback,
        };
    }
    Extent {
        min: info[1],
        max: info[2],
    }
}

/// Fall back to naming the panel when the firmware alias is absent.
fn find_by_scan() -> Result<PathBuf> {
    let raw = std::fs::read_to_string("/proc/bus/input/devices")
        .context("read /proc/bus/input/devices")?;
    match pick_touch(&raw) {
        Some(node) => Ok(PathBuf::from(format!("/dev/input/{node}"))),
        None => bail!("no touchscreen in /proc/bus/input/devices"),
    }
}

/// The finger panel's `eventN`, by name.
///
/// `pt_mt` is the Scribe's, sitting next to a Wacom pen node that must not be
/// picked instead — the pen reports the same axes but is not what a finger
/// taps with.
fn pick_touch(raw: &str) -> Option<String> {
    const PANELS: [&str; 6] = ["pt_mt", "cyttsp", "zforce", "atmel", "focaltech", "goodix"];
    let mut name = String::new();
    for block in raw.split("\n\n") {
        let mut handler = None;
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("N: Name=") {
                name = rest.trim_matches('"').to_ascii_lowercase();
            } else if let Some(rest) = line.strip_prefix("H: Handlers=") {
                handler = rest.split_whitespace().find(|h| h.starts_with("event"));
            }
        }
        if PANELS.iter().any(|p| name.contains(p))
            && let Some(handler) = handler
        {
            return Some(handler.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPTURE: &str = "\
I: Bus=0000 Vendor=0000 Product=0000 Version=0000
N: Name=\"WacomDigitizer\"
H: Handlers=event2 perfmgr
B: KEY=1c03 0 0 0 0 0 0 0 0 0 0

I: Bus=0000 Vendor=0000 Product=0000 Version=0000
N: Name=\"pt_mt\"
H: Handlers=event3 perfmgr
B: KEY=0

I: Bus=0000 Vendor=0000 Product=0000 Version=0000
N: Name=\"stylus-custom\"
H: Handlers=event4 perfmgr
B: KEY=1c03 0 0 0 0 0 0 0 0 0 0
";

    #[test]
    fn the_finger_panel_is_picked_over_the_pen() {
        // The Wacom node comes first and reports the same axes; taking it would
        // mean taps never arriving.
        assert_eq!(pick_touch(CAPTURE).as_deref(), Some("event3"));
    }

    #[test]
    fn no_panel_is_not_a_panic() {
        assert_eq!(pick_touch("I: Bus=0000\nN: Name=\"pwrkey\"\n"), None);
    }

    /// Drive the state machine the way the kernel actually does: the tracking
    /// id comes **first**, then the coordinates, then `SYN_REPORT`.
    ///
    /// The earlier version of this helper sent the position first, which is why
    /// it never caught the press landing at the previous touch's coordinates.
    fn contact(t: &mut Touchscreen, x: i32, y: i32) -> Vec<Touch> {
        let mut out = Vec::new();
        t.feed(EV_ABS, ABS_MT_SLOT, 0, &mut out);
        t.feed(EV_ABS, ABS_MT_TRACKING_ID, 1, &mut out);
        t.feed(EV_ABS, ABS_MT_POSITION_X, x, &mut out);
        t.feed(EV_ABS, ABS_MT_POSITION_Y, y, &mut out);
        t.feed(EV_SYN, SYN_REPORT, 0, &mut out);
        out
    }

    fn release(t: &mut Touchscreen) -> Vec<Touch> {
        let mut out = Vec::new();
        t.feed(EV_ABS, ABS_MT_TRACKING_ID, -1, &mut out);
        t.feed(EV_SYN, SYN_REPORT, 0, &mut out);
        out
    }

    fn syn(t: &mut Touchscreen) -> Vec<Touch> {
        let mut out = Vec::new();
        t.feed(EV_SYN, SYN_REPORT, 0, &mut out);
        out
    }

    /// A `Touchscreen` with no device behind it, for the state machine alone.
    fn detached() -> Touchscreen {
        Touchscreen {
            file: File::open("/dev/null").expect("/dev/null"),
            x_extent: Extent { min: 0, max: 1859 },
            y_extent: Extent { min: 0, max: 2479 },
            pending: Vec::new(),
            slot: 0,
            origin: None,
            x: 0,
            y: 0,
            fired: false,
            began: false,
            ended: false,
            logged: true,
        }
    }

    #[test]
    fn a_press_lands_where_the_finger_actually_is() {
        // The bug this pins: the tracking id arrives before the coordinates, so
        // reporting the press on the id carried the *previous* touch's position
        // — one behind every time, and (0, 0) on the very first press, which is
        // why nothing under the first tap ever lit up.
        let mut t = detached();
        assert_eq!(
            contact(&mut t, 900, 1200),
            vec![Touch::Down { x: 900, y: 1200 }]
        );
        assert_eq!(release(&mut t), vec![Touch::Up { x: 900, y: 1200 }]);

        // And a second, elsewhere, is not reported at the first one's position.
        assert_eq!(
            contact(&mut t, 300, 400),
            vec![Touch::Down { x: 300, y: 400 }]
        );
        assert_eq!(release(&mut t), vec![Touch::Up { x: 300, y: 400 }]);
    }

    #[test]
    fn a_drag_lifts_where_the_finger_left() {
        // sidle resolves on the lift position, so dragging off a key and
        // letting go acts on wherever it ended up rather than where it began.
        let mut t = detached();
        contact(&mut t, 100, 100);
        let mut out = Vec::new();
        t.feed(EV_ABS, ABS_MT_POSITION_X, 800, &mut out);
        assert_eq!(release(&mut t), vec![Touch::Up { x: 800, y: 100 }]);
    }

    /// Put a second contact down at a panel position.
    fn second(t: &mut Touchscreen, x: i32, y: i32) {
        let mut out = Vec::new();
        t.feed(EV_ABS, ABS_MT_SLOT, 1, &mut out);
        t.feed(EV_ABS, ABS_MT_TRACKING_ID, 2, &mut out);
        t.feed(EV_ABS, ABS_MT_POSITION_X, x, &mut out);
        t.feed(EV_ABS, ABS_MT_POSITION_Y, y, &mut out);
        t.feed(EV_ABS, ABS_MT_SLOT, 0, &mut out);
    }

    #[test]
    fn a_second_finger_reports_nothing_of_its_own() {
        // Including in opposite corners, which is the firmware's screenshot
        // gesture: the framework serves it off the same panel, and a second
        // capture from here would only duplicate the file it writes.
        let mut t = detached();
        contact(&mut t, 20, 20);
        second(&mut t, 1840, 2460);
        assert!(syn(&mut t).is_empty());
        assert!(syn(&mut t).is_empty());
    }

    #[test]
    fn a_second_finger_is_ignored() {
        let mut t = detached();
        contact(&mut t, 300, 300);
        // Slot 1 must not move the tracked contact.
        let mut out = Vec::new();
        t.feed(EV_ABS, ABS_MT_SLOT, 1, &mut out);
        t.feed(EV_ABS, ABS_MT_POSITION_X, 1500, &mut out);
        t.feed(EV_ABS, ABS_MT_TRACKING_ID, -1, &mut out);
        t.feed(EV_ABS, ABS_MT_SLOT, 0, &mut out);
        assert_eq!(release(&mut t), vec![Touch::Up { x: 300, y: 300 }]);
    }
}
