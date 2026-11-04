// `mobax mcp` — a Model Context Protocol server on stdio, so MCP hosts (Claude
// Desktop, Claude Code, the .mcpb bundle, …) can drive a device with the
// same verbs as the CLI.
//
// Transport: newline-delimited JSON-RPC 2.0 on stdin/stdout; logs go to
// stderr. Each tool call runs on its own thread, so a long `wait` never
// holds up a `ui` or a `screenshot`.
//
// Tool calls run the CLI's verbs in this process on warm sockets: the
// server keeps a small pool of connections per device, opened through the
// adb server itself (no host port per device), and reuses them. Actions on
// one device run one at a time; after each, the server waits for the screen
// to settle and replies with what changed (see `mcp_screen`). Lifecycle
// calls (`use`, `drop`) and the catch-all `mobax` tool still run this binary
// as a child process: they are rare, and some leave helpers running.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::json::{self, Value};
use crate::json_out::encode_str;
use crate::mcp_screen::{Change, Screen};
use crate::session::Session;
use crate::{output, Conn};
use crate::Connect;

/// Protocol revisions this server speaks; the newest is offered when the
/// client asks for one we don't know.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Upper bound for one child-process tool call (a verb's own --timeout is
/// usually far lower); stops a stuck child from wedging the server.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// First line of `mobax --help`, printed after argument errors.
const USAGE_HEAD: &str = "mobax — drive Android from the shell.";

/// Verbs the generic `mobax` tool refuses: interactive or never-ending, or
/// they would open a window or recurse.
const BLOCKED_VERBS: &[&str] = &["mcp", "shell", "tui", "mirror", "fan", "run"];

/// Warm sockets kept per device between calls.
const POOL_PER_DEVICE: usize = 4;

const INSTRUCTIONS: &str = "Drive an Android device or emulator. Call `use` first \
to connect and start the on-device daemon.\n\
\n\
`ui` lists the screen as rows numbered @1, @2, …: verb (tap / fill / - for text), \
class, label, #resource-id, centre x,y and state. Act on a row by its ref \
(tap target \"@3\", fill selector \"@5\"), or by text, #resource_id or a CSS-like \
selector ('Button[text=\"OK\"]').\n\
\n\
`tap`, `fill`, `type`, `swipe`, `go` and `open` wait for the screen to settle and \
reply with what changed: + added, ~ changed and - removed rows, or the whole new \
screen. You rarely need `ui` after an action. A ref stays valid until a reply \
shows a new screen.\n\
\n\
Every tool takes an optional `device` (adb serial; default: the one `use` \
connected). `observe: false` skips an action's settle and reply. \
`timeout_ms` defaults to 10000.\n\
\n\
When you already know several steps (tap a field, fill it, press enter, wait for \
the result), send them as one `batch`: one call instead of one per step.\n\
\n\
To wait for the screen, use `watch` (any change, or `until` some text shows) or \
`wait` (a selector) instead of calling `ui` repeatedly; the device pushes \
changes, so waiting is free. Use `screenshot` only when the rows don't tell \
you enough (an image costs far more). \
An error starts with ERR:CODE and says what to do next. If `use` finds no device, \
an emulator can be started on the host with `android emulator start NAME` (give \
the AVD hw.gpu.mode=host).";

/// `mobax mcp [GLOBAL OPTIONS]`: serves until stdin closes. The options
/// (e.g. `--device SERIAL`, `--port N`) apply to every tool call.
pub(crate) fn serve(global: &[String]) -> io::Result<()> {
    let engine = Arc::new(Engine::new(global));
    let out = Arc::new(Mutex::new(io::stdout()));
    let workers = Workers::new(WORKERS);
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if line.contains("\"tools/call\"") {
            let (engine, out) = (engine.clone(), out.clone());
            workers.spawn(move || {
                if let Some(reply) = handle(&line, &engine) {
                    send(&out, &reply);
                }
            });
        } else if let Some(reply) = handle(&line, &engine) {
            send(&out, &reply);
        }
    }
    // stdin closed: let calls in flight finish and reply.
    workers.join();
    Ok(())
}

/// Tool calls running at once; more wait in the queue. A blocked `wait`
/// holds one, so this bounds how many devices can be waited on together.
const WORKERS: usize = 8;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Where tool calls run. A fixed set of threads takes jobs from a queue;
/// callers only `spawn`, so how calls are scheduled (threads today, an async
/// runtime if this ever drives far more devices) stays in this one type.
struct Workers {
    queue: Option<std::sync::mpsc::Sender<Job>>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Workers {
    fn new(n: usize) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let threads = (0..n)
            .map(|i| {
                let rx = rx.clone();
                std::thread::Builder::new()
                    .name(format!("mcp-{i}"))
                    .spawn(move || loop {
                        // Hold the queue only to take a job, not to run it.
                        let job = lock(&rx).recv();
                        match job {
                            Ok(job) => job(),
                            Err(_) => return,
                        }
                    })
                    .expect("spawn mcp worker")
            })
            .collect();
        Workers { queue: Some(tx), threads }
    }

    fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        if let Some(q) = &self.queue {
            let _ = q.send(Box::new(job));
        }
    }

    /// Runs what is queued, then stops the threads.
    fn join(mut self) {
        self.queue = None;
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// One reply line on stdout; replies from parallel calls never interleave.
fn send(out: &Mutex<io::Stdout>, reply: &str) {
    let mut o = lock(out);
    let _ = o.write_all(reply.as_bytes());
    let _ = o.write_all(b"\n");
    let _ = o.flush();
}

/// One JSON-RPC message in, at most one reply out (none for notifications).
fn handle(line: &str, engine: &Engine) -> Option<String> {
    let msg = match json::parse(line) {
        Ok(v) => v,
        Err(e) => return Some(error_reply("null", -32700, &format!("parse error: {e}"))),
    };
    let id = json::obj_get(&msg, "id").map(to_json);
    let method = json::obj_get(&msg, "method").and_then(json::as_str).unwrap_or("");
    let params = json::obj_get(&msg, "params");
    // Notifications (no id) get no reply.
    let id = id?;
    let result = match method {
        "initialize" => Ok(initialize(params)),
        "ping" => Ok("{}".to_string()),
        "tools/list" => Ok(tools_list()),
        "tools/call" => tools_call(params, engine),
        "" => Err((-32600, "invalid request: no method".to_string())),
        m => Err((-32601, format!("method not found: {m}"))),
    };
    Some(match result {
        Ok(r) => format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{r}}}"#),
        Err((code, msg)) => error_reply(&id, code, &msg),
    })
}

fn error_reply(id: &str, code: i32, msg: &str) -> String {
    let mut m = String::new();
    encode_str(&mut m, msg);
    format!(r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":{code},"message":{m}}}}}"#)
}

fn initialize(params: Option<&Value>) -> String {
    let asked = params
        .and_then(|p| json::obj_get(p, "protocolVersion"))
        .and_then(json::as_str)
        .unwrap_or("");
    let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOL_VERSIONS[0]);
    let mut instructions = String::new();
    encode_str(&mut instructions, INSTRUCTIONS);
    format!(
        r#"{{"protocolVersion":"{version}","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"mobileax","version":"{}"}},"instructions":{instructions}}}"#,
        env!("CARGO_PKG_VERSION")
    )
}

// ---------- in-process execution ----------

/// A device daemon: its serial and the daemon instance (`mobaxd_<port>`).
#[derive(Clone, PartialEq, Eq, Hash)]
struct Target {
    serial: String,
    port: u16,
}

struct Engine {
    /// Options from the `mobax mcp` command line, passed to child calls.
    global: Vec<String>,
    host: String,
    port: u16,
    device: Option<String>,
    targets: Mutex<HashMap<Option<String>, Target>>,
    pool: Mutex<HashMap<Target, Vec<Session>>>,
    screens: Mutex<HashMap<Target, Screen>>,
    /// One action at a time per device.
    locks: Mutex<HashMap<Target, Arc<Mutex<()>>>>,
    /// One daemon start at a time per device.
    starting: Mutex<HashMap<Target, Arc<Mutex<()>>>>,
}

/// A tool's text reply and whether it succeeded.
type Reply = (bool, String);

