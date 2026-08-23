//! Driving the Bluetooth daemon over its HTTP API on `127.0.0.1:8321`:
//! `/health`, `/scan`, `/pair`, `/devices`, `/status`. Liveness is asked over
//! the socket, which is the one that conflicts when two daemons run.

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

/// How long a daemon gets to shut itself down before `SIGKILL`. Thawing `btd`
/// is the first thing its `SIGTERM` handler does. See [`end`].
const STOP_TIMEOUT: Duration = Duration::from_millis(1500);

/// Long enough for a scan to answer, short enough that a wedged daemon does not
/// hang the editor.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// How much of the daemon's log one pairing may produce. An attempt writes a
/// few kilobytes; a wedged daemon writes without stopping.
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

/// Whether two addresses name one keyboard. `/devices` stores an address bare;
/// `/scan-status` appends the address type.
pub fn same_address(a: &str, b: &str) -> bool {
    bare(a).eq_ignore_ascii_case(bare(b))
}

/// The address without the address type appended to it.
fn bare(address: &str) -> &str {
    address.split('/').next().unwrap_or(address)
}

/// What the Bluetooth stack can do. `Starting` and `Unavailable` are separate
/// states: bringing the radio up runs a Python runtime, a kernel module and a
/// chip handoff, and the editor opens ahead of all of it.
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
    /// Set on a spawn from this process. `stop` acts on this handle alone.
    child: Option<Child>,
    /// Leave the radio up when the editor goes away. Off by default.
    keep_alive: bool,
    /// When the daemon was spawned. [`Hid::poll_up`] measures from here.
    spawned: Instant,
    /// Whether the daemon has answered. `poll_up` asks until it has.
    answered: bool,
    /// Whether the daemon is answering. `ready` reports it.
    up: bool,
    /// Whether the log carries the slow-start line, said once.
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

    /// How far along the Bluetooth stack is.
    pub fn ready(&self) -> Ready {
        match (self.up, self.child.is_some()) {
            (true, _) => Ready::Up,
            (false, true) => Ready::Starting,
            (false, false) => Ready::Unavailable,
        }
    }

    /// The install beside the running binary: `<ext>/bin/karyll` next to
    /// `<ext>/hid/`.
    pub fn beside_executable() -> Result<Self> {
        let exe = std::env::current_exe().context("locate the running binary")?;
        let ext = exe
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| anyhow!("binary is not inside <ext>/bin"))?;
        Ok(Self::at(ext.join("hid")))
    }

    /// The extension's `var/`, holding anything that outlives a session.
    fn var(&self) -> PathBuf {
        self.base
            .parent()
            .map(|ext| ext.join("var"))
            .unwrap_or_else(|| self.base.clone())
    }

    /// The daemon's output, kept beside the app's log on the user partition.
    /// `/var/log` is tmpfs here and a reboot empties it.
    fn log_path(&self) -> PathBuf {
        self.var().join("hid.log")
    }

    /// The spawned daemon's pid. [`Hid::start`] adopts a daemon this file
    /// names and replaces any other.
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
    /// A daemon left running leaves its uhid keyboard node live and ungrabbed,
    /// and holds `/dev/stpbt` away from `bsa_server`. See [`Hid::start`].
    pub fn set_keep_alive(&mut self, on: bool) {
        self.keep_alive = on;
    }

    /// The pid in `pid_path`, confirmed against `/proc`. A pid file outlives
    /// the process it names and the kernel reissues the number.
    fn left_running(&self) -> Option<u32> {
        let pid = std::fs::read_to_string(self.pid_path())
            .ok()?
            .trim()
            .parse::<u32>()
            .ok()?;
        is_daemon(pid, &self.base).then_some(pid)
    }

    /// Whether any daemon is answering on [`PORT`].
    pub fn is_up(&self) -> bool {
        self.get("/health").is_ok()
    }

    /// Start the daemon, unless one is answering, and wait for it. The daemon
    /// takes `/dev/stpbt` from `bsa_server`, which holds it one at a time.
    pub fn start(&mut self) -> Result<()> {
        if !self.is_installed() {
            bail!("no Bluetooth stack at {}", self.base.display());
        }

        // A daemon `pid_path` names is adopted; its log is the same file at
        // the same path, and the keyboard link holds across the launch.
        if self.is_up()
            && self.keep_alive
            && let Some(pid) = self.left_running()
        {
            eprintln!("bluetooth: keeping the daemon we left running (pid {pid})");
            self.up = true;
            return Ok(());
        }

        // A daemon this install did not spawn carries unknown state and an
        // unwired log. `panic = "abort"` skips `Drop`, and a crash orphans one.
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

        // The daemon logs to stdout, not to the file named in its config.
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
            // Its own process group. Outside the editor's group the daemon
            // outlives a signal aimed at that group, which is what
            // [`Hid::set_keep_alive`] and [`Hid::left_running`] rest on.
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .context("spawn the Bluetooth daemon")?;
        // The pid file is written at spawn. `panic = "abort"` skips `Drop`,
        // and the next launch reads this file to recognise what it finds.
        let _ = std::fs::write(self.pid_path(), format!("{}\n", child.id()));
        self.child = Some(child);
        self.answered = false;
        self.up = false;
        self.complained = false;
        self.spawned = Instant::now();
        Ok(())
    }

    /// Whether the daemon has come up, asked once and then remembered. Nothing
    /// blocks on it, and it is asked until it answers: warming the chip takes
    /// thirteen seconds, and half a minute where it needs a retry.
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

    /// Stop the daemon, releasing the radio, unless [`Hid::set_keep_alive`]
    /// holds it up. Killing the process is what frees `/dev/stpbt`; `/stop`
    /// powers the chip down and leaves the next launch a cold transport.
    pub fn stop(&mut self) {
        if self.keep_alive {
            // A `Child` has no `Drop`. The process carries on, its output
            // going to `var/hid.log` and the pid file naming it.
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

        // An adopted daemon has no handle. `kill_running` sweeps `/proc` for a
        // process this one did not parent.
        if self.is_up() {
            let killed = kill_running(&self.base);
            eprintln!("bluetooth: stopped {killed} adopted daemon process(es)");
        }
    }

    /// Every keyboard the daemon has paired, connected or not. The daemon's
    /// config read back, which holds across restarts.
    pub fn devices(&self) -> Result<Vec<Device>> {
        parse_devices(&self.get("/devices")?)
    }

    /// Which keyboard holds the link. One at a time: the daemon waits on every
    /// remembered device and keeps the first connection, behind one uhid node
    /// that [`Hid::devices`] reports the same for all of them.
    pub fn connected(&self) -> Option<String> {
        let body = self.get("/status").ok()?;
        let address = field(&body, "connected_device")?;
        (!address.is_empty() && address != "null").then_some(address)
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
        // `/scan` schedules the scan on the daemon's event loop. Until it
        // begins the reply is an error, distinct from an empty result.
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

    /// `None` while pairing runs, then whether it worked. Success is the
    /// device appearing in the paired list. A keyboard shared with another
    /// host pairs completely and drops the link a second later.
    pub fn pair_done(&self, address: &str) -> Result<Option<bool>> {
        let body = self.get("/pair-status")?;
        if field(&body, "pairing") == Some("true".into()) {
            return Ok(None);
        }
        // The daemon answers `{"ok": true, "message": "Paired successfully"}`.
        // There is no `success` field.
        if field(&body, "ok") == Some("true".into()) {
            return Ok(Some(true));
        }
        // The paired list decides, whatever the message says.
        let paired = self
            .devices()
            .unwrap_or_default()
            .iter()
            .any(|d| same_address(&d.address, address));
        Ok(Some(paired))
    }

    /// Where the daemon's log stands, taken before `/pair`. Every attempt
    /// generates a fresh passkey, and `pair_progress` reads from here on.
    pub fn log_mark(&self) -> u64 {
        std::fs::metadata(self.log_path())
            .map(|meta| meta.len())
            .unwrap_or(0)
    }

    /// The last thing the daemon said about the pairing started at `mark`.
    ///
    /// A BLE keyboard has no display: the daemon pairs with `mitm=True` and
    /// DisplayYesNo against a keyboard's KeyboardOnly, and prints the passkey
    /// to its own stdout alone. `/pair-status` does not carry it.
    pub fn pair_prompt(&self, mark: u64) -> Option<Prompt> {
        prompt_in(&self.log_since(mark)?)
    }

    /// The daemon's log from `mark` on, bounded by [`PAIR_LOG_BYTES`].
    fn log_since(&self, mark: u64) -> Option<String> {
        let mut file = std::fs::File::open(self.log_path()).ok()?;
        file.seek(SeekFrom::Start(mark)).ok()?;
        let mut raw = Vec::new();
        // Bytes, not a string: a truncated multi-byte sequence keeps the tail.
        file.take(LOG_TAIL_LIMIT).read_to_end(&mut raw).ok()?;
        Some(String::from_utf8_lossy(&raw).into_owned())
    }

    /// Drop the link, keeping the pairing and the link key. The daemon waits
    /// on every remembered device and reconnects five seconds later to
    /// whichever is awake.
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

        // HTTP/1.0: the server closes when it is done and the read ends with
        // no chunked encoding and no keep-alive to parse.
        write!(socket, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n")
            .context("write request")?;
        socket.flush().ok();
        let _ = socket.shutdown(Shutdown::Write);

        let mut raw = String::new();
        socket.read_to_string(&mut raw).context("read response")?;
        // The daemon's own answer, verbatim.
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
/// `/proc/<pid>/cmdline` separates them with NUL, and a joined string lets a
/// match span two. Everything matched against it is ASCII.
fn cmdline(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(String::from_utf8_lossy(&raw).replace('\0', " "))
}

/// Whether `pid` is a live daemon from `base`, matched on the install path.
/// A pid alone proves nothing; the kernel hands the number out again.
/// [`kill_running`] matches wider.
fn is_daemon(pid: u32, base: &Path) -> bool {
    cmdline(pid).is_some_and(|line| line.contains(&*base.to_string_lossy()))
}

/// Kill every running copy of the daemon by scanning `/proc` for its command
/// line, returning how many were signalled. The match is on the install path:
/// the daemon runs as `dist/main.bin` and its own name appears nowhere.
/// End the daemon: `SIGTERM`, then `SIGKILL` after [`STOP_GRACE`]. The daemon
/// freezes `btd` to take the radio and thaws it from its `SIGTERM` handler.
/// A `SIGKILL` leaves `btd` frozen and costs the next launch fifteen seconds.
fn end(pid: u32) {
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        // Signal 0 asks whether the process is there without touching it.
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

/// Duplicate a file handle when `try_clone` fails, keeping the other stream.
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

/// The value of a top-level JSON field, as text. The daemon's replies are flat
/// `json.dumps` output. Strings come back unescaped, everything else verbatim.
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

/// Devices out of a `/devices` or `/scan-status` reply. Both carry a `devices`
/// array of flat objects, one device per `{...}`.
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

/// The latest passkey or failure in a stretch of the daemon's log. Read
/// backwards: a failure following a passkey wins.
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

/// The daemon's failure text, turned into the thing to do about it. The
/// messages named here are the actionable ones; the rest pass through.
fn why(raw: &str) -> String {
    let raw = raw.trim();
    if raw.contains("CONFIRM_VALUE_FAILED") {
        // The keyboard sent a passkey that did not match: wrong digits, or a
        // code left over from a timed-out attempt.
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
    fn a_scan_result_and_a_paired_device_are_one_keyboard() {
        // The suffix is the address type, printed by one endpoint of the two.
        assert!(same_address("AA:BB:CC:DD:EE:FF/P", "AA:BB:CC:DD:EE:FF"));
        assert!(same_address("aa:bb:cc:dd:ee:ff", "AA:BB:CC:DD:EE:FF/P"));
        assert!(!same_address("AA:BB:CC:DD:EE:FF", "11:22:33:44:55:66"));
    }

    /// What [`Hid::connected`] reads out of a `/status` reply.
    fn connected_in(body: &str) -> Option<String> {
        let address = field(body, "connected_device")?;
        (!address.is_empty() && address != "null").then_some(address)
    }

    #[test]
    fn a_daemon_with_no_link_names_no_keyboard() {
        // `devices` holds every remembered keyboard; this field names the one
        // holding the link.
        let idle = r#"{"ok": true, "connected": false, "connected_device": null,
            "devices": [{"address": "AA:BB:CC:DD:EE:FF", "protocol": "ble"}]}"#;
        assert_eq!(connected_in(idle), None);

        let live = r#"{"ok": true, "connected": true,
            "connected_device": "11:22:33:44:55:66", "connected_protocol": "classic"}"#;
        assert_eq!(connected_in(live).as_deref(), Some("11:22:33:44:55:66"));
    }

    #[test]
    fn query_values_are_escaped() {
        // Names carry spaces, and an address carries colons.
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
        // A keyboard shared with another host: link key saved, descriptor
        // fetched, uhid node created, connection dropped a second later. The
        // daemon reconnects on its own.
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

    /// Verbatim from one `var/hid.log`, over a failed BLE pairing.
    const PAIRING: &str = "\
2026-08-14 19:31:37,662 INFO ble_hid: [BLE] Pairing with D0:D5:E8:A1:E1:67...
2026-08-14 19:31:37,958 INFO ble_hid: [BLE] Connected to D0:D5:E8:A1:E1:67
2026-08-14 19:31:37,958 INFO ble_hid: [BLE] Initiating pairing...
2026-08-14 19:31:38,126 INFO ble_hid: Display PIN: 220862
";

    #[test]
    fn the_passkey_is_read_out_of_the_daemons_log() {
        // The one place the passkey appears. `/pair-status` carries only
        // `{"ok": true, "pairing": true}`.
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
        // A failure after a passkey wins, and the panel stops asking.
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
        // The bundled interpreter, from inside the extension. The daemon's own
        // name appears nowhere in it.
        assert!(matches(
            "/mnt/us/extensions/karyll/hid/dist/main.bin --daemon",
            base
        ));
        assert!(matches(
            "/mnt/us/extensions/karyll/hid/kindle-hid-passthrough --daemon",
            base
        ));
        // A match on the binary's name sees neither of the first form.
        assert!(
            !"/mnt/us/extensions/karyll/hid/dist/main.bin --daemon"
                .contains("kindle-hid-passthrough")
        );
        // An install under another path.
        assert!(!matches(
            "/mnt/us/kindle_hid_passthrough/dist/main.bin --daemon",
            base
        ));
        // Nor is anything unrelated.
        assert!(!matches("/usr/bin/bsa_server", base));
    }

    #[test]
    fn a_scan_that_has_not_begun_is_not_a_finished_one() {
        // `/scan` schedules the work; the reply is this error until the daemon
        // picks it up.
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
