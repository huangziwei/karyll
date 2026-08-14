//! Reading the pen.
//!
//! **As a pointer, and only as a pointer.** The pen places the cursor, drags a
//! selection and presses buttons — the same vocabulary a finger has, and the
//! same code path: this reports [`Touch`] and [`crate::Editor::tapped`] cannot
//! tell which device a tap came from. Handwriting is not wanted and is not here.
//!
//! A nib is a few tenths of a millimetre across against a fingertip's several,
//! so placing a caret between two characters is a thing the pen can do and a
//! finger cannot — and the pen is already in the hand of anyone using a Scribe.
//!
//! The digitizer is a plain single-touch device — `ABS_X`, `ABS_Y` and
//! `BTN_TOUCH` — rather than the multitouch protocol the finger panel speaks, so
//! it has its own small state machine. `SYN_REPORT` closes each packet.
//!
//! **Hover is ignored.** `BTN_TOOL_PEN` says the nib is *near* the glass, and the
//! digitizer streams position the whole time it is: several hundred packets for a
//! pen resting in a hand over the page. Only `BTN_TOUCH` — the nib actually down
//! — begins anything.
//!
//! Which node: `WacomDigitizer` at `/dev/input/event2`, in the panel's own
//! coordinate space. The `stylus-custom` node beside it is a virtual mirror the
//! framework maintains for X, already rotated; taking that one would apply the
//! framework's rotation on top of [`crate::orientation`]'s.

use std::fs::File;
use std::io::Read;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::evdev::{EVENT_BYTES, decode_raw};
use crate::touch::{Extent, Touch, read_extent};

const EV_SYN: u16 = 0x00;
const SYN_REPORT: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
/// The nib on the glass. `BTN_TOOL_PEN` (320) says only that it is nearby.
const BTN_TOUCH: u16 = 0x14a;

/// The firmware's own symlink, from `/etc/udev/rules.d/40-stylus.rules`. It
/// points at the raw digitizer rather than at the virtual mirror, which is the
/// one we want.
const PEN_ALIAS: &str = "/dev/input/stylus";

pub struct Pen {
    file: File,
    /// The digitizer's own axis ranges, which are not the panel's — an EMR grid
    /// counts far finer than the pixels over it. [`Extent::to_pixels`] is what
    /// makes that not matter.
    pub x_extent: Extent,
    pub y_extent: Extent,
    pending: Vec<u8>,
    x: i32,
    y: i32,
    /// Whether the nib is currently on the glass.
    down: bool,
    /// `BTN_TOUCH` changed in the packet being read. Reported at `SYN_REPORT`
    /// rather than as it arrives, so a press carries the position from its own
    /// packet instead of the previous stroke's.
    began: bool,
    ended: bool,
    logged: bool,
}

