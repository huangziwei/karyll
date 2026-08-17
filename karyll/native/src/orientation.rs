//! Which way up the window is, and how a touch maps onto it.
//!
//! Two separate things share the word "orientation" here:
//!
//! - **What the framework does on its own.** An accelerometer-driven flip, and
//!   only ever 180°. That is the framework's choice, not the sensor's: the
//!   part is a Kionix KX132-1211 whose tilt-position register reports six
//!   states including both landscapes, and it is readable from
//!   `/dev/input/eventN` like any other input device. Turning the Scribe on its
//!   side does nothing in the stock reader because the reader is portrait-only,
//!   not because the hardware cannot say.
//! - **What an app asks for.** The lab126 window manager reads the window's
//!   name as a layout spec, ending in `_O:<letter>`. Setting `L` or `R` there is
//!   how landscape is requested, which is why the stock reader offers it as a
//!   setting rather than as a gesture. Nothing rotates into landscape by itself.
//!
//! The touchscreen is panel-fixed whichever way the window is turned, so every
//! tap is mapped here before anything looks at where it landed.

use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    /// Portrait.
    #[default]
    Up,
    /// Portrait, turned end over end.
    Down,
    /// Landscape.
    Left,
    /// Landscape, the other way.
    Right,
}

impl Orientation {
    /// The letter the window manager expects in the name's `_O:` field.
    pub fn letter(self) -> char {
        match self {
            Self::Up => 'U',
            Self::Down => 'D',
            Self::Left => 'L',
            Self::Right => 'R',
        }
    }

    pub fn from_letter(letter: &str) -> Self {
        match letter {
            "D" => Self::Down,
            "L" => Self::Left,
            "R" => Self::Right,
            _ => Self::Up,
        }
    }

    /// Which way the device is physically being held, from the accelerometer's
    /// own position code.
    ///
    /// **Established on hardware, and not derivable any other way.**
    /// The driver advertises `ABS_X`, `ABS_Y` and `ABS_Z` and then reports zero
    /// for all three forever, so there is no gravity vector to reason from —
    /// the whole signal is one code on `ABS` 24, which the X driver calls
    /// `rotation`.
    ///
    /// The four values were read off by turning the device while watching what
    /// the window manager settled on, and every observation agreed:
    ///
    /// | code | | code | |
    /// |---|---|---|---|
    /// | 15 | `Up` | 17 | `Right` |
    /// | 16 | `Down` | 18 | `Left` |
    ///
    /// The portrait pair comes first and the landscape pair second, which is a
    /// good sign the encoding is deliberate rather than coincidence. Anything
    /// else is `None` — the sensor emits a settling burst on power-up, and an
    /// unknown code is a reason to hold the last orientation rather than guess.
    pub fn from_tilt(code: i32) -> Option<Self> {
        match code {
            15 => Some(Self::Up),
            16 => Some(Self::Down),
            17 => Some(Self::Right),
            18 => Some(Self::Left),
            _ => None,
        }
    }

    /// Ask the window manager which way it currently has the screen.
    ///
    /// Silent, deliberately. This is polled several times a second to catch the
    /// framework's own 180° flips, and logging every answer puts ninety
    /// identical lines into a two-minute session's log and buries the ones that
    /// matter. Callers log the transitions they care about.
    pub fn detect() -> Self {
        let Ok(out) = Command::new("lipc-get-prop")
            .args(["com.lab126.winmgr", "orientation"])
            .output()
        else {
            return Self::Up;
        };
        if !out.status.success() {
            return Self::Up;
        }
        Self::from_letter(String::from_utf8_lossy(&out.stdout).trim())
    }

