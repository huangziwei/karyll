//! Reading input devices straight from `/dev/input/eventN` — the keyboard, and
//! the accelerometer.
//!
//! Not through X. The editor's window never takes focus, and raw key codes are
//! what the CJK engine wants, so going through X would mean translating twice.
//! It also means the editor works without the udev rule that X's evdev backend
//! needs before it will bind a new keyboard on this device.
//!
//! Nodes are found by reading `/proc/bus/input/devices` rather than by probing
//! capability bits with an ioctl, because the parse can be tested against real
//! captures from the device — and the captures below are real.
//!
//! Both devices share this file rather than having one each, because they share
//! the wire format exactly. A second copy of the `struct input_event` layout is
//! the kind of duplication that has already cost this project twice.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::keymap::code;

/// `_IOW('E', 0x90, int)`: direction(W=1)<<30 | size(4)<<16 | type('E')<<8 | nr.
const EVIOCGRAB: libc::c_int = 0x4004_4590;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;

/// `EV_*` as bits of the `B: EV=` bitmap, which is what
/// `/proc/bus/input/devices` prints.
const EV_KEY_BIT: u32 = 1 << EV_KEY;
const EV_ABS_BIT: u32 = 1 << EV_ABS;

/// The three axes, and the fourth thing this driver reports.
///
/// `ABS=1000007` on the Scribe's `kx132-accel` sets bits 0, 1, 2 and **24**.
/// The first three are `ABS_X`, `ABS_Y`, `ABS_Z`. Code 24 is `ABS_PRESSURE` in
/// the kernel's own headers, which an accelerometer plainly does not have — the
/// driver has repurposed a spare code, and given that it logs
/// `KX132_1211_TSCP` and `Orientation state is face up` around the same events,
/// it is very likely the tilt position. That is a guess until a device run
/// says otherwise, which is why it is carried through with a neutral name.
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_TILT: u16 = 0x18;

/// Multitouch protocol B's per-contact identifier, which only a device that
/// tracks several contacts has any use for.
const ABS_MT_TRACKING_ID: u16 = 0x39;

/// `struct timeval` is two C longs, so its width follows the target. The device
/// is 32-bit, which makes `struct input_event` 16 bytes there.
const TIME_BYTES: usize = 2 * size_of::<libc::c_long>();
pub const EVENT_BYTES: usize = TIME_BYTES + 2 + 2 + 4;

/// A key going down, coming up, or repeating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: u16,
    pub pressed: bool,
    /// Held long enough for the kernel to repeat it.
    pub repeat: bool,
}

/// Decode one `struct input_event` into `(type, code, value)`.
///
/// The one decoder every input device uses — the keyboard, the accelerometer,
/// the touchscreen and the pen — so the struct layout is written down once.
pub fn decode_raw(buf: &[u8]) -> Option<(u16, u16, i32)> {
    if buf.len() < EVENT_BYTES {
        return None;
    }
    let kind = u16::from_ne_bytes([buf[TIME_BYTES], buf[TIME_BYTES + 1]]);
    let code = u16::from_ne_bytes([buf[TIME_BYTES + 2], buf[TIME_BYTES + 3]]);
    let value = i32::from_ne_bytes([
        buf[TIME_BYTES + 4],
        buf[TIME_BYTES + 5],
        buf[TIME_BYTES + 6],
        buf[TIME_BYTES + 7],
    ]);
    Some((kind, code, value))
}

/// Decode one `struct input_event`. `None` for anything that is not a key.
fn decode(buf: &[u8]) -> Option<KeyEvent> {
    let (kind, code, value) = decode_raw(buf)?;
    if kind != EV_KEY {
        return None;
    }
    Some(KeyEvent {
        code,
        pressed: value != 0,
        repeat: value == 2,
    })
}

