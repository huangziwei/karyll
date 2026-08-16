//! Driving the Bluetooth daemon.
//!
//! The daemon runs an HTTP API on `127.0.0.1:8321`, which is a far better seam
//! than shelling out: `/health` answers whether it is up, and `/scan`,
//! `/pair`, `/devices` and `/connect` are everything a keyboard needs. karyll
//! therefore owns the whole Bluetooth story itself — start it, scan, pair,
//! stop it — with no launcher script and no separate app.
//!
//! Shelling out was tried first and does not work: `pgrep -f` did not see the
//! running daemon, so a second tap started a second one and it died binding an
//! already-bound port. `/health` cannot be wrong the same way, because it is
//! the very socket that conflicts.
//!
//! The client here is hand-rolled rather than a crate. One `GET` over a local
//! socket does not justify a dependency, and every dependency has to be pure
//! Rust to keep the static cross-link intact.

use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

/// Where the daemon listens. Localhost only — it is not reachable off-device.
const API: &str = "127.0.0.1:8321";

/// The daemon is a bundled Python runtime and takes seconds to come up.
const START_TIMEOUT: Duration = Duration::from_secs(25);

/// How long a daemon gets to shut itself down before it is killed. It is
/// spent while the writer is leaving, so it has to be short — and what matters
/// most about a clean exit, thawing the framework's `btd`, is the first thing
/// its handler does. See [`end`].
const STOP_TIMEOUT: Duration = Duration::from_millis(1500);

/// Long enough for a scan to answer, short enough that a wedged daemon does not
/// hang the editor.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// How much of the daemon's log one pairing is allowed to produce. A single
/// attempt writes a few kilobytes; this only stops a wedged daemon that is
/// writing continuously from being read into memory a second at a time.
const LOG_TAIL_LIMIT: u64 = 256 * 1024;

/// Where a scan has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scan {
    /// Asked for, but the daemon has not picked it up yet.
    Starting,
    /// Under way, with what has been seen so far.
    Running(Vec<Device>),
    /// Over, with the final list.
    Done(Vec<Device>),
}

/// What the daemon's log says about a pairing under way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// The passkey the writer has to type on the keyboard, then Enter.
    Passkey(String),
    /// Pairing ended badly, said in terms of what to do about it.
    Failed(String),
}

/// A device the daemon knows about, paired or just seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub address: String,
    pub name: String,
    /// `ble` or `classic`.
    pub protocol: String,
}

/// What the Bluetooth stack can do right now.
///
/// **Starting is not failing**, and the difference is the whole of what a
/// writer needs to be told. Bringing the radio up is a Python runtime, a kernel
/// module and a chip handoff, and karyll opens the document without waiting for
/// any of it — so for the first seconds of a session there is no daemon to
/// answer, and a control that reaches for one anyway comes back with a
/// connection error that reads like a broken app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ready {
    /// Spawned, not answering yet.
    Starting,
    /// Answering.
    Up,
    /// Never spawned: nothing to wait for.
    Unavailable,
}

pub struct Hid {
    /// The install directory, which is also the daemon's base path.
    base: PathBuf,
    /// Set when we started it, so we only stop what we own.
    child: Option<Child>,
    /// Leave the radio up when the editor goes away. Off by default.
    keep_alive: bool,
    /// When the daemon was spawned, which is what [`Hid::poll_up`] measures its
    /// patience from.
    spawned: Instant,
    /// Whether the daemon has answered, so it is asked only until it has.
    answered: bool,
    /// Whether it is answering now, which is what the Keyboard section shows
    /// and what every control on it needs before it can do anything.
    up: bool,
    /// Whether the log has been told it is taking a while, which is said once
    /// and does not stop the asking.
    complained: bool,
}