    /// Map a point from panel coordinates onto the window.
    ///
    /// The point must already be in the panel's own pixel space, which is
    /// always portrait and never changes shape. `window` is the surface as it
    /// currently is, which in landscape has its sides swapped — so in landscape
    /// the window's *height* is the panel's width, and vice versa.
    ///
    /// The two landscape mappings are a guess at which way round the compositor
    /// turns us — there is no way to know without running it, and the log line
    /// in the tap handler is what settles it. If taps land on the mirror of
    /// where they should, `Left` and `Right` are the wrong way round.
    pub fn apply(self, x: i32, y: i32, window: (u16, u16)) -> (i32, i32) {
        let (w, h) = (window.0 as i32, window.1 as i32);
        match self {
            Self::Up => (x, y),
            Self::Down => (w - x, h - y),
            // A quarter turn swaps the axes: what runs down the panel runs
            // across the window.
            Self::Left => (y, h - x),
            Self::Right => (w - y, x),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORTRAIT: (u16, u16) = (1860, 2480);
    const LANDSCAPE: (u16, u16) = (2480, 1860);

    /// The four codes read off the device, with the two pairs the right way
    /// round — a transposition here would rotate the page ninety degrees from
    /// where the writer is holding it, which is the whole failure this mapping
    /// exists to avoid.
    #[test]
    fn the_accelerometer_codes_are_the_ones_the_device_reported() {
        assert_eq!(Orientation::from_tilt(15), Some(Orientation::Up));
        assert_eq!(Orientation::from_tilt(16), Some(Orientation::Down));
        assert_eq!(Orientation::from_tilt(17), Some(Orientation::Right));
        assert_eq!(Orientation::from_tilt(18), Some(Orientation::Left));

        // 15/16 are the portrait pair and 17/18 the landscape pair — a
        // transposition between the pairs would be the worst kind of wrong.
        let landscape = |c| {
            matches!(
                Orientation::from_tilt(c),
                Some(Orientation::Left | Orientation::Right)
            )
        };
        assert!(!landscape(15));
        assert!(!landscape(16));
        assert!(landscape(17));
        assert!(landscape(18));
    }

    /// The sensor emits a settling burst when it powers up, so an unrecognised
    /// code has to mean "hold what you had" rather than any orientation at all.
    #[test]
    fn an_unknown_code_names_no_orientation() {
        for code in [0, 1, 14, 19, 20, -1, 255] {
            assert_eq!(Orientation::from_tilt(code), None, "{code}");
        }
    }

    #[test]
    fn upright_passes_coordinates_through() {
        assert_eq!(Orientation::Up.apply(100, 200, PORTRAIT), (100, 200));
    }

    #[test]
    fn a_half_turn_mirrors_both_axes() {
        assert_eq!(Orientation::Down.apply(100, 200, PORTRAIT), (1760, 2280));
        // The centre is its own mirror.
        assert_eq!(Orientation::Down.apply(930, 1240, PORTRAIT), (930, 1240));
    }

    #[test]
    fn mirroring_twice_is_the_identity() {
        let (x, y) = Orientation::Down.apply(300, 700, PORTRAIT);
        assert_eq!(Orientation::Down.apply(x, y, PORTRAIT), (300, 700));
    }

    #[test]
    fn a_quarter_turn_maps_the_panel_across_the_long_edge() {
        // The bug this pins: a point halfway down the panel has to arrive
        // halfway across a landscape window. Scaling into the window's own axes
        // first squashed this by 1860/2480, so a tap aimed at the third button
        // of five landed on the second.
        let down_the_panel = 2480 / 2;
        let (x, _) = Orientation::Left.apply(900, down_the_panel, LANDSCAPE);
        assert_eq!(
            x, down_the_panel,
            "the panel's long axis is the window's width"
        );
        assert_eq!(
            x * 5 / LANDSCAPE.0 as i32,
            2,
            "still the third of five cells"
        );
    }

    #[test]
    fn a_quarter_turn_lands_inside_the_landscape_window() {
        // The corners of the panel must map to corners of the window, or taps
        // near an edge fall outside it.
        for (px, py) in [(0, 0), (1859, 0), (0, 2479), (1859, 2479)] {
            for turn in [Orientation::Left, Orientation::Right] {
                let (x, y) = turn.apply(px, py, LANDSCAPE);
                assert!(
                    (0..=LANDSCAPE.0 as i32).contains(&x),
                    "{turn:?} sent x out of the window: {x}"
                );
                assert!(
                    (0..=LANDSCAPE.1 as i32).contains(&y),
                    "{turn:?} sent y out of the window: {y}"
                );
            }
        }
    }

    #[test]
    fn the_letter_is_what_the_window_manager_reads() {
        assert_eq!(Orientation::Up.letter(), 'U');
        assert_eq!(Orientation::Left.letter(), 'L');
        assert_eq!(Orientation::from_letter("R"), Orientation::Right);
        // Anything unrecognised is upright, which is never worse than refusing
        // to start.
        assert_eq!(Orientation::from_letter(""), Orientation::Up);
        assert_eq!(Orientation::from_letter("nonsense"), Orientation::Up);
    }

    // `rotated()` and `is_landscape()` went with the *Rotate* button they
    // existed for: the orientation follows the device now, so there is nothing
    // to toggle and no caller left asking which way round we are. Their tests
    // went with them rather than being kept alive against a hypothetical future
    // caller.
}