/// Whether a `B:` bitmap advertises a given code.
///
/// The words are printed most significant first, so word 0 — the one holding
/// codes 0..31 — is last, and the count runs from the right. A code the bitmap
/// is too short to describe is absent rather than an error: that is how a
/// single-word `ABS=f000003` answers a question about `ABS_MT_TRACKING_ID`, and
/// answering `false` is exactly right.
///
/// Words are `BITS_PER_LONG` wide, so 32 here — every Kindle this runs on is
/// `armv7l`. On a 64-bit kernel the same dump would pack two of these words
/// into one and a code above 31 would be read from the wrong place.
fn advertises(bitmap: &str, bit: u16) -> bool {
    let words: Vec<&str> = bitmap.split_whitespace().collect();
    let (word, shift) = (bit as usize / 32, bit % 32);
    words
        .len()
        .checked_sub(1 + word)
        .and_then(|at| u32::from_str_radix(words[at], 16).ok())
        .is_some_and(|w| w & (1 << shift) != 0)
}

/// One device block from `/proc/bus/input/devices`.
#[derive(Default, Debug)]
struct Block {
    handler: Option<String>,
    /// The `B: EV=` bitmap, which says which event types the device reports.
    ev: u32,
    /// The `B: KEY=` bitmap, verbatim — its word width is not worth guessing.
    keys: String,
    /// The `B: ABS=` bitmap, which is what separates a finger panel from a pen
    /// and a real accelerometer from the other sensors on the bus.
    abs: String,
}

