//! Which way up the window is, and how a touch maps onto it. **The framework
//! flips 180° on its own**; **landscape is only ever asked for**, through the
//! `_O:<letter>` field of the window name. Touch is panel-fixed and mapped here.

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

    /// Which way the device is physically held. **The whole signal is one code
    /// on `ABS` 24** — `ABS_X`/`Y`/`Z` report zero forever — and an unknown code
    /// is `None`, since the sensor emits a settling burst on power-up.
    pub fn from_tilt(code: i32) -> Option<Self> {
        match code {
            15 => Some(Self::Up),
            16 => Some(Self::Down),
            17 => Some(Self::Right),
            18 => Some(Self::Left),
            _ => None,
        }
    }

    /// Ask the window manager which way it currently has the screen. Silent
    /// deliberately: this is polled several times a second, so callers log the
    /// transitions they care about.
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

    /// Map a point from panel coordinates onto the window. The point must
    /// already be in the panel's own pixel space, which is always portrait;
    /// `window` in landscape has its sides swapped.
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

    /// The four codes, with the two pairs the right way round: a transposition
    /// rotates the page ninety degrees from where the writer is holding it.
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
        // A point halfway down the panel has to arrive halfway across a
        // landscape window; scaling into the window's axes first squashes it.
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
}
