//! Keeping the device awake while the editor is open.
//!
//! karyll grabs the keyboard with `EVIOCGRAB`, so keystrokes never reach the
//! framework and cannot reset `powerd`'s idle timer. Only touches do, and
//! writing does not involve touching the glass, so the device sleeps
//! mid-sentence.
//!
//! `preventScreenSaver` is the lever — `rw Int` on `com.lab126.powerd`. The
//! screensaver is the first step of the *idle* chain and the suspend follows
//! it, so holding it off holds off both.
//!
//! **The power button is a separate path and is not affected.** It is its own
//! input device (`bd71828-pwrkey`), and powerd reports `prevent_screen_saver`,
//! `defer_suspend` and `suspend_grace` as three distinct states with
//! `deferSuspend` and `abortSuspend` as the suspend-level controls. So a
//! deliberate press still sleeps the device while this latch is held, which is
//! what makes holding it for a whole session acceptable.
//!
//! It is a latch rather than a one-shot, so releasing it matters more than
//! setting it: it also holds WiFi awake, and a session that leaves it set has
//! changed the device's behaviour after karyll is gone. It is released on every
//! exit path in `run`, and again by the launcher's trap, which fires even for
//! the aborts that skip Rust's own cleanup.
//!
//! **And released while the session is still open, once the writer stops.**
//! Held for the whole session it fixes the interruption and buys a Kindle that
//! cannot sleep: leave the editor open on a desk and a device rated in weeks of
//! standby is flat by morning. It is a latch on
//! *writing*, not on the app being on screen, so [`crate::Editor`] gives it back
//! after [`IDLE_SLEEP`] with no key and no touch, and takes it again on the next
//! one.

use std::process::Command;

/// How long the writer has to be away before the device may sleep.
///
/// **Generous, because the cost is asymmetric.** Sleeping on someone who paused
/// to think is the interruption this whole module exists to prevent, and waking
/// the device again means the power button or the glass — a Bluetooth keystroke
/// will not do it, since the daemon carrying it is suspended too. A quarter of
/// an hour is longer than any pause in writing and far shorter than a night.
pub const IDLE_SLEEP: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Hold the screensaver off, or let it come back.
///
/// The write is checked rather than assumed. A latch that quietly failed to
/// set means the device sleeps mid-sentence again; one that quietly failed to
/// release means a Kindle that never sleeps and a battery that goes flat
/// overnight. Neither announces itself, so both are read back and logged.
///
/// Best-effort otherwise: off-device there is no `lipc-set-prop` at all, and
/// failing here is a device that sleeps rather than a session that cannot run.
pub fn prevent_screensaver(on: bool) {
    let value = if on { "1" } else { "0" };
    if let Err(err) = Command::new("lipc-set-prop")
        .args(["com.lab126.powerd", "preventScreenSaver", value])
        .status()
    {
        eprintln!("power: preventScreenSaver={value} unavailable ({err})");
        return;
    }
    match status().as_deref().and_then(latched) {
        Some(held) if held == on => eprintln!("power: screensaver held off = {held}"),
        Some(held) => eprintln!("power: asked for preventScreenSaver={value}, daemon says {held}"),
        None => eprintln!("power: preventScreenSaver={value} set, daemon did not confirm"),
    }
}

/// The daemon's own report of what it is doing.
fn status() -> Option<String> {
    let out = Command::new("lipc-get-prop")
        .args(["com.lab126.powerd", "status"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether the latch is held, read out of the daemon's status report.
///
/// The report spells the field `prevent_screen_saver` where the writable
/// property is `preventScreenSaver`. They are one latch under two spellings,
/// which is the only reason this needs its own parse rather than reading the
/// property straight back.
fn latched(status: &str) -> Option<bool> {
    let value = status
        .lines()
        .find_map(|line| line.trim().strip_prefix("prevent_screen_saver:"))?;
    Some(value.trim() != "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim from a device probe, so the parse is written against what the
    /// daemon actually prints rather than against a guess at its shape.
    const REPORT: &str = "Powerd state: Active\n\
                          Remaining time in this state: 589.624499\n\
                          defer_suspend:0\n\
                          suspend_grace:0\n\
                          prevent_screen_saver:0\n";

    #[test]
    fn a_report_from_the_device_says_the_latch_is_open() {
        assert_eq!(latched(REPORT), Some(false));
    }

    #[test]
    fn a_held_latch_reads_back_as_held() {
        let held = REPORT.replace("prevent_screen_saver:0", "prevent_screen_saver:1");
        assert_eq!(latched(&held), Some(true));
    }

    #[test]
    fn a_report_without_the_field_is_not_an_answer_either_way() {
        assert_eq!(latched("Powerd state: Active\n"), None);
    }

    #[test]
    fn the_field_is_not_confused_with_the_others_beside_it() {
        let deferred = REPORT.replace("defer_suspend:0", "defer_suspend:1");
        assert_eq!(latched(&deferred), Some(false));
    }
}