/// Split a `/proc/bus/input/devices` dump into its device blocks.
///
/// One parse, two selectors on top of it. A blank line ends a block, and the
/// dump ends with one, but an extra is appended so a trailing block without it
/// is still emitted.
fn blocks(raw: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut current = Block::default();
    for line in raw.lines().chain(std::iter::once("")) {
        if let Some(rest) = line.strip_prefix("H: Handlers=") {
            current.handler = rest
                .split_whitespace()
                .find(|h| h.starts_with("event"))
                .map(String::from);
        } else if let Some(rest) = line.strip_prefix("B: KEY=") {
            current.keys = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("B: ABS=") {
            current.abs = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("B: EV=") {
            current.ev = u32::from_str_radix(rest.trim(), 16).unwrap_or(0);
        } else if line.is_empty() {
            if current.handler.is_some() {
                out.push(std::mem::take(&mut current));
            } else {
                current = Block::default();
            }
        }
    }
    out
}

/// Choose the keyboard, returning its `eventN` handler.
///
/// A device counts as a keyboard when it advertises **Q**. That rule was
/// validated against this device's real bitmaps: it accepts a keyboard and
/// rejects the WacomDigitizer, the stylus and the power key, which all carry
/// `EV_KEY` for a handful of buttons.
fn pick_keyboard(raw: &str) -> Option<String> {
    blocks(raw)
        .into_iter()
        .find(|b| advertises(&b.keys, code::Q))
        .and_then(|b| b.handler)
}

/// Choose the touchscreen, returning its `eventN` handler.
///
/// **The finger panel is the one that tracks more than one finger.** Every
/// Kindle's panel speaks multitouch protocol B and so advertises
/// `ABS_MT_TRACKING_ID`; no pen node does, the Scribe's digitizer and its
/// `stylus-custom` mirror both being single-touch `ABS=f000003`. One bit
/// separates them on every device seen:
///
/// | device | `ABS=` | |
/// |---|---|---|
/// | `pt_mt` | `ee18000 0` | **picked** |
/// | `fts_ts` | `2618000 0` | **picked** |
/// | `cyttsp5_mt` | `6608000 0` | **picked** |
/// | `WacomDigitizer`, `stylus-custom` | `f000003` | rejected |
///
/// This replaced a list of panel names — `pt_mt`, `cyttsp`, `zforce`, `atmel`,
/// `focaltech`, `goodix` — which is the same mistake the accelerometer rule
/// below already avoids: the controller varies across Kindles and the shape of
/// the device does not. The list had no entry for `fts_ts`, so touch did not
/// come up at all on the panel that ships it.
pub fn pick_touchscreen(raw: &str) -> Option<String> {
    blocks(raw)
        .into_iter()
        .find(|b| advertises(&b.abs, ABS_MT_TRACKING_ID))
        .and_then(|b| b.handler)
}

/// Choose the accelerometer, returning its `eventN` handler.
///
/// The rule is **reports three spatial axes and no keys**, which is what
/// separates an accelerometer from a pointer and from the other sensors sharing
/// the bus:
///
/// | device | `EV=` | `ABS=` | has KEY | |
/// |---|---|---|---|---|
/// | `bd71828-pwrkey` | `3` | — | yes | rejected |
/// | `kx132-accel` | `9` | `1000007` | no | **picked** |
/// | `bma2x2` | `9` | `100 7` | no | **picked** |
/// | `max44009_als` | `9` | `100 0` | no | rejected |
/// | `bma_interrupt` | `d` | `3000000` | no | rejected |
/// | `WacomDigitizer`, `pt_mt`, `stylus-custom` | `b`/`f` | | yes | rejected |
///
/// Axes were added to the rule because "absolute axes and no keys" is also an
/// ambient light sensor, and a device that has one lists it first: on that
/// Kindle the editor opened `max44009_als` and read brightness as orientation.
/// `bma_interrupt` is the same accelerometer as `bma2x2` reported through the
/// interrupt path X uses, and reports neither X nor Y.
///
/// Matching on the name would have been easier and wrong: the udev rules ship
/// `60-kx132.rules`, `60-bma2x2.rules` and `60-bma4xy.rules`, so the part
/// varies across Kindles even though the shape of the device does not.
fn pick_accelerometer(raw: &str) -> Option<String> {
    blocks(raw)
        .into_iter()
        .find(|b| {
            b.ev & EV_ABS_BIT != 0
                && b.ev & EV_KEY_BIT == 0
                && [ABS_X, ABS_Y, ABS_Z]
                    .iter()
                    .all(|&axis| advertises(&b.abs, axis))
        })
        .and_then(|b| b.handler)
}

pub struct Keyboard {
    file: File,
    path: PathBuf,
    grabbed: bool,
    /// Bytes of a partially read event, kept across reads because a read can
    /// return a fraction of one.
    pending: Vec<u8>,
}

impl Keyboard {
    /// Find and open the keyboard.
    pub fn open() -> Result<Self> {
        let raw = std::fs::read_to_string("/proc/bus/input/devices")
            .context("read /proc/bus/input/devices")?;
        let Some(node) = pick_keyboard(&raw) else {
            bail!("no keyboard in /proc/bus/input/devices — is one paired?");
        };
        Self::open_path(PathBuf::from(format!("/dev/input/{node}")))
    }

    pub fn open_path(path: PathBuf) -> Result<Self> {
        let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;

        // The kernel reads the argument as "non-zero grabs, zero releases".
        // A failed grab is not fatal — we still read the device — but the
        // framework goes on seeing the same keys, so say so.
        let grabbed = unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGRAB as _, 1) } == 0;
        if !grabbed {
            eprintln!(
                "keyboard: EVIOCGRAB failed on {} — the framework still sees these keys",
                path.display()
            );
        }
        Ok(Self {
            file,
            path,
            grabbed,
            pending: Vec::with_capacity(EVENT_BYTES * 8),
        })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Whether udev has tagged this node `ID_INPUT_KEYBOARD`.
    ///
    /// karyll does not need the tag: it reads the node directly. X's
    /// `evdev_drv.so` binds nothing without it, and this device's udev applies
    /// no such tag on its own — so the tag is the difference between a keyboard
    /// that works everywhere and one that works only in the editor. A session
    /// log silent on it cannot tell those two apart.
    ///
    /// Read from udev's own database at `/run/udev/data/c<major>:<minor>`, one
    /// `E:` line per property. `None` when udev has no record of the node.
    pub fn tagged_for_x(&self) -> Option<bool> {
        let rdev = self.file.metadata().ok()?.rdev();
        // The glibc split: both halves are non-contiguous in the encoded value.
        let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff_u64);
        let minor = (rdev & 0xff) | ((rdev >> 12) & !0xff_u64);
        let data = std::fs::read_to_string(format!("/run/udev/data/c{major}:{minor}")).ok()?;
        Some(data.lines().any(|line| line == "E:ID_INPUT_KEYBOARD=1"))
    }

    /// The node's descriptor, so a caller can wait on it alongside the X
    /// connection instead of choosing one to block on.
    pub fn fd(&self) -> std::os::unix::io::RawFd {
        self.file.as_raw_fd()
    }

    /// Block until at least one key event arrives, then return everything that
    /// was ready.
    ///
    /// Returning a batch matters on eink: a burst of keystrokes should cost one
    /// repaint, not one per key.
    pub fn read_batch(&mut self) -> Result<Vec<KeyEvent>> {
        let mut buf = [0u8; EVENT_BYTES * 16];
        let n = self.file.read(&mut buf).context("read keyboard")?;
        if n == 0 {
            bail!("keyboard {} closed", self.path.display());
        }
        self.pending.extend_from_slice(&buf[..n]);

        let mut out = Vec::new();
        let whole = self.pending.len() / EVENT_BYTES;
        for i in 0..whole {
            if let Some(event) = decode(&self.pending[i * EVENT_BYTES..]) {
                out.push(event);
            }
        }
        self.pending.drain(..whole * EVENT_BYTES);
        Ok(out)
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        if self.grabbed {
            unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB as _, 0) };
        }
    }
}

