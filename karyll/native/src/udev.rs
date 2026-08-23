//! A udev rule tagging the Bluetooth daemon's uhid input nodes
//! `ID_INPUT_KEYBOARD`. `evdev_drv.so` binds nothing without the tag;
//! [`crate::evdev`] reads the node directly. See [`RULE`] for the match.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Where the rule goes. `99-` orders it after the rules in the directory.
pub const PATH: &str = "/etc/udev/rules.d/99-uhid-keyboard.rules";

/// The rule. Matched on the device path, which the daemon's nodes hold under
/// `/uhid/` and the i2c and gpio-keys devices do not. `ensure` compares it byte
/// for byte against `PATH` and rewrites a file that differs.
const RULE: &str = concat!(
    r#"ACTION=="add", SUBSYSTEM=="input", KERNEL=="event*", DEVPATH=="*/uhid/*", "#,
    r#"ATTRS{capabilities/key}!="", "#,
    r#"ENV{ID_INPUT}="1", ENV{ID_INPUT_KEY}="1", ENV{ID_INPUT_KEYBOARD}="1""#,
    "\n"
);

/// What [`ensure`] found or did.
pub enum Outcome {
    /// The file at `PATH` matches [`RULE`].
    Present,
    /// Written, with a `udevadm trigger` over the connected devices.
    Installed,
}

/// Put the rule in place. A matching file costs one read.
pub fn ensure() -> Result<Outcome> {
    if std::fs::read_to_string(PATH).is_ok_and(|have| have == RULE) {
        return Ok(Outcome::Present);
    }

    remount("rw").context("make the rootfs writable")?;
    let wrote = write_rule();
    // Read-only again whatever happened above.
    let restored = remount("ro").context("make the rootfs read-only again");
    wrote?;
    restored?;

    let _ = run("udevadm", &["control", "--reload-rules"]);
    // Re-add the connected devices: a node created before the rule carries
    // untagged properties, and X acts on the add.
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

/// Remount the rootfs `rw` or `ro`, through `mntroot` or `mount`.
fn remount(how: &str) -> Result<()> {
    if run("mntroot", &[how]) {
        return Ok(());
    }
    if run("mount", &["-o", &format!("remount,{how}"), "/"]) {
        return Ok(());
    }
    bail!("neither mntroot nor mount would remount / {how}")
}

/// Run a command, returning whether it succeeded. Output is discarded; each
/// effect is read back off the filesystem.
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
        // udev reads one rule per line. The `concat!` is for the source margin.
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
        // Device paths from this hardware: one uhid keyboard, then the five
        // built-in input devices.
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