impl Engine {
    fn new(global: &[String]) -> Self {
        let (mut host, mut port, mut device) = ("127.0.0.1".to_string(), crate::daemon::DEFAULT_PORT, None);
        let mut it = global.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "-s" | "--device" => device = it.next().cloned(),
                "--port" => port = it.next().and_then(|p| p.parse().ok()).unwrap_or(port),
                "--host" => host = it.next().cloned().unwrap_or(host),
                _ => {}
            }
        }
        Engine {
            global: global.to_vec(),
            host,
            port,
            device,
            targets: Mutex::default(),
            pool: Mutex::default(),
            screens: Mutex::default(),
            locks: Mutex::default(),
            starting: Mutex::default(),
        }
    }

    /// In-process calls reach the daemon through the local adb server; a
    /// remote `--host` goes through child processes as before.
    fn in_process(&self) -> bool {
        crate::is_local(&self.host)
    }

    fn target(&self, device: Option<&str>) -> Result<Target, String> {
        let key = device.map(str::to_string).or_else(|| self.device.clone());
        if let Some(t) = lock(&self.targets).get(&key) {
            return Ok(t.clone());
        }
        let t = match &key {
            Some(serial) => Target { serial: serial.clone(), port: self.port },
            None => Target {
                serial: crate::daemon::target_serial()
                    .map_err(|e| format!("ERR:{}:{e}\nPass `device`, or call `use` first.", e.code().as_short()))?,
                port: self.port,
            },
        };
        lock(&self.targets).insert(key, t.clone());
        Ok(t)
    }

    /// Drops everything known about devices (after `use` / `drop`).
    fn forget(&self) {
        lock(&self.targets).clear();
        lock(&self.pool).clear();
        lock(&self.screens).clear();
    }

    fn checkout(&self, t: &Target) -> Result<Session, String> {
        loop {
            let pooled = lock(&self.pool).get_mut(t).and_then(Vec::pop);
            match pooled {
                Some(mut s) if s.conn.is_alive() => {
                    s.invalidate_dump();
                    return Ok(s);
                }
                Some(_) => continue,
                None => break,
            }
        }
        let conn = match Conn::connect_device(&t.serial, t.port) {
            Ok(c) => c,
            // The device is there but its daemon isn't: never started, or the
            // device was reset under us (a snapshot load, a reboot). Start it
            // and carry on; screens seen before are stale.
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                self.start_daemon(t).map_err(|e| {
                    format!("ERR:DEVICE_GONE:no daemon on {} and starting one failed ({e})\nCall `use`.", t.serial)
                })?;
                Conn::connect_device(&t.serial, t.port)
                    .map_err(|e| format!("ERR:DEVICE_GONE:{} ({e})\nCall `use`.", t.serial))?
            }
            Err(e) => return Err(format!("ERR:DEVICE_GONE:cannot reach {} ({e})\nCall `use` with an attached device.", t.serial)),
        };
        Ok(Session::with_conn(conn, "127.0.0.1", t.port))
    }

    /// Starts the daemon on `t` once, however many calls find it missing at
    /// the same time.
    fn start_daemon(&self, t: &Target) -> io::Result<()> {
        let guard = lock(&self.starting).entry(t.clone()).or_default().clone();
        let _one = lock(&guard);
        // Another call may have started it while this one waited.
        if crate::daemon::ping_daemon(&t.serial, t.port).is_ok() {
            return Ok(());
        }
        crate::daemon::connect(&crate::daemon::ConnectOpts { serial: Some(t.serial.clone()), port: Some(t.port) })?;
        lock(&self.screens).remove(t);
        lock(&self.pool).remove(t);
        Ok(())
    }

    fn checkin(&self, t: &Target, s: Session) {
        let mut pool = lock(&self.pool);
        let idle = pool.entry(t.clone()).or_default();
        if idle.len() < POOL_PER_DEVICE {
            idle.push(s);
        }
    }

    fn action_lock(&self, t: &Target) -> Arc<Mutex<()>> {
        lock(&self.locks).entry(t.clone()).or_default().clone()
    }

    /// Runs one CLI verb on a pooled socket.
    fn run(&self, device: Option<&str>, argv: &[String]) -> Reply {
        let t = match self.target(device) {
            Ok(t) => t,
            Err(e) => return (false, e),
        };
        let mut sess = match self.checkout(&t) {
            Ok(s) => s,
            Err(e) => return (false, e),
        };
        let r = run_verb(&t.serial, &mut sess, argv);
        if r.keep {
            self.checkin(&t, sess);
        }
        (r.ok, r.text)
    }

    /// Runs an action verb, then (with `observe`) waits for the screen to
    /// settle and appends what changed, or with `brief` only how it settled
    /// (a `batch` step: the batch describes the screen once, at the end).
    fn act(&self, device: Option<&str>, argv: &[String], observe: bool, brief: bool) -> Reply {
        let t = match self.target(device) {
            Ok(t) => t,
            Err(e) => return (false, e),
        };
        let guard = self.action_lock(&t);
        let _one_at_a_time = lock(&guard);
        let mut sess = match self.checkout(&t) {
            Ok(s) => s,
            Err(e) => return (false, e),
        };
        let seq = if observe { event_seq(&mut sess.conn) } else { None };
        let verb_started = Instant::now();
        let mut r = run_verb(&t.serial, &mut sess, argv);
        let verb_ms = verb_started.elapsed().as_millis();
        if r.ok && observe {
            let mut budget = settle_budget(argv.first().map(String::as_str).unwrap_or(""));
            // START_DELIVERED_TO_TOP: the app was already in front; nothing
            // is coming, so don't wait seconds for it.
            if argv.first().map(String::as_str) == Some("open") && r.text.contains("result=3") {
                budget.first_ms = 300;
            }
            let expect = if argv.first().map(String::as_str) == Some("open") { started_pkg(&r.text) } else { None };
            match self.observe_change(&t, &mut sess.conn, seq, budget, verb_ms, expect.as_deref()) {
                Ok((note, _)) if brief => r.text.push_str(&format!("\n{note}")),
                Ok((_, change)) => r.text.push_str(&format!("\n{change}")),
                Err(e) => {
                    r.text.push_str(&format!("\n(could not read the screen afterwards: {e})"));
                    r.keep = false;
                }
            }
            // Toasts leave no rows: report the ones this action raised.
            if let (true, Some(since)) = (r.keep, seq) {
                if let Ok(body) = sess.conn.call(&format!("toasts since={since}")) {
                    r.text.push_str(&toast_lines(&String::from_utf8_lossy(&body)));
                }
            }
        }
        if r.keep {
            self.checkin(&t, sess);
        }
        (r.ok, r.text)
    }

    /// Waits for the action that followed `seq` to settle, reads the screen,
    /// and describes it against the last one this server saw: (how it
    /// settled, that plus what changed).
    /// [expect]: the package an `open` started; while the screen from
    /// before it is still in front, the launch hasn't arrived.
    fn observe_change(
        &self, t: &Target, conn: &mut Conn, seq: Option<u64>, b: Settle, verb_ms: u128, expect: Option<&str>,
    ) -> io::Result<(String, String)> {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(b.timeout_ms as u64);
        // MOBAX_TIMING=1: where the time went, appended to the note.
        let mut parts = vec![format!("verb {verb_ms}")];
        let mut lap = Instant::now();
        let mut mark = |what: &str, parts: &mut Vec<String>| {
            parts.push(format!("{what} {}", lap.elapsed().as_millis()));
            lap = Instant::now();
        };
        let (had_rows, before) = {
            let screens = lock(&self.screens);
            let prev = screens.get(t);
            (prev.is_some_and(|s| !s.is_empty()), prev.map(|s| s.pkg.clone()))
        };
        let loading = |dump: &Value| {
            transitional(dump, had_rows) || still_before(before.as_deref(), &Screen::top_pkg(dump), expect)
        };
        // The daemon settles and hands back the screen once it holds still,
        // in one round trip; an older one gets the steps one by one.
        let settled = match seq {
            Some(since) => settle_dump(conn, since, b.first_ms, b)?,
            None => None,
        };
        let (mut reacted, still_moving, mut dump) = match settled {
            Some(s) => {
                mark("settle", &mut parts);
                s
            }
            None => self.observe_stepwise(t, conn, seq, b, had_rows, deadline, &mut parts)?,
        };
        // A tap that starts another activity can go quiet before the new
        // screen draws (another process starting). If nothing visible changed,
        // give it one more short window to show up.
        if b.second_look_ms > 0 && self.unchanged(t, &dump) {
            if let Some(since) = event_seq(conn) {
                if let Some((true, _, d)) = settle_dump(conn, since, b.second_look_ms, b)? {
                    reacted = true;
                    dump = d;
                }
            }
            mark("second look", &mut parts);
        }
        // A launching app shows its starting window first: wait for content,
        // longer than the settle itself (a cold start under load is slow).
        // After the second look, which can catch a new screen's first, empty frame.
        let launch_deadline = started + Duration::from_millis(b.timeout_ms.max(b.launch_ms) as u64);
        while loading(&dump) && Instant::now() < launch_deadline {
            let Some(since) = event_seq(conn) else { break };
            match settle_dump(conn, since, 300, b)? {
                Some((_, _, d)) => dump = d,
                None => break,
            }
            mark("launch", &mut parts);
        }
        let note = settle_note(started.elapsed().as_millis(), still_moving, reacted, loading(&dump));
        let note = if std::env::var_os("MOBAX_TIMING").is_some() {
            format!("{note} [{}]", parts.join(" · "))
        } else {
            note
        };
        let mut screens = lock(&self.screens);
        let prev = screens.get(t);
        let screen = Screen::from_dump(&dump, prev);
        let out = screen.describe(&note, screen.change_from(prev));
        screens.insert(t.clone(), screen);
        Ok((note, out))
    }

    /// `observe_change` against a daemon without `settle_dump`: settle, read,
    /// then read until two reads agree and no window is mid-slide.
    /// (reacted, still moving, the screen).
    #[allow(clippy::too_many_arguments)]
    fn observe_stepwise(
        &self,
        t: &Target,
        conn: &mut Conn,
        seq: Option<u64>,
        b: Settle,
        had_rows: bool,
        deadline: Instant,
        parts: &mut Vec<String>,
    ) -> io::Result<(bool, bool, Value)> {
        let settle = |conn: &mut Conn, since: Option<u64>, first_ms: u32| -> io::Result<Vec<u8>> {
            match since {
                Some(n) => conn.call(&format!(
                    "settle since={n} first_ms={first_ms} idle_ms={} timeout_ms={}", b.idle_ms, b.timeout_ms)),
                // Older still: a plain quiet period.
                None => conn.call(&format!("wait_for_idle idle_ms={} timeout_ms={}", b.idle_ms, b.timeout_ms)),
            }
        };
        let lap = Instant::now();
        let first = settle(conn, seq, b.first_ms)?;
        let reacted = !String::from_utf8_lossy(&first).contains("changed=0");
        let still_moving = first.starts_with(b"ERR:");
        let mut dump = self.read_settled(conn, had_rows, deadline)?;
        if reacted && !self.unchanged(t, &dump) {
            dump = self.read_still(conn, dump, deadline)?;
            // Mid-slide positions hold still until the slide ends; its end
            // is the next event.
            if Screen::is_sliding(&dump) {
                let since = event_seq(conn);
                settle(conn, since, 700)?;
                let fresh = self.read_settled(conn, had_rows, deadline)?;
                dump = self.read_still(conn, fresh, deadline)?;
            }
        }
        parts.push(format!("stepwise {}", lap.elapsed().as_millis()));
        Ok((reacted, still_moving, dump))
    }

    /// `watch`: waits, without polling, until the screen differs from the
    /// one this server last showed the agent, or until a row shows `until`.
    /// A dedicated socket carries the daemon's `ui_watch` push stream.
    fn watch(&self, device: Option<&str>, until: Option<&str>, timeout_ms: u64) -> Reply {
        let t = match self.target(device) {
            Ok(t) => t,
            Err(e) => return (false, e),
        };
        // Taken from the pool but never given back: the stream owns it.
        let mut sess = match self.checkout(&t) {
            Ok(s) => s,
            Err(e) => return (false, e),
        };
        let started = Instant::now();
        let deadline = started + Duration::from_millis(timeout_ms);
        if let Err(e) = sess.conn.send_cmd("ui_watch min_ms=100") {
            return (false, format!("ERR:DAEMON_ERROR:{e}"));
        }
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let _ = sess.conn.set_read_timeout(Some(left));
            let frame = match sess.conn.read_frame() {
                Ok(f) => f,
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break,
                Err(e) => return (false, format!("ERR:DAEMON_ERROR:{e}")),
            };
            if frame.starts_with(b"ERR:") {
                return (false, String::from_utf8_lossy(&frame).into_owned());
            }
            let dump = match mobax_core::wire::parse_watch_frame(&frame) {
                Ok((_, _, d)) => d,
                Err(e) => return (false, format!("ERR:DAEMON_ERROR:{e}")),
            };
            let mut screens = lock(&self.screens);
            let prev = screens.get(&t);
            // Skip mid-transition frames (a starting window, rows that all
            // vanished while the next activity draws); the next frame follows.
            let had_rows = prev.is_some_and(|s| !s.is_empty());
            if Screen::is_starting(&dump) || (had_rows && Screen::row_count(&dump) == 0) {
                continue;
            }
            let screen = Screen::from_dump(&dump, prev);
            let change = screen.change_from(prev);
            let hit = match until {
                Some(text) => screen.shows(text),
                None => !matches!(change, Change::Same),
            };
            if hit {
                let note = format!("after {} ms", started.elapsed().as_millis());
                let text = screen.describe(&note, change);
                screens.insert(t, screen);
                return (true, text);
            }
        }
        let ms = started.elapsed().as_millis();
        match until {
            Some(text) => (false, format!(
                "ERR:TIMEOUT:{text:?} did not appear within {ms} ms\nCall `ui` to see where the screen is.")),
            None => (true, format!("no change on screen in {ms} ms")),
        }
    }

    /// The screen now, skipping mid-transition reads: a starting window, or
    /// a screen whose rows have all gone while the next activity draws. A
    /// screen that had no rows to begin with (a game's canvas) is taken as it
    /// is.
    fn read_settled(&self, conn: &mut Conn, had_rows: bool, deadline: Instant) -> io::Result<Value> {
        loop {
            let dump = parse_dump(&conn.call("dump")?)?;
            if !transitional(&dump, had_rows) || Instant::now() >= deadline {
                return Ok(dump);
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// Window animations send no accessibility events, so a screen can be
    /// quiet and still sliding in, its rows at coordinates about to change.
    /// Reads again until two reads agree on the rows and where they are.
    fn read_still(&self, conn: &mut Conn, mut dump: Value, deadline: Instant) -> io::Result<Value> {
        let mut rows = Screen::from_dump(&dump, None).table();
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(60));
            dump = parse_dump(&conn.call("dump")?)?;
            let now = Screen::from_dump(&dump, None).table();
            if now == rows {
                break;
            }
            rows = now;
        }
        Ok(dump)
    }

    /// `dump` shows nothing new against the last screen seen on `t`.
    fn unchanged(&self, t: &Target, dump: &Value) -> bool {
        let screens = lock(&self.screens);
        let prev = screens.get(t);
        matches!(Screen::from_dump(dump, prev).change_from(prev), Change::Same)
    }

    /// `ui`: the screen as a table of @N rows.
    fn ui(&self, device: Option<&str>) -> Reply {
        let t = match self.target(device) {
            Ok(t) => t,
            Err(e) => return (false, e),
        };
        if let Err(e) = self.reread(&t) {
            return (false, e);
        }
        let screens = lock(&self.screens);
        let Some(screen) = screens.get(&t) else { return (false, "ERR:INTERNAL:no screen".into()) };
        (true, format!("{} · {} rows\n{}", screen.title(), screen.len(), screen.table().trim_end()))
    }

    /// Reads the screen now and remembers it (refs carried over).
    fn reread(&self, t: &Target) -> Result<(), String> {
        let mut sess = self.checkout(t)?;
        let dump = sess.conn.call("dump").and_then(|b| parse_dump(&b)).map_err(|e| format!("ERR:DAEMON_ERROR:{e}"))?;
        self.checkin(t, sess);
        let mut screens = lock(&self.screens);
        let screen = Screen::from_dump(&dump, screens.get(t));
        screens.insert(t.clone(), screen);
        Ok(())
    }

    /// The remembered screen is no longer `seen` (another window set), so
    /// refs taken from `seen` mean nothing now.
    fn left(&self, t: &Target, seen: Option<&Screen>) -> bool {
        let screens = lock(&self.screens);
        match (screens.get(t), seen) {
            (Some(now), Some(seen)) => matches!(now.change_from(Some(seen)), Change::New),
            _ => true,
        }
    }

    /// The x,y and selector of row `@n`, checked against the screen as it
    /// is now: a ref from a screen that has since gone is refused (with the
    /// new screen), so a late transition can't send a tap to the wrong row.
    fn row(&self, device: Option<&str>, n: u32) -> Result<(i64, i64, String), String> {
        let t = self.target(device)?;
        let mut sess = self.checkout(&t)?;
        let dump = sess.conn.call("dump").and_then(|b| parse_dump(&b)).map_err(|e| format!("ERR:DAEMON_ERROR:{e}"))?;
        self.checkin(&t, sess);
        let mut screens = lock(&self.screens);
        let Some(prev) = screens.get(&t) else {
            return Err(format!("ERR:NOT_FOUND:no @{n} yet\nCall `ui` to number the screen."));
        };
        let had = prev.get(n).is_some();
        let now = Screen::from_dump(&dump, Some(prev));
        let same_screen = matches!(now.change_from(Some(prev)), Change::Same | Change::Rows(_));
        let found = now.get(n).map(|r| (r.x, r.y, r.sel.clone()));
        let reply = format!("{} · {} rows\n{}", now.title(), now.len(), now.table().trim_end());
        screens.insert(t, now);
        match found {
            Some(r) if had && same_screen => Ok(r),
            _ => Err(format!(
                "ERR:PRECONDITION:@{n} is not on the screen any more; nothing was done\nThe screen now · {reply}"
            )),
        }
    }

    fn screenshot(&self, device: Option<&str>, size: Option<i64>, native: bool) -> Result<Vec<u8>, String> {
        let t = self.target(device)?;
        let mut sess = self.checkout(&t)?;
        let wire = if native { "screenshot max=1".to_string() } else { format!("screenshot size={}", size.unwrap_or(768)) };
        let bytes = sess.conn.call(&wire).map_err(|e| format!("ERR:DAEMON_ERROR:{e}"))?;
        self.checkin(&t, sess);
        if bytes.starts_with(b"ERR:") {
            return Err(String::from_utf8_lossy(&bytes).into_owned());
        }
        Ok(bytes)
    }
}