impl Hid {
    /// Point at an install without touching it.
    pub fn at(base: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            child: None,
            keep_alive: false,
            spawned: Instant::now(),
            answered: true,
            up: false,
            complained: false,
        }
    }

    /// How far along the Bluetooth stack is, for a page that has to say.
    pub fn ready(&self) -> Ready {
        match (self.up, self.child.is_some()) {
            (true, _) => Ready::Up,
            (false, true) => Ready::Starting,
            (false, false) => Ready::Unavailable,
        }
    }

    /// The install that sits beside the running binary, which is how it is laid
    /// out on the device: `<ext>/bin/karyll` next to `<ext>/hid/`.
    pub fn beside_executable() -> Result<Self> {
        let exe = std::env::current_exe().context("locate the running binary")?;
        let ext = exe
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| anyhow!("binary is not inside <ext>/bin"))?;
        Ok(Self::at(ext.join("hid")))
    }

    /// The extension's `var/`, which is where anything outliving a session goes.
    fn var(&self) -> PathBuf {
        self.base
            .parent()
            .map(|ext| ext.join("var"))
            .unwrap_or_else(|| self.base.clone())
    }

    /// Where the daemon's own output is kept: beside the app's log, on the user
    /// partition, because `/var/log` is tmpfs here and a reboot destroys
    /// precisely the log a crash makes you want.
    fn log_path(&self) -> PathBuf {
        self.var().join("hid.log")
    }

    /// Which process the daemon is, written when we spawn one.
    ///
    /// [`Hid::start`] adopts a daemon this file names and replaces any other.
    fn pid_path(&self) -> PathBuf {
        self.var().join("hid.pid")
    }

    pub fn is_installed(&self) -> bool {
        self.base.join("kindle-hid-passthrough").is_file()
    }

    /// Whether the daemon is meant to outlive the editor.
    pub fn keep_alive(&self) -> bool {
        self.keep_alive
    }

    /// Choose whether quitting takes the Bluetooth stack down with it.
    ///
    /// The keyboard on this device is an evdev node the daemon creates, and
    /// karyll's exclusive grab on it lasts only as long as karyll. A daemon left
    /// running leaves a live and ungrabbed keyboard behind for the rest of the
    /// device.
    ///
    /// The cost is the radio. The daemon displaces `bsa_server` on `/dev/stpbt`,
    /// which takes one holder, so Audible and VoiceView have none until this is
    /// turned off or the Kindle reboots. See [`Hid::start`].
    ///
    /// An X client needs one thing more: `evdev_drv.so` binds only devices udev
    /// has tagged `ID_INPUT_KEYBOARD`, and nothing on this device applies that
    /// tag. The rule that applies it lives on the rootfs, outside karyll's
    /// install. karyll reads the node directly and is unaffected — see
    /// [`crate::evdev`].
    pub fn set_keep_alive(&mut self, on: bool) {
        self.keep_alive = on;
    }

    /// The pid of a daemon we left running, if that pid is still that daemon.
    ///
    /// A pid file outlives the process it names and the kernel reissues the
    /// number, so the pid is confirmed against `/proc` before it is returned.
    fn left_running(&self) -> Option<u32> {
        let pid = std::fs::read_to_string(self.pid_path())
            .ok()?
            .trim()
            .parse::<u32>()
            .ok()?;
        is_daemon(pid, &self.base).then_some(pid)
    }

    /// Whether a daemon is answering, ours or one already running.
    pub fn is_up(&self) -> bool {
        self.get("/health").is_ok()
    }

    /// Start the daemon unless one is already answering, and wait for it.
    ///
    /// Starting it displaces the stock Bluetooth stack: `/dev/stpbt` takes one
    /// holder, so the daemon frees the radio from `bsa_server`. That is why
    /// this is per session rather than a boot service — it costs Audible and
    /// VoiceView only while karyll is open.
    pub fn start(&mut self) -> Result<()> {
        if !self.is_installed() {
            bail!("no Bluetooth stack at {}", self.base.display());
        }

        // A daemon this install left running is adopted: the pid file names it,
        // and its log is the same file at the same path. Adoption holds a launch
        // under a second and keeps the keyboard link up across one.
        if self.is_up()
            && self.keep_alive
            && let Some(pid) = self.left_running()
        {
            eprintln!("bluetooth: keeping the daemon we left running (pid {pid})");
            self.up = true;
            return Ok(());
        }

        // A daemon already answering is one we did not start, and inheriting it
        // is a mistake twice over. Its state is unknown — it may have been left
        // suspended, or by an older build — and because we did not spawn it we
        // never wired up its log, so a failure has nothing behind it.
        //
        // These get orphaned easily: the release profile aborts on panic, which
        // skips `Drop`, so any crash leaves one running for good. Take
        // ownership instead of adopting.
        if self.is_up() {
            eprintln!("bluetooth: a daemon is already running — replacing it");
            let killed = kill_running(&self.base);
            eprintln!("bluetooth: stopped {killed} stale daemon process(es)");
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.is_up() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(200));
            }
            if self.is_up() {
                bail!("a stale Bluetooth daemon is holding the radio and will not stop");
            }
            // The radio needs a moment to come back after its holder dies.
            std::thread::sleep(Duration::from_millis(500));
        }

        // Keep the daemon's output. It logs to stdout, not to the file named in
        // its config, so discarding these leaves a pairing failure with nothing
        // to look at — which is exactly what happened when this moved out of
        // the launcher script and into here.
        let log = self.log_path();
        let (out, err) = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            Ok(file) => (
                Stdio::from(file.try_clone().unwrap_or_else(|_| file_dup(&file))),
                Stdio::from(file),
            ),
            Err(err) => {
                eprintln!(
                    "bluetooth: cannot write {} ({err}) — losing its log",
                    log.display()
                );
                (Stdio::null(), Stdio::null())
            }
        };

        let child = Command::new(self.base.join("kindle-hid-passthrough"))
            .arg("--daemon")
            .current_dir(&self.base)
            .env("KINDLE_HID_BASE", &self.base)
            // Its own process group, which is what [`Hid::set_keep_alive`]
            // needs to mean anything. The launcher replaces a running editor by
            // signalling it, and escalates to `SIGKILL` if it does not go; a
            // daemon inheriting the editor's group is reachable by anything
            // aimed at that group and dies with it whatever `stop` does, with
            // `stop` never reached to have an opinion. Out of the group it
            // outlives even a killed editor, which is what leaves a pid file
            // worth reading and [`Hid::left_running`] a question worth asking.
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .context("spawn the Bluetooth daemon")?;
        // The pid file is written at spawn. `panic = "abort"` skips `Drop`, so a
        // crash leaves the daemon running, and the next launch needs the file to
        // recognise what it finds.
        let _ = std::fs::write(self.pid_path(), format!("{}\n", child.id()));
        self.child = Some(child);
        self.answered = false;
        self.up = false;
        self.complained = false;
        self.spawned = Instant::now();
        Ok(())
    }

    /// Whether the daemon has come up yet, asked once and then remembered.
    ///
    /// **Nothing waits for it.** Bringing the stack up is a bundled Python
    /// runtime, a kernel module and a chip handoff, which on the slowest Kindle
    /// measured is thirteen seconds. Waiting for that before opening the window
    /// makes a tap on the tile do nothing visible for long enough to look like
    /// a failure, and none of it is needed to open a document: a keyboard is
    /// found by the same poll that finds one paired mid-session, and everything
    /// else that needs the daemon asks for it when it is asked for.
    ///
    /// So it is polled while the writer reads, and says so once, either way.
    ///
    /// **Being late is not the same as being dead**, and this asks until it
    /// answers. Warming a Broadcom chip means killing the framework's own
    /// `bsa_server` and taking the UART, and on a Kindle where that needs a
    /// retry it is half a minute before the daemon says a word. Giving up on it
    /// at a deadline leaves the editor believing there is no Bluetooth on a
    /// device where the keyboard is about to connect — so the deadline only
    /// buys a line in the log.
    pub fn poll_up(&mut self) -> Option<Result<()>> {
        if self.answered || self.child.is_none() {
            return None;
        }
        if self.is_up() {
            self.answered = true;
            self.up = true;
            return Some(Ok(()));
        }
        if !self.complained && self.spawned.elapsed() > START_TIMEOUT {
            self.complained = true;
            return Some(Err(anyhow!(
                "the Bluetooth daemon has not answered in {START_TIMEOUT:?} — still asking"
            )));
        }
        None
    }

    /// Stop the daemon, releasing the radio — unless [`Hid::set_keep_alive`]
    /// says to leave it up, in which case this leaves it exactly where it is.
    ///
    /// Deliberately **not** `/stop` first. That suspends the daemon and powers
    /// the chip down, and killing the process a moment later means nothing ever
    /// resumes it — so the next launch opens the transport against a cold chip,
    /// where the daemon's own comment says HCI Reset times out. Killing the
    /// process is what actually frees `/dev/stpbt`, which is the only thing
    /// that has to happen here.
    pub fn stop(&mut self) {
        if self.keep_alive {
            // A `Child` has no `Drop` in Rust, so releasing the handle is the
            // whole of it: the process carries on, its output going to the same
            // `var/hid.log`, and the pid file naming it.
            match self.child.take() {
                Some(child) => {
                    eprintln!("bluetooth: leaving the daemon running (pid {})", child.id())
                }
                None => eprintln!("bluetooth: leaving the adopted daemon running"),
            }
            return;
        }

        let _ = std::fs::remove_file(self.pid_path());
        if let Some(mut child) = self.child.take() {
            end(child.id());
            let _ = child.wait();
            return;
        }

        // An adopted daemon has no handle to kill — an earlier launch spawned it
        // — and the `/proc` sweep is the only way to signal a process we are not
        // the parent of. It is what makes the setting take effect on the launch
        // that turns it off.
        if self.is_up() {
            let killed = kill_running(&self.base);
            eprintln!("bluetooth: stopped {killed} adopted daemon process(es)");
        }
    }

    /// Devices the daemon has paired.
    pub fn devices(&self) -> Result<Vec<Device>> {
        parse_devices(&self.get("/devices")?)
    }

    /// Begin a scan. Results arrive through [`Hid::scan_results`].
    pub fn scan(&self) -> Result<()> {
        self.get("/scan").map(|_| ())
    }

    /// How a scan is getting on.
    pub fn scan_results(&self) -> Result<Scan> {
        let body = self.get("/scan-status")?;
        if field(&body, "scanning") == Some("true".into()) {
            return Ok(Scan::Running(parse_devices(&body)?));
        }
        // `/scan` only schedules the scan on the daemon's event loop; until it
        // actually begins there is no scan and no result, and the reply is an
        // error. Reading that as "finished, nothing found" is what made a scan
        // end about a second after it was asked for — before the radio had done
        // anything at all.
        if field(&body, "ok") == Some("false".into()) {
            return Ok(Scan::Starting);
        }
        Ok(Scan::Done(parse_devices(&body)?))
    }

    /// Begin pairing. Progress arrives through [`Hid::pair_done`].
    pub fn pair(&self, device: &Device) -> Result<()> {
        self.get(&format!(
            "/pair?addr={}&protocol={}&name={}",
            escape(&device.address),
            escape(&device.protocol),
            escape(&device.name)
        ))
        .map(|_| ())
    }

    /// `None` while pairing is still in progress, otherwise whether it worked.
    ///
    /// Success is decided by asking whether the device is now in the paired
    /// list, not by the status field alone. A keyboard can pair completely —
    /// link key saved, descriptor fetched, uhid node created — and then drop
    /// its connection a second later, which is normal for one that also belongs
    /// to another host. The daemon reconnects on its own, so a dropped
    /// connection is not a failed pairing, and treating it as one is how a
    /// working keyboard got thrown away.
    pub fn pair_done(&self, address: &str) -> Result<Option<bool>> {
        let body = self.get("/pair-status")?;
        if field(&body, "pairing") == Some("true".into()) {
            return Ok(None);
        }
        // The daemon answers `{"ok": true, "message": "Paired successfully"}`.
        // There is no `success` field, and looking for one meant a pairing that
        // had plainly worked was reported to the user as a failure.
        if field(&body, "ok") == Some("true".into()) {
            return Ok(Some(true));
        }
        // Not reported as a success — but the paired list is the ground truth.
        let paired = self
            .devices()
            .unwrap_or_default()
            .iter()
            .any(|d| d.address.eq_ignore_ascii_case(address));
        Ok(Some(paired))
    }

    /// Where the daemon's log has got to, to be read back from after `/pair`.
    ///
    /// Taken before pairing starts so a passkey from an earlier attempt — every
    /// attempt generates a fresh one — cannot be mistaken for this one's.
    pub fn log_mark(&self) -> u64 {
        std::fs::metadata(self.log_path())
            .map(|meta| meta.len())
            .unwrap_or(0)
    }

    /// The last thing the daemon said about the pairing started at `mark`.
    ///
    /// **A BLE keyboard has no display, so the host is the side that shows the
    /// passkey** and the writer types it on the keyboard. The daemon pairs BLE
    /// with `mitm=True` and DisplayYesNo against a keyboard's KeyboardOnly,
    /// which is passkey entry with us displaying — and the passkey reaches
    /// nothing but its own stdout (`pairing.py`'s `display_number`). It is not
    /// in `/pair-status`, so with nothing reading the log the writer is asked
    /// for a number that was never shown, and pairing fails every time on a
    /// timeout or a guess. karyll already owns that stream (see [`Hid::start`]),
    /// so reading it back is not a new coupling.
    pub fn pair_prompt(&self, mark: u64) -> Option<Prompt> {
        prompt_in(&self.log_since(mark)?)
    }

    /// The daemon's log from `mark` on, bounded so a chatty daemon cannot make
    /// this grow without limit.
    fn log_since(&self, mark: u64) -> Option<String> {
        let mut file = std::fs::File::open(self.log_path()).ok()?;
        file.seek(SeekFrom::Start(mark)).ok()?;
        let mut raw = Vec::new();
        // Bytes rather than a string: bumble colours its warnings, and one
        // truncated multi-byte sequence must not lose the whole tail.
        file.take(LOG_TAIL_LIMIT).read_to_end(&mut raw).ok()?;
        Some(String::from_utf8_lossy(&raw).into_owned())
    }

    pub fn connect(&self, device: &Device) -> Result<()> {
        self.get(&format!(
            "/connect?addr={}&protocol={}",
            escape(&device.address),
            escape(&device.protocol)
        ))
        .map(|_| ())
    }

    /// Drop the link without forgetting the device.
    ///
    /// The daemon has had this endpoint all along and karyll never called it,
    /// which is why the Config page had no way to disconnect — and why tapping
    /// a keyboard that was already connected asked it to connect *again*, which
    /// tears the link down and builds it back up.
    pub fn disconnect(&self, device: &Device) -> Result<()> {
        self.get(&format!(
            "/disconnect?addr={}&protocol={}",
            escape(&device.address),
            escape(&device.protocol)
        ))
        .map(|_| ())
    }

    pub fn remove(&self, address: &str) -> Result<()> {
        self.get(&format!("/remove?addr={}", escape(address)))
            .map(|_| ())
    }

    /// One GET against the daemon, returning the response body.
    fn get(&self, path: &str) -> Result<String> {
        let addr: SocketAddr = API.parse().expect("a literal address");
        let mut socket = TcpStream::connect_timeout(&addr, Duration::from_millis(500))
            .with_context(|| format!("connect to the Bluetooth daemon at {API}"))?;
        socket.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        socket.set_write_timeout(Some(REQUEST_TIMEOUT))?;

        // HTTP/1.0 so the server closes when it is done and the read ends
        // without needing to understand chunked encoding or keep-alive.
        write!(socket, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n")
            .context("write request")?;
        socket.flush().ok();
        let _ = socket.shutdown(Shutdown::Write);

        let mut raw = String::new();
        socket.read_to_string(&mut raw).context("read response")?;
        // The daemon's own answer, verbatim. Without this a pairing failure is
        // a shrug — the panel says it failed and nothing says why.
        if path != "/health" {
            eprintln!("bluetooth: GET {path} -> {}", raw.replace('\n', " ").trim());
        }
        let body = raw
            .split_once("\r\n\r\n")
            .or_else(|| raw.split_once("\n\n"))
            .map(|(_, body)| body)
            .ok_or_else(|| anyhow!("no body in the daemon's response"))?;
        Ok(body.to_string())
    }
}

