//! `preventScreenSaver` and `flIntensity` on `com.lab126.powerd`. One latch
//! covers the screensaver, the suspend behind it and WiFi; the other sets the
//! frontlight.

use std::process::Command;

/// Time without a key or a touch before `prevent_screensaver(false)`.
pub const IDLE_SLEEP: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Hold the screensaver off, or release it. `status` is read back.
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

/// How many presses of a brightness key cover the whole range.
const FRONTLIGHT_STEPS: i32 = 8;

/// Take the frontlight one step brighter or dimmer. `flMaxIntensity` is the
/// ceiling; a device answering neither property has none to set.
pub fn step_frontlight(up: bool) {
    let (Some(at), Some(max)) = (intensity("flIntensity"), intensity("flMaxIntensity")) else {
        eprintln!("power: no frontlight to set");
        return;
    };
    let want = stepped(at, max, up);
    if want == at {
        eprintln!("power: frontlight held at {at} of {max}");
        return;
    }
    match Command::new("lipc-set-prop")
        .args(["-i", "com.lab126.powerd", "flIntensity", &want.to_string()])
        .status()
    {
        Ok(_) => eprintln!("power: frontlight {want} of {max}"),
        Err(err) => eprintln!("power: frontlight {want} unavailable ({err})"),
    }
}

/// Where one press lands, held inside `0..=max`.
fn stepped(at: i32, max: i32, up: bool) -> i32 {
    let step = (max / FRONTLIGHT_STEPS).max(1);
    let want = if up { at + step } else { at - step };
    want.clamp(0, max)
}

/// One of the integer properties on `com.lab126.powerd`.
fn intensity(property: &str) -> Option<i32> {
    let out = Command::new("lipc-get-prop")
        .args(["-i", "com.lab126.powerd", property])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok())?
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

    /// The scale `flMaxIntensity` reports here: 0 through 24.
    #[test]
    fn eight_presses_cover_the_frontlights_whole_range() {
        let mut at = 0;
        for _ in 0..FRONTLIGHT_STEPS {
            at = stepped(at, 24, true);
        }
        assert_eq!(at, 24);
        for _ in 0..FRONTLIGHT_STEPS {
            at = stepped(at, 24, false);
        }
        assert_eq!(at, 0);
    }

    #[test]
    fn a_step_stops_at_either_end() {
        assert_eq!(stepped(24, 24, true), 24);
        assert_eq!(stepped(0, 24, false), 0);
        assert_eq!(stepped(23, 24, true), 24);
        assert_eq!(stepped(1, 24, false), 0);
    }

    /// A scale shorter than [`FRONTLIGHT_STEPS`] moves one level at a time.
    #[test]
    fn a_short_scale_still_moves() {
        assert_eq!(stepped(0, 4, true), 1);
        assert_eq!(stepped(2, 4, false), 1);
    }
}