/// How long to wait for an action's effect to show and then calm down.
#[derive(Clone, Copy)]
struct Settle {
    /// For the first accessibility event after the action.
    first_ms: u32,
    /// Quiet that counts as settled.
    idle_ms: u32,
    timeout_ms: u32,
    /// After a settle that changed nothing visible, wait this long for a
    /// late screen (0: don't).
    second_look_ms: u32,
    /// How long, in all, to wait while the screen is a starting window or
    /// empty (an app launching); below [Settle::timeout_ms] it changes nothing.
    launch_ms: u32,
}

/// How an action's settle ended, for the reply's first line. [loading]: the
/// wait ran out on a starting window or an empty screen, so nothing on it
/// can be acted on yet.
fn settle_note(ms: u128, still_moving: bool, reacted: bool, loading: bool) -> String {
    if loading {
        format!("still loading after {ms} ms: call `wait` for what you need before acting")
    } else if still_moving {
        format!("still changing after {ms} ms")
    } else if reacted {
        format!("settled in {ms} ms")
    } else {
        "no reaction on screen".to_string()
    }
}

/// An app launch takes a few hundred ms to show anything and arrives in
/// bursts; a tap or a key may open another screen; typing and scrolling
/// react at once. A tap's second look is short: launching apps from the
/// home screen always showed a change within the first window, so it mostly
/// runs for taps that do nothing.
fn settle_budget(verb: &str) -> Settle {
    match verb {
        "open" => Settle { first_ms: 3000, idle_ms: 300, timeout_ms: 6000, second_look_ms: 0, launch_ms: 15_000 },
        "tap" | "go" => Settle { first_ms: 500, idle_ms: 200, timeout_ms: 3000, second_look_ms: 400, launch_ms: 6000 },
        "swipe" => Settle { first_ms: 300, idle_ms: 200, timeout_ms: 2000, second_look_ms: 0, launch_ms: 0 },
        _ => Settle { first_ms: 300, idle_ms: 150, timeout_ms: 1500, second_look_ms: 0, launch_ms: 0 },
    }
}

