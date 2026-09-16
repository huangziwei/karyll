//! The framework's on-screen keyboard, asked for over lipc: it maps its own
//! window over the foot of ours. **It composes with its own predictor**, whose
//! candidate bar is drawn on that window and whose commits arrive over lipc.

use std::process::Command;

/// The lipc service [`open`] and [`close`] set a property on.
const SERVICE: &str = "com.lab126.keyboard";
const OPEN: &str = "open";
const CLOSE: &str = "close";

/// The name [`open`] hands over, and [`close`] matches against. The keyboard
/// addresses its commits to it, so [`crate::lipc::Service`] must hold this name
/// on the bus before the keyboard is raised.
pub const CLIENT: &str = "com.karyll.editor";

/// The layout [`open`] asks for. `pad` and `web` are the two the framework
/// matches against; any other value draws the alphabetic one.
const LAYOUT: &str = "abc";

/// Bit 0 runs the predictor and its candidate bar, bit 1 asks for surrounding
/// text, bit 2 makes backspace take a word.
const FLAGS: u32 = 0x1;

/// The live layout, rewritten on a keyboard language change.
const KEYMAP: &str = "/var/local/system/current.keymap";

/// The [`KEYMAP`] field naming the keys and the candidate bar together.
const FIELD: &str = "\"portrait_height\"";

/// Panel height to keyboard height, as the keymaps state it. One figure per
/// panel, the same for all 29 languages.
const PANELS: [(i32, i32); 3] = [(2480, 808), (1696, 578), (1680, 578)];

/// Raises it, in the language the device is set to. Answers whether
/// `lipc-set-prop` exited clean.
pub fn open() -> bool {
    set(OPEN, &format!("{CLIENT}:{LAYOUT}:{FLAGS}"))
}

/// Dismisses it, by the bare [`CLIENT`] name.
pub fn close() -> bool {
    set(CLOSE, CLIENT)
}

/// One `lipc-set-prop` on [`SERVICE`].
fn set(prop: &str, value: &str) -> bool {
    match Command::new("lipc-set-prop")
        .args([SERVICE, prop, value])
        .status()
    {
        Ok(status) if status.success() => true,
        Ok(status) => {
            eprintln!("!! osk: lipc-set-prop {prop} {status}");
            false
        }
        Err(err) => {
            eprintln!("!! osk: lipc-set-prop would not run: {err}");
            false
        }
    }
}

/// How much of a `panel` px tall panel it covers, anchored to the foot and full
/// width: [`KEYMAP`] first, then [`PANELS`], then a third. **`panel` is the
/// hardware's own height** — the band does not turn with the window.
pub fn height(panel: i32) -> i32 {
    if let Some(said) = std::fs::read_to_string(KEYMAP).ok().and_then(|said| {
        let head: String = said.chars().take(2048).collect();
        of_keymap(&head)
    }) {
        return said;
    }
    of_panel(panel)
}

/// What [`PANELS`] states for a panel this tall, or a third of it.
fn of_panel(panel: i32) -> i32 {
    PANELS
        .iter()
        .find(|(height, _)| *height == panel)
        .map(|(_, band)| *band)
        .unwrap_or(panel / 3)
}

/// The [`FIELD`] value in `said`, as a positive number of pixels.
fn of_keymap(said: &str) -> Option<i32> {
    let (_, rest) = said.split_once(FIELD)?;
    let (_, rest) = rest.split_once(':')?;
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|height| *height > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The head of a [`KEYMAP`], as a Scribe writes it.
    const SCRIBE: &str = r#"{
    "keyboard_language" : "en-US",
    "candidate_height" : 100,
    "keyboard_height" : 708,
    "portrait_height" : 808,
    "landscape_height" : 808,
"#;

    #[test]
    fn the_live_layout_states_the_height() {
        assert_eq!(of_keymap(SCRIBE), Some(808));
    }

    #[test]
    fn a_keymap_without_the_field_states_nothing() {
        assert_eq!(of_keymap("{}"), None);
        assert_eq!(of_keymap(r#"{"portrait_height" : }"#), None);
        assert_eq!(of_keymap(r#"{"portrait_height" : 0}"#), None);
    }

    #[test]
    fn a_panel_off_the_table_takes_a_third() {
        assert_eq!(of_panel(2480), 808, "Scribe");
        assert_eq!(of_panel(1680), 578, "Colorsoft and Oasis 2");
        assert_eq!(of_panel(1696), 578);
        assert_eq!(of_panel(1448), 1448 / 3);
    }
}