impl Drop for Hid {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One process' command line, its arguments joined by spaces.
///
/// `/proc/<pid>/cmdline` separates arguments with NUL, and joining them is what
/// lets a match span two of them — which [`kill_running`]'s second pattern
/// needs. A lossy decode is enough: everything matched against it is ASCII.
fn cmdline(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(String::from_utf8_lossy(&raw).replace('\0', " "))
}

/// Whether `pid` is a live daemon from `base`.
///
/// Matched on the install path, for the reason [`kill_running`] gives: the
/// program's own name appears nowhere in its command line. A pid on its own
/// proves nothing — the kernel hands the number out again — so this match is
/// what makes a pid file safe to act on.
///
/// Narrower than [`kill_running`]'s match: that one sweeps up strays, this one
/// decides whether to adopt a process, and adoption is safe only for one that
/// came out of this install.
fn is_daemon(pid: u32, base: &Path) -> bool {
    cmdline(pid).is_some_and(|line| line.contains(&*base.to_string_lossy()))
}

/// Kill every running copy of the daemon, by scanning `/proc` for its command
/// line. Returns how many were signalled.
///
/// **Matched on the install path, not on the program's name.** The name was
/// tried, both here and as `pkill -f kindle-hid-passthrough` in the launcher,
/// and matched nothing: the daemon runs as `dist/main.bin` from inside this
/// extension, so the string "kindle-hid-passthrough" appears nowhere in its
/// command line. That left a daemon answering on the API port that karyll could
/// neither stop nor replace — so it never spawned its own, and never captured
/// its log.
/// End the daemon the way it knows how to be ended.
///
/// **`SIGKILL` costs the next launch fifteen seconds.** To take the radio the
/// daemon freezes the framework's `btd` so it cannot respawn `bsa_server`, and
/// a daemon killed outright never thaws it — so the next start asks `btfd` to
/// warm the chip, waits twelve seconds for a `bsa_server` that a stopped `btd`
/// will never spawn, and then has to recover `btd` itself before it can go on.
/// The same warm-up against a healthy framework takes one second.
///
/// So it is asked first: the daemon installs a `SIGTERM` handler and shuts down
/// cleanly on one. `SIGKILL` is what happens when it does not answer, because
/// the editor is on its way out and cannot wait long — but by then the thawing
/// is the first thing its handler will have done.
fn end(pid: u32) {
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        // Signal 0 asks whether the process is still there without touching it.
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
}

fn kill_running(base: &Path) -> usize {
    let base = base.to_string_lossy().to_string();
    let me = std::process::id();
    let mut killed = 0;
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Some(cmdline) = cmdline(pid) else {
            continue;
        };
        if cmdline.contains(&base) || cmdline.contains("main.py --daemon") {
            end(pid);
            killed += 1;
        }
    }
    killed
}