struct VerbRun {
    ok: bool,
    text: String,
    /// The socket is still in step with the daemon and can go back to the pool.
    keep: bool,
}

/// One CLI verb on `sess` (a socket to `serial`), its output captured.
/// Verbs that open their own connections go to the same device.
fn run_verb(serial: &str, sess: &mut Session, argv: &[String]) -> VerbRun {
    let (res, text) = crate::daemon::with_target(serial, || {
        output::capture(|| crate::dispatch_session_verb(sess, argv))
    });
    let text = text.trim_end().to_string();
    match res {
        Ok(()) => VerbRun { ok: true, text, keep: true },
        Err(e) => match output::err_code_of(&e) {
            // A reported failure: the daemon's reply was read in full.
            Some(code) => VerbRun { ok: false, text: with_hint(text, code), keep: true },
            // Not reported: setup or the socket failed. Only a bad argument
            // is caught before anything is sent, so only then is the socket
            // still in step.
            None => {
                let code = crate::errors::code_of(&e);
                let msg = format!("{text}\nERR:{}:{e}", code.as_short());
                let msg = msg.trim_start().to_string();
                let text = match e.get_ref().and_then(|i| i.downcast_ref::<mobax_core::Error>()) {
                    Some(mobax_core::Error::WhichDevice(_)) => format!("{msg}\nPass `device`, or call `use` with one."),
                    _ => with_hint(msg, code),
                };
                VerbRun { ok: false, text, keep: code == crate::errors::ErrCode::BadArg }
            }
        },
    }
}

/// Adds what an agent should do next after an error.
fn with_hint(text: String, code: crate::errors::ErrCode) -> String {
    use crate::errors::ErrCode::*;
    let hint = match code {
        NotFound => "Not on screen. Call `ui` to see what is there, or `swipe` to scroll.",
        Timeout => "It did not happen in time. Call `ui` to see where the screen is.",
        Ambiguous => "Several nodes match. Use an @N ref from `ui`, or a more specific selector.",
        DeviceGone | DaemonError => "Call `use` to reconnect.",
        SecureWindow => "The app blocks screen capture here; use `ui` instead.",
        BadArg | UnknownCmd => "Check the arguments against the tool's schema.",
        Precondition | Internal => "",
    };
    if hint.is_empty() { text } else { format!("{text}\n{hint}") }
}

/// `settle_dump`: the daemon settles after the action that followed
/// `since` and answers with the screen once it holds still. (reacted, still
/// moving, the screen), or None from a daemon that predates the verb.
fn settle_dump(conn: &mut Conn, since: u64, first_ms: u32, b: Settle) -> io::Result<Option<(bool, bool, Value)>> {
    let body = conn.call(&format!(
        "settle_dump since={since} first_ms={first_ms} idle_ms={} timeout_ms={}", b.idle_ms, b.timeout_ms))?;
    if body.starts_with(b"ERR:UNKNOWN_CMD") {
        return Ok(None);
    }
    let nl = body.iter().position(|&c| c == b'\n')
        .ok_or_else(|| io::Error::other(format!("settle_dump: {}", String::from_utf8_lossy(&body))))?;
    let head = String::from_utf8_lossy(&body[..nl]);
    Ok(Some((head.contains("changed=1"), head.contains("moving=1"), parse_dump(&body[nl + 1..])?)))
}

/// A launching app's starting window, or rows gone that were there: the
/// screen in between two others.
fn transitional(dump: &Value, had_rows: bool) -> bool {
    Screen::is_starting(dump) || (had_rows && Screen::row_count(dump) == 0)
}

/// An `open` of [expect] while the screen from before it ([before]) is still
/// in front: the launch hasn't arrived (a late event from that screen ended
/// the settle). Any other package in front counts as arrived: the app may
/// raise another package's window first (a permission dialog).
fn still_before(before: Option<&str>, now: &str, expect: Option<&str>) -> bool {
    matches!((before, expect), (Some(b), Some(e)) if b != e && now == b)
}

/// The package in `open`'s reply: `ok started=PKG/ACTIVITY …`.
fn started_pkg(text: &str) -> Option<String> {
    let rest = text.split_once("started=")?.1;
    let pkg = rest.split(|c: char| c == '/' || c.is_whitespace()).next()?;
    (!pkg.is_empty()).then(|| pkg.to_string())
}

/// `toasts` reply → a `toast: …` line per toast (none for an error: an
/// older daemon doesn't keep them).
fn toast_lines(reply: &str) -> String {
    if reply.starts_with("ERR:") {
        return String::new();
    }
    reply.lines().filter(|l| !l.trim().is_empty()).map(|l| format!("\ntoast: {l}")).collect()
}

/// The daemon's event counter, to settle against after an action.
fn event_seq(conn: &mut Conn) -> Option<u64> {
    let body = conn.call("event_seq").ok()?;
    String::from_utf8_lossy(&body).trim().strip_prefix("seq=")?.parse().ok()
}

fn parse_dump(body: &[u8]) -> io::Result<Value> {
    if body.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(body).into_owned()));
    }
    let text = std::str::from_utf8(body).map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
    json::parse(text).map_err(|e| io::Error::other(format!("dump not json: {e}")))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `@12` → 12.
fn row_ref(s: &str) -> Option<u32> {
    s.trim().strip_prefix('@')?.parse().ok()
}

// ---------- tools ----------

struct Tool {
    name: &'static str,
    description: &'static str,
    /// JSON Schema of the arguments object.
    schema: &'static str,
}

// Shared arguments carry no description here: the instructions explain
// them once, instead of once per tool in every session's tool list.
const DEVICE: &str = r#""device":{"type":"string"}"#;
const TIMEOUT: &str = r#""timeout_ms":{"type":"integer"}"#;
const OBSERVE: &str = r#""observe":{"type":"boolean"}"#;
const SCROLL: &str = r#""scroll":{"type":"boolean","description":"page the biggest list until the target shows"}"#;

/// The tool list, built once (its schemas are leaked strings).
fn tools() -> &'static [Tool] {
    static TOOLS: std::sync::OnceLock<Vec<Tool>> = std::sync::OnceLock::new();
    TOOLS.get_or_init(build_tools)
}

/// An argument [name] doesn't take (a batch step's own `tool` aside), and
/// the ones it does. The server checks this itself, so the schemas can
/// leave out `additionalProperties`.
fn unknown_arg(name: &str, args: &Value) -> Option<(String, Vec<String>)> {
    let tool = tools().iter().find(|t| t.name == name)?;
    let schema = json::parse(tool.schema).ok()?;
    let props = json::obj_get(&schema, "properties")?;
    let Value::Obj(known) = props else { return None };
    let known: Vec<String> = known.iter().map(|(k, _)| k.clone()).collect();
    let Value::Obj(given) = args else { return None };
    given
        .iter()
        .map(|(k, _)| k)
        .find(|k| k.as_str() != "tool" && !known.contains(k))
        .map(|k| (k.clone(), known))
}

