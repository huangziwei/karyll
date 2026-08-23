//! `preventScreenSaver` on `com.lab126.powerd`, held while writing is under
//! way. One latch covers the screensaver and the suspend behind it, and holds
//! WiFi awake with them.

use std::process::Command;

/// Time without a key or a touch before `prevent_screensaver(false)`.
pub const IDLE_SLEEP: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Hold the screensaver off, or release it. `status` is read back: a set or a
/// release that failed is silent.
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

/// `com.lab126.powerd`'s `status` property.
fn status() -> Option<String> {
    let out = Command::new("lipc-get-prop")
        .args(["com.lab126.powerd", "status"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether the latch is held. `status` spells the field
/// `prevent_screen_saver`; the writable property is `preventScreenSaver`.
fn latched(status: &str) -> Option<bool> {
    let value = status
        .lines()
        .find_map(|line| line.trim().strip_prefix("prevent_screen_saver:"))?;
    Some(value.trim() != "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `lipc-get-prop com.lab126.powerd status` output, verbatim.
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