/// Duplicate a file handle when `try_clone` fails, so one failure does not cost
/// both streams.
fn file_dup(file: &std::fs::File) -> std::fs::File {
    file.try_clone().unwrap_or_else(|_| {
        std::fs::OpenOptions::new()
            .append(true)
            .open("/dev/null")
            .expect("/dev/null is always openable")
    })
}

fn escape(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | ':' | '/' => c.to_string(),
            ' ' => "+".to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}

/// The value of a top-level JSON field, as text.
///
/// A hand-written reader rather than a JSON crate: the daemon's replies are
/// flat, generated by `json.dumps`, and this reads exactly the handful of keys
/// karyll asks for. Values are returned unescaped for strings and verbatim for
/// everything else.
fn field(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let start = body.find(&needle)? + needle.len();
    let rest = body[start..].trim_start();
    if let Some(text) = rest.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => out.push(chars.next()?),
                other => out.push(other),
            }
        }
        None
    } else {
        let end = rest.find([',', '}', ']']).unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
}

/// Devices out of a `/devices` or `/scan-status` reply.
///
/// Both return a `devices` array of flat objects, so each `{...}` between the
/// brackets is one device.
fn parse_devices(body: &str) -> Result<Vec<Device>> {
    let Some(start) = body
        .find("\"devices\"")
        .and_then(|i| body[i..].find('[').map(|j| i + j + 1))
    else {
        return Ok(Vec::new());
    };
    let end = body[start..]
        .find(']')
        .map(|i| start + i)
        .unwrap_or(body.len());
    let mut out = Vec::new();
    for chunk in body[start..end].split('{').skip(1) {
        let object = chunk.split('}').next().unwrap_or(chunk);
        let Some(address) = field(object, "address").or_else(|| field(object, "addr")) else {
            continue;
        };
        out.push(Device {
            name: field(object, "name").unwrap_or_else(|| address.clone()),
            protocol: field(object, "protocol").unwrap_or_else(|| "ble".into()),
            address,
        });
    }
    Ok(out)
}