impl Pen {
    pub fn open() -> Result<Self> {
        let path = if Path::new(PEN_ALIAS).exists() {
            PathBuf::from(PEN_ALIAS)
        } else {
            find_by_scan()?
        };
        let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;

        // **Not grabbed**, for the reason the touchscreen is not: exclusive
        // input starves the framework of the power-off dialog, and no editor is
        // worth making the device unrecoverable.
        let x_extent = read_extent(&file, ABS_X, 1859);
        let y_extent = read_extent(&file, ABS_Y, 2479);
        // Logged because these are the numbers that say whether the digitizer
        // counts in the panel's orientation. A range wider than it is tall would
        // mean it does not.
        eprintln!(
            "pen: {} x={}..{} y={}..{}",
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
            x: 0,
            y: 0,
            down: false,
            began: false,
            ended: false,
            logged: false,
        })
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Read whatever is ready and return the contacts it completed. Empty for a
    /// pen that is merely hovering, which is most of what this device reports.
    pub fn read_batch(&mut self) -> Result<Vec<Touch>> {
        let mut buf = [0u8; EVENT_BYTES * 64];
        let n = self.file.read(&mut buf).context("read pen")?;
        if n == 0 {
            bail!("pen closed");
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
            (EV_ABS, ABS_X) => self.x = value,
            (EV_ABS, ABS_Y) => self.y = value,
            (EV_KEY, BTN_TOUCH) => {
                if value != 0 {
                    self.began = true;
                } else {
                    self.ended = true;
                }
            }
            (EV_SYN, SYN_REPORT) => {
                if self.began {
                    self.began = false;
                    self.down = true;
                    out.push(Touch::Down {
                        x: self.x,
                        y: self.y,
                    });
                    // One line per session, so the digitizer's coordinate space
                    // can be read off a log rather than guessed at.
                    if !self.logged {
                        self.logged = true;
                        eprintln!("pen: first contact at raw ({}, {})", self.x, self.y);
                    }
                }
                if self.ended {
                    self.ended = false;
                    // Guarded, so a session that started mid-stroke does not
                    // report a lift for a press nobody saw.
                    if std::mem::take(&mut self.down) {
                        out.push(Touch::Up {
                            x: self.x,
                            y: self.y,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

/// Fall back to naming the digitizer when the firmware alias is absent.
fn find_by_scan() -> Result<PathBuf> {
    let raw = std::fs::read_to_string("/proc/bus/input/devices")
        .context("read /proc/bus/input/devices")?;
    match pick_pen(&raw) {
        Some(node) => Ok(PathBuf::from(format!("/dev/input/{node}"))),
        None => bail!("no digitizer in /proc/bus/input/devices"),
    }
}

/// The digitizer's `eventN`, by name.
///
/// `WacomDigitizer` outranks the `stylus-custom` mirror beside it, which
/// reports the same axes under the framework's rotation. Both match "stylus" in
/// spirit, so the maker's name is tried first and the generic word last.
fn pick_pen(raw: &str) -> Option<String> {
    const NAMES: [&str; 3] = ["wacom", "digitizer", "stylus"];
    let mut found: Vec<(usize, String)> = Vec::new();
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
        if let (Some(rank), Some(handler)) = (
            NAMES.iter().position(|candidate| name.contains(candidate)),
            handler,
        ) {
            found.push((rank, handler.to_string()));
        }
    }
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().next().map(|(_, handler)| handler)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim from the device's `/proc/bus/input/devices`, so the choice is
    /// made against what is actually there.
    const CAPTURE: &str = "\
I: Bus=0000 Vendor=0000 Product=0000 Version=0000
N: Name=\"pt_mt\"
H: Handlers=event3 perfmgr

I: Bus=0018 Vendor=2d1f Product=0158 Version=1827
N: Name=\"stylus-custom\"
H: Handlers=event4 perfmgr

I: Bus=0018 Vendor=2d1f Product=0158 Version=1827
N: Name=\"WacomDigitizer\"
H: Handlers=event2 perfmgr
";

    #[test]
    fn the_raw_digitizer_is_picked_over_the_frameworks_mirror() {
        // The mirror comes first in the dump and matches "stylus"; taking it
        // would apply the framework's rotation on top of our own.
        assert_eq!(pick_pen(CAPTURE).as_deref(), Some("event2"));
    }

    #[test]
    fn the_mirror_will_do_when_there_is_nothing_else() {
        let only_mirror = "I: Bus=0018\nN: Name=\"stylus-custom\"\nH: Handlers=event4 perfmgr\n";
        assert_eq!(pick_pen(only_mirror).as_deref(), Some("event4"));
    }

    #[test]
    fn a_device_with_no_pen_is_not_a_panic() {
        assert_eq!(pick_pen("I: Bus=0000\nN: Name=\"pt_mt\"\n"), None);
    }

    /// A `Pen` with no device behind it, for the state machine alone.
    fn detached() -> Pen {
        Pen {
            file: File::open("/dev/null").expect("/dev/null"),
            x_extent: Extent { min: 0, max: 1859 },
            y_extent: Extent { min: 0, max: 2479 },
            pending: Vec::new(),
            x: 0,
            y: 0,
            down: false,
            began: false,
            ended: false,
            logged: true,
        }
    }

    /// The nib arriving, the way the kernel reports it: the button first, then
    /// the coordinates, then `SYN_REPORT`.
    fn touch_down(p: &mut Pen, x: i32, y: i32) -> Vec<Touch> {
        let mut out = Vec::new();
        p.feed(EV_KEY, BTN_TOUCH, 1, &mut out);
        p.feed(EV_ABS, ABS_X, x, &mut out);
        p.feed(EV_ABS, ABS_Y, y, &mut out);
        p.feed(EV_SYN, SYN_REPORT, 0, &mut out);
        out
    }

    fn lift(p: &mut Pen) -> Vec<Touch> {
        let mut out = Vec::new();
        p.feed(EV_KEY, BTN_TOUCH, 0, &mut out);
        p.feed(EV_SYN, SYN_REPORT, 0, &mut out);
        out
    }

    /// Position moving with nothing pressed: the pen in the air over the page.
    fn hover(p: &mut Pen, x: i32, y: i32) -> Vec<Touch> {
        let mut out = Vec::new();
        p.feed(EV_ABS, ABS_X, x, &mut out);
        p.feed(EV_ABS, ABS_Y, y, &mut out);
        p.feed(EV_SYN, SYN_REPORT, 0, &mut out);
        out
    }

    #[test]
    fn a_press_lands_where_the_nib_actually_is() {
        let mut p = detached();
        assert_eq!(
            touch_down(&mut p, 900, 1200),
            vec![Touch::Down { x: 900, y: 1200 }]
        );
        assert_eq!(lift(&mut p), vec![Touch::Up { x: 900, y: 1200 }]);
    }

    /// A pen held over the page reports continuously, and every one of those
    /// packets would otherwise be a tap.
    #[test]
    fn hovering_does_nothing_at_all() {
        let mut p = detached();
        assert!(hover(&mut p, 100, 100).is_empty());
        assert!(hover(&mut p, 400, 700).is_empty());
        // And the position it left behind is not the position of the press that
        // eventually comes.
        assert_eq!(
            touch_down(&mut p, 900, 1200),
            vec![Touch::Down { x: 900, y: 1200 }]
        );
    }

    /// Press at one place and lift at another is a drag, which is how a
    /// selection is made — the same gesture the finger panel reports.
    #[test]
    fn a_stroke_lifts_where_it_ended() {
        let mut p = detached();
        touch_down(&mut p, 100, 100);
        let mut out = Vec::new();
        p.feed(EV_ABS, ABS_X, 800, &mut out);
        p.feed(EV_ABS, ABS_Y, 950, &mut out);
        p.feed(EV_SYN, SYN_REPORT, 0, &mut out);
        assert!(out.is_empty(), "a stroke in progress completes nothing");
        assert_eq!(lift(&mut p), vec![Touch::Up { x: 800, y: 950 }]);
    }

    /// Opening the node mid-stroke means the first thing seen is a lift.
    #[test]
    fn a_lift_with_no_press_behind_it_is_ignored() {
        let mut p = detached();
        assert!(lift(&mut p).is_empty());
    }
}