/// One complete accelerometer report, latched at `EV_SYN`.
///
/// Events arrive one axis at a time and only mean something together, so a
/// caller must never act on a partial set — the same mistake that made the
/// touchscreen report every press one position behind.
///
/// **`x`, `y` and `z` are always zero on this firmware.** The driver advertises
/// all three and has never once sent a value for any of them, over two device
/// runs; [`tilt`](Self::tilt) is the whole signal. They are still collected
/// because collecting them is free and their being zero is a fact worth being
/// able to re-check, but nothing may depend on them. A first pass derived the
/// orientation from the gravity vector they were expected to carry, and all of
/// that work went in the bin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sample {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    /// The position code, on ABS code 24. See [`ABS_TILT`], and
    /// `orientation::Orientation::from_tilt` for what the values mean.
    pub tilt: i32,
}

/// The accelerometer.
///
/// **Never grabbed.** The framework reads this device too — it is how the stock
/// reader flips 180° — and taking input away from the framework is what cost a
/// 30-second hard reset when the touchscreen was grabbed. evdev is happy with
/// several readers; open it and leave it alone.
pub struct Accelerometer {
    file: File,
    path: PathBuf,
    pending: Vec<u8>,
    /// Axes accumulate here and are latched out whole on `EV_SYN`.
    building: Sample,
}