fn build_tools() -> Vec<Tool> {
    // Schemas are assembled at startup from the shared fragments above.
    fn obj(props: &[&str], required: &[&str]) -> &'static str {
        let req: Vec<String> = required.iter().map(|r| format!("\"{r}\"")).collect();
        let req = if req.is_empty() { String::new() } else { format!(r#","required":[{}]"#, req.join(",")) };
        Box::leak(
            format!(r#"{{"type":"object","properties":{{{}}}{req}}}"#, props.join(","))
                .into_boxed_str(),
        )
    }
    vec![
        Tool {
            name: "use",
            description: "Connect to a device and start the on-device daemon. Call this first.",
            schema: obj(&[r#""serial":{"type":"string","description":"omit when only one is attached"}"#], &[]),
        },
        Tool {
            name: "ui",
            description: "The screen as @N rows. The main way to read it.",
            schema: obj(&[r#""scope":{"type":"string","enum":["app","active","all"],"description":"app (default): app windows and dialogs; active: focused window; all: system windows too"}"#, DEVICE], &[]),
        },
        Tool {
            name: "screenshot",
            description: "The screen as a JPEG (768 px long edge by default).",
            schema: obj(&[r#""size":{"type":"integer","description":"long edge px"}"#, r#""native":{"type":"boolean"}"#, DEVICE], &[]),
        },
        Tool {
            name: "find",
            description: "Print the nodes a selector matches, without acting.",
            schema: obj(&[r#""selector":{"type":"string"}"#, SCROLL, TIMEOUT, DEVICE], &["selector"]),
        },
        Tool {
            name: "tap",
            description: "Tap a target (@N, text, #resource_id, selector) or x,y.",
            schema: obj(&[r#""target":{"type":"string"}"#, SCROLL, r#""x":{"type":"integer"}"#, r#""y":{"type":"integer"}"#, TIMEOUT, OBSERVE, DEVICE], &[]),
        },
        Tool {
            name: "type",
            description: "Type text at the cursor of the focused field, any language.",
            schema: obj(&[r#""text":{"type":"string"}"#, OBSERVE, DEVICE], &["text"]),
        },
        Tool {
            name: "fill",
            description: "Set an edit field's text in one step. selector: @N, #resource_id, selector, or the field's label.",
            schema: obj(&[r#""selector":{"type":"string"}"#, r#""text":{"type":"string"}"#, OBSERVE, DEVICE], &["selector", "text"]),
        },
        Tool {
            name: "swipe",
            description: "Swipe the finger this way (up scrolls content down).",
            schema: obj(&[r#""direction":{"type":"string","enum":["up","down","left","right"]}"#, r#""duration_ms":{"type":"integer"}"#, OBSERVE, DEVICE], &["direction"]),
        },
        Tool {
            name: "go",
            description: "Press a key: back, home, recents, enter, menu, …",
            schema: obj(&[r#""key":{"type":"string"}"#, OBSERVE, DEVICE], &["key"]),
        },
        Tool {
            name: "batch",
            description: "Several steps in one call, e.g. [{\"tool\":\"tap\",\"target\":\"@3\"},{\"tool\":\"go\",\"key\":\"enter\"}]. Each settles before the next; stops at the first failure. Replies one line per step, then what changed overall. @N refs are from the screen you last saw: after a step opens another screen, use text or selectors.",
            schema: obj(&[
                r#""steps":{"type":"array","maxItems":20,"items":{"type":"object","properties":{"tool":{"type":"string","enum":["tap","type","fill","swipe","go","open","close","wait","find","watch"]}},"required":["tool"]},"description":"{\"tool\": NAME, ...its arguments}"}"#,
                DEVICE,
            ], &["steps"]),
        },
        Tool {
            name: "wait",
            description: "Wait until a target appears.",
            schema: obj(&[r#""target":{"type":"string"}"#, TIMEOUT, DEVICE], &["target"]),
        },
        Tool {
            name: "watch",
            description: "Wait until the screen differs from what you last saw (or `until` a row shows this text, case-insensitive), then reply with what changed.",
            schema: obj(&[r#""until":{"type":"string"}"#, TIMEOUT, DEVICE], &[]),
        },
        Tool {
            name: "open",
            description: "Launch an app: package, PKG/.Activity, or label.",
            schema: obj(&[r#""app":{"type":"string"}"#, OBSERVE, DEVICE], &["app"]),
        },
        Tool {
            name: "close",
            description: "Force-stop an app by package name.",
            schema: obj(&[r#""package":{"type":"string"}"#, DEVICE], &["package"]),
        },
        Tool {
            name: "logcat",
            description: "Recent logcat lines, newest last. level: minimum priority; tags: TAG or TAG:LEVEL; grep: regexes, one must match; exclude: drop lines containing; since: 90, 30s, 5m, 2h; format: logcat -v (threadtime); lines: default 100; crash: crash buffer; clear: clear instead of read.",
            schema: obj(&[
                r#""package":{"type":"string"}"#,
                r#""pid":{"type":"integer"}"#,
                r#""level":{"type":"string","enum":["V","D","I","W","E","F"]}"#,
                r#""tags":{"type":"array","items":{"type":"string"}}"#,
                r#""grep":{"type":"array","items":{"type":"string"}}"#,
                r#""exclude":{"type":"array","items":{"type":"string"}}"#,
                r#""ignore_case":{"type":"boolean"}"#,
                r#""since":{"type":"string"}"#,
                r#""format":{"type":"string"}"#,
                r#""lines":{"type":"integer"}"#,
                r#""crash":{"type":"boolean"}"#,
                r#""clear":{"type":"boolean"}"#,
                DEVICE,
            ], &[]),
        },
        Tool {
            name: "info",
            description: "Model, Android version, display size, foreground app.",
            schema: obj(&[DEVICE], &[]),
        },
        Tool {
            name: "drop",
            description: "Stop the on-device daemon.",
            schema: obj(&[DEVICE], &[]),
        },
        Tool {
            name: "mobax",
            description: "Any other mobax CLI verb, e.g. [\"apps\",\"--3rd\"], [\"prop\",\"ro.build.version.sdk\"]. See `mobax --help`.",
            schema: obj(&[r#""args":{"type":"array","items":{"type":"string"}}"#, DEVICE], &["args"]),
        },
    ]
}

fn tools_list() -> String {
    let mut out = String::from(r#"{"tools":["#);
    for (i, t) in tools().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let (mut name, mut desc) = (String::new(), String::new());
        encode_str(&mut name, t.name);
        encode_str(&mut desc, t.description);
        out.push_str(&format!(r#"{{"name":{name},"description":{desc},"inputSchema":{}}}"#, t.schema));
    }
    out.push_str("]}");
    out
}

fn tools_call(params: Option<&Value>, engine: &Engine) -> Result<String, (i32, String)> {
    let params = params.ok_or((-32602, "tools/call needs params".to_string()))?;
    let name = json::obj_get(params, "name").and_then(json::as_str).unwrap_or("");
    let empty = Value::Obj(Vec::new());
    let args = json::obj_get(params, "arguments").unwrap_or(&empty);
    if name == "batch" {
        return Ok(reply(batch(args, engine)));
    }
    Ok(match call_tool(name, args, engine, None, false)? {
        Out::Text(r) => reply(r),
        Out::Json(result) => result,
    })
}

/// A tool's result: text, or a ready `tools/call` result (an image).
enum Out {
    Text(Reply),
    Json(String),
}

/// Runs tool `name`. `device` is the batch's device, for a step that names
/// none; `brief` keeps an action's reply to how the screen settled.
fn call_tool(name: &str, args: &Value, engine: &Engine, device: Option<&str>, brief: bool) -> Result<Out, (i32, String)> {
    let s = |k: &str| json::obj_get(args, k).and_then(json::as_str).map(str::to_string);
    let n = |k: &str| json::obj_get(args, k).and_then(json::as_num);
    let b = |k: &str| matches!(json::obj_get(args, k), Some(Value::Bool(true)));
    let need = |k: &str| s(k).ok_or_else(|| (-32602, format!("{name}: missing \"{k}\"")));
    if let Some((k, known)) = unknown_arg(name, args) {
        return Err((-32602, format!("{name}: unknown argument \"{k}\"; it takes {}", known.join(", "))));
    }
    // Actions report what changed unless the caller opts out.
    let observe = !matches!(json::obj_get(args, "observe"), Some(Value::Bool(false)));
    let device = s("device").or_else(|| device.map(str::to_string));
    let dev = device.as_deref();

    let mut argv: Vec<String> = Vec::new();
    let timeout = |argv: &mut Vec<String>| {
        if let Some(t) = n("timeout_ms") {
            argv.extend(["--timeout".to_string(), t.to_string()]);
        }
    };
    // How the verb runs: child process, plain in-process, or as an action.
    enum Via { Child, Run, Act }
    let via = match name {
        "use" => {
            argv.push("use".into());
            argv.extend(s("serial"));
            Via::Child
        }
        "drop" => {
            argv.push("drop".into());
            Via::Child
        }
        "ui" => match s("scope").as_deref() {
            Some("active") => {
                argv.extend(["ui".into(), "--active".into()]);
                Via::Run
            }
            Some("all") => {
                argv.extend(["ui".into(), "--all".into()]);
                Via::Run
            }
            _ if engine.in_process() => return Ok(Out::Text(engine.ui(dev))),
            _ => {
                argv.push("ui".into());
                Via::Run
            }
        },
        "screenshot" => {
            if !engine.in_process() {
                return screenshot_child(n("size"), b("native"), device, &engine.global).map(Out::Json);
            }
            return Ok(Out::Json(match engine.screenshot(dev, n("size"), b("native")) {
                Ok(jpeg) => format!(
                    r#"{{"content":[{{"type":"image","mimeType":"image/jpeg","data":"{}"}}],"isError":false}}"#,
                    base64(&jpeg)
                ),
                Err(e) => text_result(&e, true),
            }));
        }
        "find" => {
            argv.extend(["find".into(), need("selector")?]);
            timeout(&mut argv);
            Via::Run
        }
        "tap" => {
            argv.push("tap".into());
            match (s("target"), n("x"), n("y")) {
                (Some(t), _, _) => match row_ref(&t) {
                    Some(r) => match engine.row(dev, r) {
                        Ok((x, y, _)) => argv.extend([x.to_string(), y.to_string()]),
                        Err(e) => return Ok(Out::Text((false, e))),
                    },
                    None => argv.push(t),
                },
                (None, Some(x), Some(y)) => argv.extend([x.to_string(), y.to_string()]),
                _ => return Err((-32602, "tap: give \"target\" or both \"x\" and \"y\"".into())),
            }
            timeout(&mut argv);
            Via::Act
        }
        "type" => {
            argv.extend(["type".into(), need("text")?]);
            Via::Act
        }
        "fill" => {
            let sel = need("selector")?;
            let sel = match row_ref(&sel) {
                Some(r) => match engine.row(dev, r) {
                    Ok((_, _, daemon_sel)) => daemon_sel,
                    Err(e) => return Ok(Out::Text((false, e))),
                },
                None => sel,
            };
            argv.extend(["fill".into(), sel, need("text")?]);
            Via::Act
        }
        "swipe" => {
            argv.extend(["swipe".into(), need("direction")?]);
            argv.extend(n("duration_ms").map(|d| d.to_string()));
            Via::Act
        }
        "go" => {
            argv.extend(["go".into(), need("key")?]);
            Via::Act
        }
        "wait" => {
            argv.extend(["wait".into(), need("target")?]);
            timeout(&mut argv);
            Via::Run
        }
        "watch" => {
            if !engine.in_process() {
                return Ok(Out::Text((false, "ERR:PRECONDITION:watch needs the daemon reachable through the local adb server".into())));
            }
            let timeout = n("timeout_ms").filter(|t| *t > 0).unwrap_or(10_000) as u64;
            return Ok(Out::Text(engine.watch(dev, s("until").as_deref(), timeout)));
        }
        "open" => {
            argv.extend(["open".into(), need("app")?]);
            Via::Act
        }
        "close" => {
            argv.extend(["close".into(), need("package")?]);
            Via::Run
        }
        "logcat" => {
            argv.push("logs".into());
            if let Some(p) = s("package") {
                argv.extend(["--pkg".into(), p]);
            }
            if let Some(l) = s("level") {
                argv.extend(["--level".into(), l]);
            }
            if let Some(p) = n("pid") {
                argv.extend(["--pid".into(), p.to_string()]);
            }
            for (key, flag) in [("tags", "--tag"), ("grep", "--grep"), ("exclude", "--exclude")] {
                // A lone string is accepted too.
                let v = json::obj_get(args, key);
                let items = v.and_then(json::as_arr).map(|a| a.iter().filter_map(json::as_str).collect())
                    .or_else(|| v.and_then(json::as_str).map(|s| vec![s]))
                    .unwrap_or_default();
                for item in items {
                    argv.extend([flag.into(), item.to_string()]);
                }
            }
            if b("ignore_case") {
                argv.push("--ignore-case".into());
            }
            if let Some(v) = s("since") {
                argv.extend(["--since".into(), v]);
            }
            if let Some(v) = s("format") {
                argv.extend(["--format".into(), v]);
            }
            if let Some(n) = n("lines") {
                argv.extend(["--tail".into(), n.to_string()]);
            }
            if b("crash") {
                argv.push("--crash".into());
            }
            if b("clear") {
                argv.push("--clear".into());
            }
            Via::Run
        }
        "info" => {
            argv.push("info".into());
            Via::Run
        }
        "mobax" => {
            let list = json::obj_get(args, "args").and_then(json::as_arr).unwrap_or(&[]);
            argv = list.iter().filter_map(json::as_str).map(str::to_string).collect();
            let verb = argv.first().map(String::as_str).unwrap_or("");
            if verb.is_empty() {
                return Err((-32602, "mobax: \"args\" needs at least a verb".into()));
            }
            if BLOCKED_VERBS.contains(&verb) || (verb == "see" && argv.len() == 1) {
                return Ok(Out::Text((false, format!("`mobax {verb}` is interactive or opens a window; it isn't available over MCP"))));
            }
            Via::Child
        }
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    };
    // `scroll`: bring the target into view first, a page at a time.
    let scrolled = match (b("scroll"), name) {
        (true, "tap") => s("target").filter(|t| row_ref(t).is_none()),
        (true, "find") => s("selector"),
        _ => None,
    }
    .map(|sel| {
        let run = |argv: &[String], act: bool| -> Reply {
            if !engine.in_process() {
                child(engine, device.clone(), argv)
            } else if act {
                engine.act(dev, argv, true, true)
            } else {
                engine.run(dev, argv)
            }
        };
        let look = ["find".to_string(), sel];
        let page = ["do".to_string(), "scroll_forward".to_string()];
        let seq = ["do".to_string(), "event_seq".to_string()];
        scroll_until(SCROLL_MAX, || run(&look, false).0, || {
            let since = run(&seq, false).1;
            let (ok, text) = run(&page, false);
            if !ok || text.contains("ERR:") {
                return Err(());
            }
            // The scroll animates: wait until it stops, or a tap on what it
            // brought into view lands mid-scroll and only stops the list.
            if let Some(n) = since.trim().strip_prefix("seq=") {
                run(&["do".to_string(), format!("settle since={n} first_ms=300 idle_ms=300 timeout_ms=3000")], false);
            }
            Ok(())
        })
    });
    let (ok, text) = match via {
        Via::Run | Via::Act if !engine.in_process() => child(engine, device.clone(), &argv),
        Via::Run => engine.run(dev, &argv),
        Via::Act => engine.act(dev, &argv, observe, brief),
        Via::Child => {
            let r = child(engine, device.clone(), &argv);
            if matches!(name, "use" | "drop") {
                engine.forget();
            }
            r
        }
    };
    // A tap whose target isn't drawn yet (an app still loading, a dialog
    // sliding in): if the screen is moving, let it settle and look once more.
    // A still screen fails at once, as before.
    let by_target = name == "tap" && s("target").is_some_and(|t| row_ref(&t).is_none());
    let (ok, text) = if !ok && by_target && text.contains("NOT_FOUND") && engine.in_process() {
        // Look again each time the screen moves and settles: a dialog from
        // another process can draw after a quiet gap. Stop at the first
        // window with no events (the screen is still) or at the cap.
        let started = Instant::now();
        let mut last = (ok, text);
        while started.elapsed() < Duration::from_millis(TAP_WAIT_CAP_MS) {
            let seq = engine.run(dev, &["do".to_string(), "event_seq".to_string()]).1;
            let Some(n) = seq.trim().strip_prefix("seq=").and_then(|n| n.parse::<u64>().ok()) else { break };
            let wire = format!("settle since={n} first_ms={TAP_MOVING_MS} idle_ms=300 timeout_ms={TAP_WAIT_CAP_MS}");
            if !moved_then_settled(&engine.run(dev, &["do".to_string(), wire]).1) {
                break;
            }
            let again = engine.act(dev, &argv, observe, brief);
            if again.0 {
                last = (true, format!("waited {} ms for the screen to settle\n{}", started.elapsed().as_millis(), again.1));
                break;
            }
            last = again;
        }
        last
    } else {
        (ok, text)
    };
    let text = match scrolled {
        Some(Scrolled { times: 0, found: true }) | None => text,
        Some(Scrolled { times, found: true }) => format!("scrolled {times}×\n{text}"),
        Some(Scrolled { times, found: false }) => format!("{text}\n(scrolled {times}× to the end of the list)"),
    };
    // A bare word is a class name in selector syntax; if it missed but a node
    // shows it as text, say how to name that node (nothing is retried).
    let word = match name {
        "tap" | "wait" => s("target"),
        "find" => s("selector"),
        _ => None,
    };
    let missed = text.contains("NOT_FOUND") || text.contains("expected class name") || (name == "wait" && text.contains("TIMEOUT"));
    let text = match word {
        Some(w) if !ok && missed && bare_word(&w) => {
            let look = ["find".to_string(), format!(":has-text({w:?})")];
            let shown = if engine.in_process() { engine.run(dev, &look).0 } else { child(engine, device.clone(), &look).0 };
            if shown { format!("{text}\n{}", class_word_hint(&w)) } else { text }
        }
        _ => text,
    };
    Ok(Out::Text((ok, text)))
}

/// How long a missed tap watches for screen events before calling the
/// screen still (and failing): a tap's second look, since another app's
/// window (a permission dialog) can be quiet that long before it draws.
const TAP_MOVING_MS: u32 = 400;

/// The most a missed tap spends waiting on a moving screen.
const TAP_WAIT_CAP_MS: u64 = 4000;

/// The `settle` reply after a missed tap: the screen was moving and has
/// settled (or never stopped within the cap), so the target may be there now.
fn moved_then_settled(reply: &str) -> bool {
    reply.contains("changed=1") || reply.contains("TIMEOUT:settle")
}

/// A target with no selector syntax: one word, read as a class name.
fn bare_word(t: &str) -> bool {
    !t.is_empty() && !t.chars().any(|c| c.is_whitespace() || "@#.:[]()\"'=*>~+,".contains(c))
}

fn class_word_hint(w: &str) -> String {
    format!("No class \"{w}\", but a node shows that text: target it with :has-text({w:?}).")
}

/// Most pages `scroll: true` turns before giving up.
const SCROLL_MAX: u32 = 10;

#[derive(Debug, PartialEq)]
struct Scrolled {
    times: u32,
    found: bool,
}

/// Looks with [found], and while it misses scrolls a page with [scroll]:
/// at most [max] pages, and not past a page that can't move (the end).
fn scroll_until(max: u32, mut found: impl FnMut() -> bool, mut scroll: impl FnMut() -> Result<(), ()>) -> Scrolled {
    let mut times = 0;
    loop {
        if found() {
            return Scrolled { times, found: true };
        }
        if times == max || scroll().is_err() {
            return Scrolled { times, found: false };
        }
        times += 1;
    }
}

/// Tools a `batch` step may be: the ones that act or wait on the screen.
const BATCH_TOOLS: &[&str] = &["tap", "type", "fill", "swipe", "go", "open", "close", "wait", "find", "watch"];

/// Most steps in one `batch`.
const BATCH_MAX: usize = 20;

/// `batch`: several steps in one call, so an agent spends one round trip on
/// a sequence it already knows. Each action still waits for the screen to
/// settle (the next step needs a still screen), but replies only with how
/// it settled; the reply ends with what changed over the whole batch. Stops
/// at the first failing step.
fn batch(args: &Value, engine: &Engine) -> Reply {
    let steps = json::obj_get(args, "steps").and_then(json::as_arr).unwrap_or(&[]);
    if steps.is_empty() || steps.len() > BATCH_MAX {
        return (false, format!("ERR:BAD_ARG:batch needs 1 to {BATCH_MAX} steps"));
    }
    // A typo anywhere stops the batch before its first step runs.
    for step in steps {
        let name = json::obj_get(step, "tool").and_then(json::as_str).unwrap_or("");
        if let Some((k, known)) = unknown_arg(name, step) {
            return (false, format!("ERR:BAD_ARG:{name}: unknown argument \"{k}\"; it takes {}", known.join(", ")));
        }
    }
    if !engine.in_process() {
        return (false, "ERR:PRECONDITION:batch needs the daemon reachable through the local adb server".into());
    }
    let device = json::obj_get(args, "device").and_then(json::as_str);
    let t = match engine.target(device) {
        Ok(t) => t,
        Err(e) => return (false, e),
    };
    // @N refs name rows of the screen the agent last saw.
    let seen = lock(&engine.screens).get(&t).cloned();
    let mut out = String::new();
    let mut ok = true;
    for (i, step) in steps.iter().enumerate() {
        let name = json::obj_get(step, "tool").and_then(json::as_str).unwrap_or("");
        let text = if !BATCH_TOOLS.contains(&name) {
            Err(format!("ERR:BAD_ARG:\"{name}\" can't be a batch step; use one of {}", BATCH_TOOLS.join(", ")))
        } else if i > 0 && uses_ref(step) && engine.left(&t, seen.as_ref()) {
            Err("ERR:PRECONDITION:an earlier step opened another screen, so its @N refs are gone; nothing was done\n\
                 Name the target by text or selector, or call `ui`."
                .to_string())
        } else {
            match call_tool(name, step, engine, Some(&t.serial), true) {
                Ok(Out::Text((true, text))) => Ok(text),
                Ok(Out::Text((false, text))) => Err(text),
                Ok(Out::Json(_)) => Err(format!("ERR:BAD_ARG:\"{name}\" can't be a batch step")),
                Err((_, e)) => Err(format!("ERR:BAD_ARG:{e}")),
            }
        };
        let (mark, text) = match text {
            Ok(t) => ("ok", t),
            Err(t) => ("FAILED", t),
        };
        let text = text.trim();
        out.push_str(&format!("{}. {name} {mark}{}{}\n", i + 1, if text.is_empty() { "" } else { ": " }, text.replace('\n', " · ")));
        if mark == "FAILED" {
            ok = false;
            break;
        }
    }
    // The screen now, against the one the agent saw before the batch.
    match engine.reread(&t) {
        Ok(()) => {
            let screens = lock(&engine.screens);
            if let Some(now) = screens.get(&t) {
                out.push_str(&now.describe("after the batch", now.change_from(seen.as_ref())));
            }
        }
        Err(e) => out.push_str(&format!("(could not read the screen afterwards: {e})")),
    }
    (ok, out)
}

/// The step names a row by @N.
fn uses_ref(step: &Value) -> bool {
    ["target", "selector"]
        .iter()
        .any(|k| json::obj_get(step, k).and_then(json::as_str).and_then(row_ref).is_some())
}

fn reply((ok, text): Reply) -> String {
    text_result(&text, !ok)
}

/// Runs `mobax ARGV…` as a child process.
fn child(engine: &Engine, device: Option<String>, argv: &[String]) -> Reply {
    let out = run_hs(&engine.global, device, argv);
    (out.ok, out.text)
}

fn screenshot_child(size: Option<i64>, native: bool, device: Option<String>, global: &[String]) -> Result<String, (i32, String)> {

    let path = std::env::temp_dir().join(format!("mobax-mcp-{}-{}.jpg", std::process::id(), now_ns()));
    let mut argv = vec!["see".to_string(), path.to_string_lossy().into_owned()];
    if native {
        argv.push("--native".into());
    } else if let Some(n) = size {
        argv.extend(["--size".into(), n.to_string()]);
    }
    let out = run_hs(global, device, &argv);
    let bytes = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    match bytes {
        Ok(b) if out.ok && !b.is_empty() => Ok(format!(
            r#"{{"content":[{{"type":"image","mimeType":"image/jpeg","data":"{}"}}],"isError":false}}"#,
            base64(&b)
        )),
        _ => Ok(text_result(&out.text, true)),
    }
}

fn text_result(text: &str, is_error: bool) -> String {
    let mut t = String::new();
    encode_str(&mut t, if text.is_empty() { "ok" } else { text });
    format!(r#"{{"content":[{{"type":"text","text":{t}}}],"isError":{is_error}}}"#)
}

struct Output {
    ok: bool,
    text: String,
}

/// Runs `mobax [GLOBAL] [-s DEVICE] ARGS…` with no stdin; stdout and stderr
/// are merged into the tool result, and a non-zero exit marks it an error.
///
/// Output goes to a temp file, not a pipe: `mobax use` leaves long-lived
/// processes behind (the state-cache helper, an adb server) that inherit
/// the child's handles, so a pipe would never reach EOF.
fn run_hs(global: &[String], device: Option<String>, argv: &[String]) -> Output {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return Output { ok: false, text: format!("cannot locate mobax: {e}") },
    };
    let log = std::env::temp_dir().join(format!("mobax-mcp-{}-{}.out", std::process::id(), now_ns()));
    let file = match std::fs::File::create(&log) {
        Ok(f) => f,
        Err(e) => return Output { ok: false, text: format!("cannot create {}: {e}", log.display()) },
    };
    let err_file = match file.try_clone() {
        Ok(f) => f,
        Err(e) => return Output { ok: false, text: format!("cannot duplicate output handle: {e}") },
    };
    let mut cmd = Command::new(exe);
    cmd.args(global);
    if let Some(d) = device {
        cmd.args(["-s", &d]);
    }
    cmd.args(argv)
        // Plain text is the agent-oriented format; ignore a JSON default.
        .env_remove("HS_FORMAT")
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err_file));
    let status = match cmd.spawn() {
        Ok(mut child) => {
            let deadline = Instant::now() + CALL_TIMEOUT;
            loop {
                match child.try_wait() {
                    Ok(Some(s)) => break Some(s),
                    Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                }
            }
        }
        Err(e) => {
            let _ = std::fs::remove_file(&log);
            return Output { ok: false, text: format!("cannot run mobax: {e}") };
        }
    };
    let mut text = std::fs::read(&log).map(|b| String::from_utf8_lossy(&b).trim_end().to_string()).unwrap_or_default();
    // A parse error prints the whole usage after the message; keep the message.
    if let Some(i) = text.find(USAGE_HEAD) {
        text.truncate(i);
        text = text.trim_end().to_string();
    }
    // A helper left running may still hold the file on Windows; the temp
    // dir is cleaned eventually either way.
    let _ = std::fs::remove_file(&log);
    match status {
        Some(s) => Output { ok: s.success(), text },
        None => Output { ok: false, text: format!("timed out after {}s
{text}", CALL_TIMEOUT.as_secs()) },
    }
}

// ---------- helpers ----------

/// Re-serializes a parsed JSON value (used to echo request ids verbatim).
fn to_json(v: &Value) -> String {
    let mut out = String::new();
    write_json(&mut out, v);
    out
}

fn write_json(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => out.push_str(&n.to_string()),
        Value::Float(f) => out.push_str(&f.to_string()),
        Value::Str(s) => encode_str(out, s),
        Value::Arr(items) => {
            out.push('[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(out, it);
            }
            out.push(']');
        }
        Value::Obj(pairs) => {
            out.push('{');
            for (i, (k, val)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                encode_str(out, k);
                out.push(':');
                write_json(out, val);
            }
            out.push('}');
        }
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_still_loading_at_the_deadline_is_not_settled() {
        let n = settle_note(6829, false, true, true);
        assert!(n.starts_with("still loading after 6829 ms"), "{n}");
        assert!(n.contains("wait"), "{n}");
        assert_eq!(settle_note(900, false, true, false), "settled in 900 ms");
        assert_eq!(settle_note(900, true, true, false), "still changing after 900 ms");
        assert_eq!(settle_note(900, false, false, false), "no reaction on screen");
    }

    #[test]
    fn an_open_is_loading_while_the_screen_before_it_is_still_up() {
        // The launcher still in front of a camera launch: not there yet.
        assert!(still_before(Some("launcher"), "launcher", Some("camera")));
        // Another package in front (the app, or a dialog it raised): arrived.
        assert!(!still_before(Some("launcher"), "camera", Some("camera")));
        assert!(!still_before(Some("launcher"), "permissioncontroller", Some("camera")));
        // Reopening the app already in front, or not an open: nothing to wait for.
        assert!(!still_before(Some("camera"), "camera", Some("camera")));
        assert!(!still_before(Some("launcher"), "launcher", None));
        assert!(!still_before(None, "launcher", Some("camera")));
    }

    #[test]
    fn open_reply_names_the_started_package() {
        assert_eq!(started_pkg("ok started=com.android.camera2/com.android.camera.CameraLauncher result=0").as_deref(),
                   Some("com.android.camera2"));
        assert_eq!(started_pkg("ok force-stop=x"), None);
    }

    #[test]
    fn open_waits_long_enough_for_a_cold_start_under_load() {
        // A loaded emulator took >6.8 s to draw Settings from a cold start.
        // Only the launch wait grows: a screen that never stops animating
        // still ends the settle at timeout_ms.
        let b = settle_budget("open");
        assert!(b.launch_ms >= 15_000);
        assert_eq!(b.timeout_ms, 6000);
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn initialize_echoes_known_version() {
        let r = handle(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#, &Engine::new(&[])).unwrap();
        assert!(r.contains(r#""protocolVersion":"2025-03-26""#), "{r}");
        let r = handle(r#"{"jsonrpc":"2.0","id":"a","method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#, &Engine::new(&[])).unwrap();
        assert!(r.contains(r#""id":"a""#) && r.contains(PROTOCOL_VERSIONS[0]), "{r}");
    }

    /// scripts/agent-bench.py's yardstick: ASCII chars / 4 (rounded up) + one per other char.
    fn approx_tokens(s: &str) -> usize {
        let ascii = s.chars().filter(|c| c.is_ascii()).count();
        ascii.div_ceil(4) + (s.chars().count() - ascii)
    }

    #[test]
    fn session_cost_stays_within_budget() {
        // Every session carries the instructions and the tool list; they
        // were 311 + 2668 before trimming (the bench counts tools/list with
        // JSON spacing; the raw reply here is a little smaller). Raise only with a reason.
        let list = handle(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, &Engine::new(&[])).unwrap();
        let cost = approx_tokens(INSTRUCTIONS) + approx_tokens(&list);
        assert!(cost <= 1900, "instructions + tools/list = {cost} approx tokens");
    }

    #[test]
    fn notifications_get_no_reply() {
        assert!(handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, &Engine::new(&[])).is_none());
    }

    #[test]
    fn tools_list_is_valid_json() {
        let r = handle(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, &Engine::new(&[])).unwrap();
        let v = json::parse(&r).expect("valid JSON");
        let tools = json::obj_get(json::obj_get(&v, "result").unwrap(), "tools").and_then(json::as_arr).unwrap();
        assert_eq!(tools.len(), super::tools().len());
    }

    /// The .mcpb manifest lists the tools for hosts to show before the
    /// server runs; keep it in step with `tools()`.
    #[test]
    fn mcpb_manifest_lists_every_tool() {
        let manifest = json::parse(include_str!("../../mcpb/manifest.json")).expect("manifest is JSON");
        let listed: Vec<&str> = json::obj_get(&manifest, "tools")
            .and_then(json::as_arr)
            .unwrap()
            .iter()
            .filter_map(|t| json::obj_get(t, "name").and_then(json::as_str))
            .collect();
        let served: Vec<&str> = super::tools().iter().map(|t| t.name).collect();
        assert_eq!(listed, served);
    }

    #[test]
    fn unknown_method_and_bad_args() {
        let r = handle(r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#, &Engine::new(&[])).unwrap();
        assert!(r.contains("-32601"), "{r}");
        let r = handle(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"tap","arguments":{}}}"#, &Engine::new(&[])).unwrap();
        assert!(r.contains("-32602"), "{r}");
        let r = handle(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"mobax","arguments":{"args":["tui"]}}}"#, &Engine::new(&[])).unwrap();
        assert!(r.contains(r#""isError":true"#), "{r}");
    }

    #[test]
    fn batch_checks_its_steps_before_touching_a_device() {
        let call = |args: &str| {
            handle(&format!(r#"{{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{{"name":"batch","arguments":{args}}}}}"#), &Engine::new(&[])).unwrap()
        };
        let r = call(r#"{"steps":[]}"#);
        assert!(r.contains("BAD_ARG") && r.contains(r#""isError":true"#), "{r}");
        let many = vec![r#"{"tool":"go","key":"back"}"#; super::BATCH_MAX + 1].join(",");
        assert!(call(&format!(r#"{{"steps":[{many}]}}"#)).contains("BAD_ARG"));
    }

    #[test]
    fn unknown_arguments_are_refused_before_anything_runs() {
        let call = |name: &str, args: &str| {
            handle(&format!(r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"name":"{name}","arguments":{args}}}}}"#), &Engine::new(&[])).unwrap()
        };
        let r = call("tap", r#"{"targt":"OK"}"#);
        assert!(r.contains("-32602") && r.contains("targt") && r.contains("target"), "{r}");
        // A typo in a later step stops the batch before its first step runs.
        let r = call("batch", r#"{"steps":[{"tool":"go","key":"back"},{"tool":"tap","targt":"OK"}]}"#);
        assert!(r.contains("BAD_ARG") && r.contains("targt") && r.contains(r#""isError":true"#), "{r}");
    }

    #[test]
    fn scrolling_stops_when_found_at_the_end_or_at_the_cap() {
        // Found on the third look: two scrolls.
        let mut looks = 0;
        let r = scroll_until(10, || { looks += 1; looks == 3 }, || Ok(()));
        assert_eq!(r, Scrolled { times: 2, found: true });
        // The list ends first.
        let mut scrolls = 0;
        let r = scroll_until(10, || false, || { scrolls += 1; if scrolls > 1 { Err(()) } else { Ok(()) } });
        assert_eq!(r, Scrolled { times: 1, found: false });
        // Never there: gives up after the cap.
        let r = scroll_until(3, || false, || Ok(()));
        assert_eq!(r, Scrolled { times: 3, found: false });
        // Already on screen: no scroll.
        assert_eq!(scroll_until(10, || true, || Ok(())), Scrolled { times: 0, found: true });
    }

    #[test]
    fn tap_and_find_take_scroll() {
        let has = |name: &str| super::tools().iter().find(|t| t.name == name).unwrap().schema.contains(r#""scroll""#);
        assert!(has("tap") && has("find"));
    }

    #[test]
    fn a_bare_word_is_one_with_no_selector_syntax() {
        for w in ["다음", "Gmail", "OK"] {
            assert!(bare_word(w), "{w}");
        }
        for w in ["@3", "#shutter_button", "Button[text=OK]", ":has-text(\"x\")", "two words", "a.b", ""] {
            assert!(!bare_word(w), "{w}");
        }
    }

    #[test]
    fn a_missed_bare_word_that_is_on_screen_as_text_gets_a_hint() {
        let h = class_word_hint("다음");
        assert!(h.contains(r#":has-text("다음")"#) && h.contains("class"), "{h}");
    }

    #[test]
    fn a_missed_tap_is_retried_only_if_the_screen_was_moving() {
        // Quiet screen: the target really isn't there; fail now.
        assert!(!moved_then_settled("ok elapsed=150 changed=0"));
        // It was drawing and has settled: look again.
        assert!(moved_then_settled("ok elapsed=840 changed=1"));
        // Still moving at the cap: look once more anyway.
        assert!(moved_then_settled("ERR:TIMEOUT:settle:idle_ms=300 timeout_ms=4000"));
        assert!(!moved_then_settled("ERR:DEVICE_GONE:x"));
    }

    #[test]
    fn toasts_after_an_action_become_one_line_each() {
        assert_eq!(toast_lines("저장됨\n2단계 남음"), "\ntoast: 저장됨\ntoast: 2단계 남음");
        assert_eq!(toast_lines(""), "");
        // An older daemon without `toasts`: nothing to add.
        assert_eq!(toast_lines("ERR:UNKNOWN_CMD:toasts"), "");
    }

    #[test]
    fn a_step_names_a_row_by_ref() {
        let step = |s: &str| json::parse(s).unwrap();
        assert!(super::uses_ref(&step(r#"{"tool":"tap","target":"@3"}"#)));
        assert!(super::uses_ref(&step(r#"{"tool":"fill","selector":"@12","text":"x"}"#)));
        assert!(!super::uses_ref(&step(r#"{"tool":"tap","target":"OK"}"#)));
        assert!(!super::uses_ref(&step(r#"{"tool":"type","text":"@3"}"#)));
    }
}
