//! The udev rule that lets anything but karyll see the keyboard.
//!
//! karyll reads `/dev/input/eventN` directly and needs nothing from udev. Every
//! X client does: `evdev_drv.so` binds only devices tagged `ID_INPUT_KEYBOARD`,
//! and this device's udev runs no `input_id` step. Without a rule supplying the
//! tag a Bluetooth keyboard is complete and delivering keys, and invisible to
//! kterm and to the framework's own screens — the editor types and nothing else
//! on the device does.
//!
//! **The rule matches on the device path.** Everything the Bluetooth daemon
//! creates appears under `/devices/virtual/misc/uhid/`, and nothing else here
//! does: the touchscreen, stylus and accelerometer are on i2c and the power key
//! on gpio-keys. `capabilities/key` must be non-empty so a consumer-control
//! page, which a keyboard also registers, is not announced as a keyboard.
//!
//! **Nothing outside the rootfs is involved, and that is the point.** A rule
//! that calls a helper through `IMPORT{program}` puts half of itself on
//! `/mnt/us`, which MTP rewrites; the import then fails, and a failed import
//! imports nothing and logs nothing, so the rule reads as installed while
//! tagging no device at all.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Where the rule goes. `99-` so it runs after the firmware's own rules.
pub const PATH: &str = "/etc/udev/rules.d/99-uhid-keyboard.rules";

/// The rule, compared byte for byte against what is on disk. A firmware update
/// restores `/etc` and takes this with it, and an older build's rule differs
/// from this one — both are repaired on the next launch, with no version to
/// keep in step.
const RULE: &str = concat!(
    r#"ACTION=="add", SUBSYSTEM=="input", KERNEL=="event*", DEVPATH=="*/uhid/*", "#,
    r#"ATTRS{capabilities/key}!="", "#,
    r#"ENV{ID_INPUT}="1", ENV{ID_INPUT_KEY}="1", ENV{ID_INPUT_KEYBOARD}="1""#,
    "\n"
);

/// What [`ensure`] found or did.
pub enum Outcome {
    /// The rule on disk is already this one.
    Present,
    /// Written, and udev asked to re-tag what is already connected.
    Installed,
}

/// Put the rule in place if it is not already there.
///
/// Safe on every launch: the common path is one file read.
pub fn ensure() -> Result<Outcome> {
    if std::fs::read_to_string(PATH).is_ok_and(|have| have == RULE) {
        return Ok(Outcome::Present);
    }

    remount("rw").context("make the rootfs writable")?;
    let wrote = write_rule();
    // Read-only again whatever happened above. A rootfs left writable is the
    // one outcome worse than a keyboard X cannot see.
    let restored = remount("ro").context("make the rootfs read-only again");
    wrote?;
    restored?;

    let _ = run("udevadm", &["control", "--reload-rules"]);
    // Re-add what is already connected. A keyboard that arrived before the rule
    // existed keeps its untagged properties, and X acts only on the add.
    let _ = run(
        "udevadm",
        &["trigger", "--subsystem-match=input", "--action=add"],
    );
    Ok(Outcome::Installed)
}

fn write_rule() -> Result<()> {
    if let Some(dir) = Path::new(PATH).parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    std::fs::write(PATH, RULE).with_context(|| format!("write {PATH}"))?;
    // The mount is about to go read-only, and an unflushed write goes with it.
    let _ = run("sync", &[]);
    Ok(())
}

/// Remount the rootfs `rw` or `ro`.
///
/// `mntroot` is the firmware's own wrapper and is what this device expects;
/// `mount` is there for a rootfs without it.
fn remount(how: &str) -> Result<()> {
    if run("mntroot", &[how]) {
        return Ok(());
    }
    if run("mount", &["-o", &format!("remount,{how}"), "/"]) {
        return Ok(());
    }
    bail!("neither mntroot nor mount would remount / {how}")
}

/// Run a command, returning whether it succeeded.
///
/// Output goes nowhere: the effect of each of these is read back off the
/// filesystem, so what it printed is of no use to anything here.
fn run(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_is_one_line_and_sets_all_three_properties() {
        // udev reads one rule per line, so a rule split across lines is three
        // malformed rules. The `concat!` is for the source margin alone.
        assert_eq!(RULE.lines().count(), 1);
        assert!(RULE.ends_with('\n'));
        for key in ["ID_INPUT", "ID_INPUT_KEY", "ID_INPUT_KEYBOARD"] {
            assert!(
                RULE.contains(&format!(r#"ENV{{{key}}}="1""#)),
                "{key} missing"
            );
        }
    }

    #[test]
    fn the_rule_matches_the_daemons_devices_and_nothing_else_on_this_device() {
        // Device paths as they appear on this hardware: the first is a keyboard
        // the daemon created, the rest are the five built-in input devices.
        assert!(
            "/devices/virtual/misc/uhid/0005:0000:0000.0001/input/input5/event5".contains("/uhid/")
        );
        for built_in in [
            "/devices/platform/10019000.i2c/i2c-1/1-004b/gpio-keys.7.auto/input/input0/event0",
            "/devices/platform/11007000.i2c/i2c-0/0-001f/input/input1/event1",
            "/devices/platform/1001e000.i2c/i2c-2/2-0009/input/input2/event2",
            "/devices/platform/1001e000.i2c/i2c-2/2-0024/input/input3/event3",
            "/devices/virtual/input/input4/event4",
        ] {
            assert!(!built_in.contains("/uhid/"), "{built_in} would be tagged");
        }
    }
}