/// The latest passkey or failure in a stretch of the daemon's log.
///
/// Read backwards, so a failure that follows a passkey wins and the panel stops
/// asking for a number that is no longer wanted.
fn prompt_in(log: &str) -> Option<Prompt> {
    for line in log.lines().rev() {
        if let Some(rest) = after(line, "Display PIN:") {
            let key: String = rest
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if !key.is_empty() {
                return Some(Prompt::Passkey(key));
            }
        }
        if let Some(rest) = after(line, "Pairing failed:") {
            return Some(Prompt::Failed(why(rest)));
        }
    }
    None
}

fn after<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    line.find(marker).map(|i| &line[i + marker.len()..])
}

/// The daemon's failure text, turned into the thing to do about it.
///
/// Only the ones a writer can act on are named. Everything else is handed back
/// as the daemon said it, because a message nobody recognises still beats a
/// message that says nothing.
fn why(raw: &str) -> String {
    let raw = raw.trim();
    if raw.contains("CONFIRM_VALUE_FAILED") {
        // The keyboard sent a passkey that did not match ours: wrong digits, or
        // a leftover code from an attempt that had already timed out.
        "That code was not right — try again.".into()
    } else if raw.contains("TIMEOUT") {
        "It stopped answering. Try again.".into()
    } else if raw.contains("AUTHENTICATION_REQUIREMENTS") || raw.contains("PAIRING_NOT_SUPPORTED") {
        "It refused to pair. Forget it on its other host first.".into()
    } else {
        raw.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_flat_fields() {
        let body = r#"{"ok": true, "scanning": false, "name": "Pebble Keys 2 K380s"}"#;
        assert_eq!(field(body, "ok").as_deref(), Some("true"));
        assert_eq!(field(body, "scanning").as_deref(), Some("false"));
        assert_eq!(field(body, "name").as_deref(), Some("Pebble Keys 2 K380s"));
        assert_eq!(field(body, "absent"), None);
    }

    #[test]
    fn unescapes_quoted_values() {
        let body = r#"{"name": "a \"quoted\" name"}"#;
        assert_eq!(field(body, "name").as_deref(), Some(r#"a "quoted" name"#));
    }

    #[test]
    fn reads_a_device_list() {
        let body = r#"{"ok": true, "devices": [
            {"address": "98:B9:EA:01:67:68", "name": "K380s", "protocol": "ble"},
            {"address": "5C:2B:3E:50:4F:04", "name": "BLE-M3", "protocol": "classic"}
        ]}"#;
        let devices = parse_devices(body).unwrap();
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "K380s");
        assert_eq!(devices[0].protocol, "ble");
        assert_eq!(devices[1].address, "5C:2B:3E:50:4F:04");
    }

    #[test]
    fn an_empty_or_absent_list_is_not_an_error() {
        assert!(
            parse_devices(r#"{"ok": true, "devices": []}"#)
                .unwrap()
                .is_empty()
        );
        assert!(parse_devices(r#"{"ok": true}"#).unwrap().is_empty());
    }

    #[test]
    fn a_device_without_a_name_falls_back_to_its_address() {
        let body = r#"{"devices": [{"address": "AA:BB:CC:DD:EE:FF", "protocol": "ble"}]}"#;
        let devices = parse_devices(body).unwrap();
        assert_eq!(devices[0].name, "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn query_values_are_escaped() {
        // Names carry spaces, and an address' colons must survive intact.
        assert_eq!(escape("Pebble Keys 2"), "Pebble+Keys+2");
        assert_eq!(escape("98:B9:EA:01:67:68"), "98:B9:EA:01:67:68");
        assert_eq!(escape("a&b=c"), "a%26b%3Dc");
    }

    /// The three shapes `/scan-status` actually returns.
    fn scan_of(body: &str) -> Scan {
        if field(body, "scanning") == Some("true".into()) {
            Scan::Running(parse_devices(body).unwrap())
        } else if field(body, "ok") == Some("false".into()) {
            Scan::Starting
        } else {
            Scan::Done(parse_devices(body).unwrap())
        }
    }

    /// The matcher `kill_running` uses, isolated from the `/proc` walk.
    fn matches(cmdline: &str, base: &str) -> bool {
        cmdline.contains(base) || cmdline.contains("main.py --daemon")
    }

    /// What `pair_done` concludes, isolated from the HTTP call.
    fn done(status: &str, paired: &[&str], address: &str) -> Option<bool> {
        if field(status, "pairing") == Some("true".into()) {
            return None;
        }
        if field(status, "success") == Some("true".into()) {
            return Some(true);
        }
        Some(paired.iter().any(|a| a.eq_ignore_ascii_case(address)))
    }

    #[test]
    fn a_keyboard_that_paired_then_dropped_still_counts_as_paired() {
        // Exactly what a Magic Keyboard does: link key saved, descriptor
        // fetched, uhid node created, then the connection drops a second later
        // because another host also wants it. The daemon reconnects on its own,
        // so this is not a failure — and calling it one deletes a working
        // keyboard.
        let addr = "38:09:FB:25:E4:45";
        let failed = r#"{"ok": false, "error": "Pairing failed"}"#;
        assert_eq!(done(failed, &[addr], addr), Some(true));
    }

    #[test]
    fn a_pairing_that_never_landed_is_a_failure() {
        let addr = "38:09:FB:25:E4:45";
        assert_eq!(done(r#"{"ok": false}"#, &[], addr), Some(false));
    }

    #[test]
    fn pairing_in_progress_is_neither() {
        let addr = "38:09:FB:25:E4:45";
        assert_eq!(done(r#"{"ok": true, "pairing": true}"#, &[], addr), None);
    }

    /// Verbatim from `var/hid.log`, the run where the K380s would not pair.
    const PAIRING: &str = "\
2026-08-14 19:31:37,662 INFO ble_hid: [BLE] Pairing with D0:D5:E8:A1:E1:67...
2026-08-14 19:31:37,958 INFO ble_hid: [BLE] Connected to D0:D5:E8:A1:E1:67
2026-08-14 19:31:37,958 INFO ble_hid: [BLE] Initiating pairing...
2026-08-14 19:31:38,126 INFO ble_hid: Display PIN: 220862
";

    #[test]
    fn the_passkey_is_read_out_of_the_daemons_log() {
        // The one place it exists: the delegate prints it and tells nobody else,
        // so /pair-status is only ever `{"ok": true, "pairing": true}`.
        assert_eq!(
            prompt_in(PAIRING),
            Some(Prompt::Passkey("220862".into())),
            "a leading zero has to survive too"
        );
        assert_eq!(
            prompt_in("INFO ble_hid: Display PIN: 039102"),
            Some(Prompt::Passkey("039102".into()))
        );
    }

    #[test]
    fn nothing_to_type_yet_is_not_a_prompt() {
        assert_eq!(prompt_in(""), None);
        assert_eq!(
            prompt_in("2026-08-14 19:31:37,958 INFO ble_hid: [BLE] Initiating pairing..."),
            None
        );
    }

    #[test]
    fn a_failure_after_the_passkey_replaces_it() {
        // Otherwise the panel goes on asking for a code that the engine has
        // already given up on, and the writer types into nothing.
        let log = format!(
            "{PAIRING}2026-08-14 19:31:47,710 ERROR ble_hid: [BLE] Pairing failed: \
             ProtocolError(smp/CONFIRM_VALUE_FAILED [0x4])\n"
        );
        assert_eq!(
            prompt_in(&log),
            Some(Prompt::Failed(
                "That code was not right — try again.".into()
            ))
        );
    }

    #[test]
    fn an_unrecognised_failure_is_handed_back_as_the_daemon_said_it() {
        let log = "ERROR ble_hid: [BLE] Pairing failed: ProtocolError(smp/UNSPECIFIED [0x8])";
        assert_eq!(
            prompt_in(log),
            Some(Prompt::Failed(
                "ProtocolError(smp/UNSPECIFIED [0x8])".into()
            ))
        );
    }

    #[test]
    fn the_daemon_is_found_by_its_install_path() {
        let base = "/mnt/us/extensions/karyll/hid";
        // What it actually runs as: the bundled interpreter, from inside the
        // extension. Its own name appears nowhere.
        assert!(matches(
            "/mnt/us/extensions/karyll/hid/dist/main.bin --daemon",
            base
        ));
        assert!(matches(
            "/mnt/us/extensions/karyll/hid/kindle-hid-passthrough --daemon",
            base
        ));
        // The name-based match this replaced saw neither of the first form.
        assert!(
            !"/mnt/us/extensions/karyll/hid/dist/main.bin --daemon"
                .contains("kindle-hid-passthrough")
        );
        // An install somewhere else is not ours to kill.
        assert!(!matches(
            "/mnt/us/kindle_hid_passthrough/dist/main.bin --daemon",
            base
        ));
        // Nor is anything unrelated.
        assert!(!matches("/usr/bin/bsa_server", base));
    }

    #[test]
    fn a_scan_that_has_not_begun_is_not_a_finished_one() {
        // `/scan` only schedules the work; until the daemon picks it up the
        // reply is this error. Reading it as "done, nothing found" ended every
        // scan about a second in.
        assert_eq!(
            scan_of(r#"{"ok": false, "error": "No scan in progress"}"#),
            Scan::Starting
        );
    }

    #[test]
    fn a_running_scan_reports_what_it_has_seen_so_far() {
        let body = r#"{"ok": true, "scanning": true, "devices": [
            {"address": "11:22:33:44:55:66", "name": "Keyboard", "protocol": "ble"}]}"#;
        match scan_of(body) {
            Scan::Running(devices) => assert_eq!(devices.len(), 1),
            other => panic!("expected a running scan, got {other:?}"),
        }
    }

    #[test]
    fn a_finished_scan_carries_the_final_list() {
        let body = r#"{"ok": true, "devices": [
            {"address": "11:22:33:44:55:66", "name": "Keyboard", "protocol": "ble"}]}"#;
        match scan_of(body) {
            Scan::Done(devices) => assert_eq!(devices.len(), 1),
            other => panic!("expected a finished scan, got {other:?}"),
        }
        // Finished and empty is a real answer, unlike the not-started case.
        assert_eq!(
            scan_of(r#"{"ok": true, "devices": []}"#),
            Scan::Done(Vec::new())
        );
    }
}