impl Accelerometer {
    /// Find and open the accelerometer.
    pub fn open() -> Result<Self> {
        let raw = std::fs::read_to_string("/proc/bus/input/devices")
            .context("read /proc/bus/input/devices")?;
        let Some(node) = pick_accelerometer(&raw) else {
            bail!("no accelerometer in /proc/bus/input/devices");
        };
        let path = PathBuf::from(format!("/dev/input/{node}"));
        let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
        Ok(Self {
            file,
            path,
            pending: Vec::with_capacity(EVENT_BYTES * 8),
            building: Sample::default(),
        })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn fd(&self) -> std::os::unix::io::RawFd {
        self.file.as_raw_fd()
    }

    /// Read whatever is ready and return the reports that completed.
    pub fn read_batch(&mut self) -> Result<Vec<Sample>> {
        let mut buf = [0u8; EVENT_BYTES * 16];
        let n = self.file.read(&mut buf).context("read accelerometer")?;
        if n == 0 {
            bail!("accelerometer {} closed", self.path.display());
        }
        self.pending.extend_from_slice(&buf[..n]);

        let mut out = Vec::new();
        let whole = self.pending.len() / EVENT_BYTES;
        for i in 0..whole {
            let Some((kind, code, value)) = decode_raw(&self.pending[i * EVENT_BYTES..]) else {
                continue;
            };
            match (kind, code) {
                (EV_ABS, ABS_X) => self.building.x = value,
                (EV_ABS, ABS_Y) => self.building.y = value,
                (EV_ABS, ABS_Z) => self.building.z = value,
                (EV_ABS, ABS_TILT) => self.building.tilt = value,
                // The report is only complete here.
                (EV_SYN, _) => out.push(self.building),
                _ => {}
            }
        }
        self.pending.drain(..whole * EVENT_BYTES);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Scribe's own `/proc/bus/input/devices`, captured with a keyboard
    /// present. Trimmed to the lines the parse reads.
    const CAPTURE: &str = "\
I: Bus=0019 Vendor=0001 Product=0001 Version=0100
N: Name=\"bd71828-pwrkey\"
H: Handlers=event0 perfmgr
B: PROP=0
B: EV=3
B: KEY=100000 0 0 0

I: Bus=0018 Vendor=003d Product=0000 Version=0000
N: Name=\"kx132-accel\"
H: Handlers=event1 perfmgr
B: PROP=0
B: EV=9
B: ABS=1000007

I: Bus=0018 Vendor=2d1f Product=0158 Version=1827
N: Name=\"WacomDigitizer\"
H: Handlers=event2 perfmgr
B: PROP=2
B: EV=b
B: KEY=1c03 0 0 0 0 0 0 0 0 0 0
B: ABS=f000003

I: Bus=0000 Vendor=0000 Product=0000 Version=0000
N: Name=\"pt_mt\"
H: Handlers=event3 perfmgr
B: PROP=2
B: EV=f
B: KEY=0
B: REL=0
B: ABS=ee18000 0

I: Bus=0018 Vendor=2d1f Product=0158 Version=1827
N: Name=\"stylus-custom\"
H: Handlers=event4 perfmgr
B: PROP=0
B: EV=b
B: KEY=1c03 0 0 0 0 0 0 0 0 0 0
B: ABS=f000003

I: Bus=0003 Vendor=1234 Product=5678 Version=0001
N: Name=\"uinput-kbd test keyboard\"
H: Handlers=event5 perfmgr
B: PROP=0
B: EV=3
B: KEY=3ffffff fffffffc

";

    #[test]
    fn the_keyboard_is_picked_out_of_the_devices_this_scribe_has() {
        assert_eq!(pick_keyboard(CAPTURE).as_deref(), Some("event5"));
    }

    #[test]
    fn the_pen_stylus_and_power_key_are_not_keyboards() {
        // All three carry EV_KEY for a few buttons, which is why the rule tests
        // for a letter rather than for EV_KEY at all.
        let baseline = CAPTURE.split("I: Bus=0003").next().unwrap();
        assert_eq!(pick_keyboard(baseline), None);
    }

    #[test]
    fn key_q_decides_and_the_low_word_is_last() {
        // The uinput keyboard advertises keys 2..=57.
        assert!(advertises("3ffffff fffffffc", code::Q));
        // The power key advertises only KEY_POWER, three words up.
        assert!(!advertises("100000 0 0 0", code::Q));
        assert!(!advertises("1c03 0 0 0 0 0 0 0 0 0 0", code::Q));
        assert!(!advertises("0", code::Q));
    }

    #[test]
    fn a_code_above_the_first_word_is_read_from_the_right() {
        // ABS_MT_TRACKING_ID is 57, so word 1 — the first of two — and bit 25.
        assert!(advertises("ee18000 0", ABS_MT_TRACKING_ID));
        assert!(advertises("2618000 0", ABS_MT_TRACKING_ID));
        assert!(advertises("6608000 0", ABS_MT_TRACKING_ID));
        // A bitmap too short to describe code 57 answers no rather than panics.
        assert!(!advertises("f000003", ABS_MT_TRACKING_ID));
        assert!(!advertises("", ABS_MT_TRACKING_ID));
        // `100 7` sets bit 40, not 57 — the two are eight apart in the same word.
        assert!(!advertises("100 7", ABS_MT_TRACKING_ID));
    }

    /// The three panels, and the two pen nodes that report absolute axes beside
    /// one of them.
    #[test]
    fn the_finger_panel_is_picked_over_the_pen_on_every_device() {
        assert_eq!(pick_touchscreen(CAPTURE).as_deref(), Some("event3"));
        assert_eq!(pick_touchscreen(COLORSOFT).as_deref(), Some("event1"));
        assert_eq!(pick_touchscreen(OASIS2).as_deref(), Some("event9"));
    }

    #[test]
    fn a_device_with_no_panel_is_not_a_panic() {
        assert_eq!(pick_touchscreen("I: Bus=0000\nN: Name=\"pwrkey\"\n"), None);
    }

    #[test]
    fn the_accelerometer_is_picked_over_the_other_sensors() {
        assert_eq!(pick_accelerometer(CAPTURE).as_deref(), Some("event1"));
        // Two light sensors and an interrupt mirror are listed before the real
        // one; without the axes in the rule the first of them wins.
        assert_eq!(pick_accelerometer(OASIS2).as_deref(), Some("event7"));
    }

    #[test]
    fn a_device_with_no_accelerometer_reports_none() {
        assert_eq!(pick_accelerometer(COLORSOFT), None);
    }

    #[test]
    fn page_turn_buttons_are_not_a_keyboard() {
        // KEY_PAGEUP and KEY_PAGEDOWN, three words up and nowhere near Q.
        assert_eq!(pick_keyboard(OASIS2), None);
    }

    /// The Colorsoft's own `/proc/bus/input/devices`: a power key and a panel,
    /// and nothing else at all.
    const COLORSOFT: &str = "\
I: Bus=0019 Vendor=0001 Product=0001 Version=0100
N: Name=\"bd71828-pwrkey\"
H: Handlers=event0 ktch
B: PROP=0
B: EV=3
B: KEY=100000 0 0 0

I: Bus=0018 Vendor=0000 Product=0000 Version=0000
N: Name=\"fts_ts\"
H: Handlers=event1 ktch
B: PROP=2
B: EV=b
B: KEY=400 0 0 0 0 0 0 0 0 0 0
B: ABS=2618000 0

";

    /// The Oasis 2's, trimmed to the blocks that decide something: two power
    /// keys, the page-turn pair, two light sensors, the accelerometer in both
    /// the forms it is published in, and the panel.
    const OASIS2: &str = "\
I: Bus=0019 Vendor=0000 Product=0000 Version=0000
N: Name=\"30370000.snvs:snvs-powerkey\"
H: Handlers=kbd event0
B: PROP=0
B: EV=3
B: KEY=100000 0 0 0

I: Bus=0019 Vendor=0001 Product=0001 Version=0100
N: Name=\"gpio-keys\"
H: Handlers=kbd event4
B: PROP=0
B: EV=3
B: KEY=2100 0 0 0

I: Bus=0000 Vendor=0000 Product=0000 Version=0000
N: Name=\"max44009_als\"
H: Handlers=event5
B: PROP=0
B: EV=9
B: ABS=100 0

I: Bus=0000 Vendor=0000 Product=0000 Version=0000
N: Name=\"max44009_als1\"
H: Handlers=event6
B: PROP=0
B: EV=9
B: ABS=100 0

I: Bus=0018 Vendor=0000 Product=0000 Version=0000
N: Name=\"bma2x2\"
H: Handlers=event7
B: PROP=0
B: EV=9
B: ABS=100 7

I: Bus=0018 Vendor=0000 Product=0000 Version=0000
N: Name=\"bma_interrupt\"
H: Handlers=event8
B: PROP=0
B: EV=d
B: REL=3c6
B: ABS=3000000

I: Bus=0000 Vendor=0000 Product=0000 Version=0000
N: Name=\"cyttsp5_mt\"
H: Handlers=event9
B: PROP=2
B: EV=f
B: KEY=6420 0 0 0 0 0 0 0 0 0 0
B: REL=0
B: ABS=6608000 0

";

    /// Build one `struct input_event` the way the kernel lays it out.
    fn event(kind: u16, code: u16, value: i32) -> Vec<u8> {
        let mut buf = vec![0u8; EVENT_BYTES];
        buf[TIME_BYTES..TIME_BYTES + 2].copy_from_slice(&kind.to_ne_bytes());
        buf[TIME_BYTES + 2..TIME_BYTES + 4].copy_from_slice(&code.to_ne_bytes());
        buf[TIME_BYTES + 4..TIME_BYTES + 8].copy_from_slice(&value.to_ne_bytes());
        buf
    }

    #[test]
    fn press_release_and_repeat_are_told_apart() {
        let down = decode(&event(EV_KEY, 35, 1)).unwrap();
        assert_eq!(
            down,
            KeyEvent {
                code: 35,
                pressed: true,
                repeat: false
            }
        );

        let up = decode(&event(EV_KEY, 35, 0)).unwrap();
        assert_eq!(
            up,
            KeyEvent {
                code: 35,
                pressed: false,
                repeat: false
            }
        );

        let held = decode(&event(EV_KEY, 35, 2)).unwrap();
        assert_eq!(
            held,
            KeyEvent {
                code: 35,
                pressed: true,
                repeat: true
            }
        );
    }

    #[test]
    fn non_key_events_are_skipped() {
        // EV_SYN packets separate reports and carry no keystroke.
        assert_eq!(decode(&event(0, 0, 0)), None);
        // A short read is not an event yet.
        assert_eq!(decode(&[0u8; 4]), None);
    }

    mod accelerometer {
        use super::*;

        /// The rule is "absolute axes and no keys". Against the five real
        /// devices on this Scribe that picks exactly one — and picking by name
        /// would have been wrong, because the udev rules ship drivers for
        /// kx132, bma2x2 and bma4xy.
        #[test]
        fn the_accelerometer_is_the_one_device_with_axes_and_no_keys() {
            assert_eq!(pick_accelerometer(CAPTURE).as_deref(), Some("event1"));
        }

        /// The pen and the touchscreen both report absolute axes, so an
        /// ABS-only rule would have taken whichever came first.
        #[test]
        fn the_pen_and_the_touchscreen_are_not_accelerometers() {
            for block in blocks(CAPTURE) {
                let picked = block.ev & EV_ABS_BIT != 0 && block.ev & EV_KEY_BIT == 0;
                assert_eq!(
                    picked,
                    block.handler.as_deref() == Some("event1"),
                    "{:?} ev={:#x}",
                    block.handler,
                    block.ev
                );
            }
        }

        /// Every device in the dump is seen, and each keeps its own bitmaps —
        /// the failure mode of a hand-rolled block parser is bleeding one
        /// device's fields into the next.
        #[test]
        fn every_block_is_parsed_with_its_own_fields() {
            let all = blocks(CAPTURE);
            assert_eq!(all.len(), 6);
            assert_eq!(all[0].ev, 0x3, "pwrkey");
            assert_eq!(all[1].ev, 0x9, "accel");
            assert!(all[1].keys.is_empty(), "the accel advertises no keys");
            assert_eq!(all[5].keys, "3ffffff fffffffc", "the keyboard");
        }

        /// Axes arrive as separate events and only mean something together, so
        /// a report is latched at EV_SYN and never before. Reporting on a
        /// partial set is the bug that made every touch land one position
        /// behind.
        #[test]
        fn a_report_is_latched_whole_at_syn() {
            let mut stream = Vec::new();
            for (kind, code, value) in [
                (EV_ABS, ABS_X, -16),
                (EV_ABS, ABS_Y, 1024),
                (EV_ABS, ABS_Z, 32),
                (EV_ABS, ABS_TILT, 4),
                (EV_SYN, 0, 0),
            ] {
                stream.extend_from_slice(&event(kind, code, value));
            }

            // Feed it the way a read would, then check only one sample came out
            // and it has every axis.
            let mut building = Sample::default();
            let mut out = Vec::new();
            for i in 0..stream.len() / EVENT_BYTES {
                let (kind, code, value) = decode_raw(&stream[i * EVENT_BYTES..]).unwrap();
                match (kind, code) {
                    (EV_ABS, ABS_X) => building.x = value,
                    (EV_ABS, ABS_Y) => building.y = value,
                    (EV_ABS, ABS_Z) => building.z = value,
                    (EV_ABS, ABS_TILT) => building.tilt = value,
                    (EV_SYN, _) => out.push(building),
                    _ => {}
                }
            }
            assert_eq!(
                out,
                vec![Sample {
                    x: -16,
                    y: 1024,
                    z: 32,
                    tilt: 4
                }]
            );
        }
    }

    #[test]
    fn the_event_struct_is_16_bytes_on_the_device() {
        // armv7 has 32-bit longs, so timeval is 8 bytes and the event is 16 —
        // which is what `od` showed when the input chain was proven.
        assert_eq!(TIME_BYTES + 8, EVENT_BYTES);
        if cfg!(target_pointer_width = "32") {
            assert_eq!(EVENT_BYTES, 16);
        }
    }
}
