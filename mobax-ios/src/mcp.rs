//! `mobax-ios mcp` — the iPhone and the iOS Simulator over MCP, with the same
//! tools and the same economy as `mobax mcp` for Android.
//!
//! What keeps it cheap is mobax-core's [Screen]: `ui` lists rows numbered
//! @1, @2, …, an action replies with only the rows that changed (+ added,
//! ~ changed, - removed) or, on another screen, the new table, and a ref
//! keeps its number while its row is on screen. WDA waits for the app to go
//! idle before it answers a source request, so one read after an action is
//! the settled screen.
//!
//! Transport: newline-delimited JSON-RPC 2.0 on stdin/stdout, one call at a
//! time; logs go to stderr.

use std::{collections::HashMap, sync::Arc, time::Duration};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use idevice::{provider::IdeviceProvider, wda::WdaClient};
use mobax_core::screen::{Change, Screen};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{sim, wda};

/// How long a screen may look as it did before an action before the reply
/// says nothing changed.
const NO_CHANGE: Duration = Duration::from_millis(1200);
/// How long after it first changed a screen may keep changing before it is
/// taken as it is.
const STILL_MOVING: Duration = Duration::from_millis(2000);
/// The most an action waits for its screen.
const SETTLE_MAX: Duration = Duration::from_secs(8);

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Drive an iPhone or iOS Simulator.\n\
\n\
`ui` lists the screen as rows numbered @1, @2, …: verb (tap / fill / - for text), \
element type, label (· value), #accessibility-id, centre x,y in points and state. \
Act on a row by its ref (tap target \"@3\"), its label or #id, or x,y.\n\
\n\
`tap`, `type`, `swipe`, `go` and `open` wait for the screen to settle and reply with \
what changed: + added, ~ changed, - removed, ↕ moved rows; or the whole new \
screen. You \
rarely need `ui` after an action. A ref stays valid while its row is on screen.\n\
\n\
iOS has no back key: `go back` taps the navigation bar's back button, else swipes \
from the left edge.";

/// What the server keeps between calls.
pub struct Engine {
    provider: Arc<dyn IdeviceProvider>,
    /// The simulator's udid, when the target is one.
    sim: Option<String>,
    screen: Option<Screen>,
    /// WDA's session, asked for once (it outlives us; a stale one is
    /// dropped on the first error that names it).
    sid: std::sync::Mutex<Option<String>>,
    /// The screen in points, from the last read.
    size: (i64, i64),
    /// The key of `screen`, and its rows as read.
    key: String,
    last_rows: Option<Vec<wda::Row>>,
    /// Each screen as last seen, by key, so coming back to one costs a line
    /// instead of its table.
    seen: HashMap<String, Screen>,
}

/// One read of the screen, before numbering.
struct Snap {
    key: String,
    title: String,
    keyboard: bool,
    rows: Vec<wda::Row>,
    /// The screen in points, from the root element.
    size: (i64, i64),
}

/// A tool's text, and whether it is an error.
type Reply = (bool, String);

pub async fn serve(provider: Arc<dyn IdeviceProvider>, sim: Option<String>) -> Result<(), String> {
    let mut engine = Engine {
        provider,
        sim,
        screen: None,
        sid: std::sync::Mutex::new(None),
        size: (0, 0),
        key: String::new(),
        last_rows: None,
        seen: HashMap::new(),
    };
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await.map_err(|e| format!("stdin: {e}"))? {
        if line.trim().is_empty() {
            continue;
        }
        let Some(reply) = engine.handle(&line).await else {
            continue;
        };
        out.write_all(reply.as_bytes())
            .await
            .map_err(|e| format!("stdout: {e}"))?;
        out.write_all(b"\n")
            .await
            .map_err(|e| format!("stdout: {e}"))?;
        out.flush().await.map_err(|e| format!("stdout: {e}"))?;
    }
    Ok(())
}

fn tools() -> Value {
    let target = json!({"type": "string", "description": "@N, label, or #id"});
    let int = json!({"type": "integer"});
    let timeout = json!({"type": "integer", "description": "ms (default 10000)"});
    let tool = |name: &str, description: &str, props: Value, required: &[&str]| {
        json!({"name": name, "description": description,
               "inputSchema": {"type": "object", "properties": props, "required": required}})
    };
    json!([
        tool("ui", "The screen as @N rows. The main way to read it.", json!({}), &[]),
        tool("tap", "Tap a target (@N, label, #id) or x,y.",
             json!({"target": target, "x": int, "y": int}), &[]),
        tool("type", "Type text into the focused field, any language.",
             json!({"text": {"type": "string"}}), &["text"]),
        tool("swipe", "Move the finger this way (up scrolls content down), from x,y (default: centre). The content stops with the finger; `fling` lets it coast.",
             json!({"direction": {"type": "string", "enum": ["up", "down", "left", "right"]},
                    "x": int, "y": int, "duration_ms": int, "fling": {"type": "boolean"}}), &["direction"]),
        tool("go", "Key: back, home, enter, hide_keyboard, volumeup, volumedown.",
             json!({"key": {"type": "string"}}), &["key"]),
        tool("open", "Launch an app by bundle id.", json!({"app": {"type": "string"}}), &["app"]),
        tool("close", "Terminate an app by bundle id.", json!({"app": {"type": "string"}}), &["app"]),
        tool("wait", "Wait until a row shows this text.",
             json!({"target": {"type": "string"}, "timeout_ms": timeout}), &["target"]),
        tool("watch", "Wait until the screen changes from what you last saw (or `until` a row shows this text), then reply with what changed.",
             json!({"until": {"type": "string"}, "timeout_ms": timeout}), &[]),
        tool("batch", "Several steps in one call, e.g. [{\"tool\":\"tap\",\"target\":\"@3\"},{\"tool\":\"type\",\"text\":\"hi\"}]. Stops at the first failure. One line per step, then what changed overall.",
             json!({"steps": {"type": "array", "maxItems": 20,
                              "items": {"type": "object", "properties": {"tool": {"type": "string",
                                  "enum": ["tap", "type", "swipe", "go", "open", "close", "wait"]}},
                                        "required": ["tool"]}}}), &["steps"]),
        tool("screenshot", "The screen as a PNG. Prefer `ui`.", json!({}), &[]),
        tool("info", "Device or simulator, iOS version, screen size.", json!({}), &[]),
    ])
}

impl Engine {
    async fn handle(&mut self, line: &str) -> Option<String> {
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(error(&Value::Null, -32700, &format!("parse error: {e}"))),
        };
        let id = req.get("id").cloned()?; // notifications get no reply
        let method = req["method"].as_str().unwrap_or("");
        let result = match method {
            "initialize" => {
                let asked = req["params"]["protocolVersion"].as_str().unwrap_or("");
                let version = PROTOCOL_VERSIONS
                    .iter()
                    .find(|v| **v == asked)
                    .unwrap_or(&PROTOCOL_VERSIONS[0]);
                json!({"protocolVersion": version, "capabilities": {"tools": {}},
                       "serverInfo": {"name": "mobileax-ios", "version": env!("CARGO_PKG_VERSION")},
                       "instructions": INSTRUCTIONS})
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools": tools()}),
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                let args = req["params"]["arguments"].clone();
                match name {
                    "screenshot" => match self.screenshot().await {
                        Ok(png) => json!({"content": [{"type": "image", "mimeType": "image/png",
                                                       "data": STANDARD.encode(png)}]}),
                        Err(e) => text(true, &e),
                    },
                    "batch" => {
                        let (failed, t) = self.batch(&args).await;
                        text(failed, &t)
                    }
                    _ => {
                        let (failed, t) = self.call(name, &args).await;
                        text(failed, &t)
                    }
                }
            }
            _ => return Some(error(&id, -32601, &format!("no method {method}"))),
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string())
    }

    /// Runs one tool; returns (is_error, text). A session WDA no longer
    /// knows (it restarted) is dropped and the tool run once more.
    async fn call(&mut self, name: &str, a: &Value) -> Reply {
        let (failed, text) = self.call_once(name, a).await;
        let stale = failed && (text.contains("nvalid session") || text.contains("does not exist"));
        if stale && self.sid.lock().unwrap().take().is_some() {
            return self.call_once(name, a).await;
        }
        (failed, text)
    }

    async fn call_once(&mut self, name: &str, a: &Value) -> Reply {
        let s = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string);
        let i = |k: &str| a.get(k).and_then(Value::as_i64);
        let r = match name {
            "ui" => self.ui().await,
            "tap" => match self.point(s("target").as_deref(), i("x"), i("y")).await {
                Ok((x, y, what)) => match self.tap(x, y).await {
                    Ok(()) => self.settled(&format!("tapped {what}")).await,
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            },
            "type" => match s("text") {
                Some(t) => self.type_text(&t).await,
                None => Err("type: `text` is required".into()),
            },
            "swipe" => {
                let fling = a.get("fling").and_then(Value::as_bool).unwrap_or(false);
                self.swipe(
                    s("direction").as_deref(),
                    i("x"),
                    i("y"),
                    i("duration_ms"),
                    fling,
                )
                .await
            }
            "go" => self.go(s("key").as_deref().unwrap_or("")).await,
            "open" | "close" => match s("app") {
                Some(app) => match self.app(name, &app).await {
                    Ok(()) => self.settled(&format!("{name} {app}")).await,
                    Err(e) => Err(e),
                },
                None => Err(format!("{name}: `app` is required")),
            },
            "wait" => self.wait(s("target"), i("timeout_ms"), false).await,
            "watch" => self.wait(s("until"), i("timeout_ms"), true).await,
            "info" => self.info().await,
            other => Err(format!("no tool {other}")),
        };
        match r {
            Ok(t) => (false, t),
            Err(e) => (true, e),
        }
    }

    /// The screen now, unnumbered. Its key is the app, the navigation bar's
    /// title and any alert or sheet over it: another key is another screen.
    async fn snap(&self) -> Result<Snap, String> {
        let t0 = std::time::Instant::now();
        let (mut tree, mut rows) = wda::screen(&*self.provider).await?;
        // An app opened outside WDA (simctl, a person) can leave WDA reading
        // the home screen's launch card ("card:<bundle>:sceneID…") for
        // seconds. Activating that app through WDA (~60 ms) points it there.
        let card = rows
            .iter()
            .find_map(|r| r.id.strip_prefix("card:"))
            .and_then(|c| c.split(':').next())
            .map(str::to_string);
        if let Some(bundle) = card {
            if let Ok((_, sid)) = self.client().await {
                if wda::app(&*self.provider, &sid, "activate", &bundle)
                    .await
                    .is_ok()
                {
                    (tree, rows) = wda::screen(&*self.provider).await?;
                }
            }
        }
        if std::env::var_os("MOBAX_IOS_TRACE").is_some() {
            eprintln!(
                "source {} ms, {} rows",
                t0.elapsed().as_millis(),
                rows.len()
            );
        }

        let app = match tree["label"].as_str().unwrap_or("").trim() {
            "" => "home".to_string(),
            a => a.to_string(),
        };
        if app == "home" {
            // The status bar: clock, signal, battery.
            rows.retain(|r| r.verb != "-" || r.bounds.3 > 54);
        }
        let mut kinds = Vec::new();
        any_type(&tree, &mut kinds);
        let over: Vec<&str> = ["Alert", "Sheet"]
            .into_iter()
            .filter(|k| kinds.iter().any(|x| x == k))
            .collect();
        let nav = find_type(&tree, "NavigationBar")
            .and_then(|n| {
                [n["name"].as_str(), n["label"].as_str()]
                    .into_iter()
                    .flatten()
                    .map(str::trim)
                    .find(|t| !t.is_empty())
            })
            .unwrap_or("")
            .to_string();
        let mut title = app.clone();
        if !nav.is_empty() && nav != app {
            title.push_str(&format!(" › {nav}"));
        }
        for o in &over {
            title.push_str(&format!(" · {}", o.to_lowercase()));
        }
        let dim = |k: &str| tree["rect"][k].as_f64().unwrap_or(0.0) as i64;
        Ok(Snap {
            key: format!("{app}|{nav}|{}", over.join(",")),
            title,
            keyboard: kinds.iter().any(|k| k == "Keyboard"),
            rows,
            size: (dim("width"), dim("height")),
        })
    }

    /// Numbers `snap` against the screen it continues (the current one, or
    /// the same screen as last seen), makes it current, and says how it
    /// differs from that.
    fn adopt(&mut self, snap: Snap, note: &str) -> String {
        let returning = snap.key != self.key;
        let prev = if returning {
            self.seen.get(&snap.key)
        } else {
            self.screen.as_ref()
        };
        let screen = Screen::from_rows(
            snap.key.clone(),
            snap.title,
            snap.keyboard,
            wda::ui_rows(&snap.rows),
            prev,
        );
        let change = screen.change_from(prev);
        self.last_rows = Some(snap.rows);
        if snap.size.0 > 0 && snap.size.1 > 0 {
            self.size = snap.size;
        }
        let out = match (returning, change) {
            (true, Change::Same) => format!(
                "{note} · {} · back as last seen, {} rows (`ui` lists them)",
                screen.title(),
                screen.len()
            ),
            (true, Change::Rows(lines)) => format!(
                "{note} · {} · back; since last seen:\n{}",
                screen.title(),
                lines.join("\n")
            ),
            (_, change) => screen.describe(note, change),
        };
        self.key = snap.key.clone();
        self.seen.insert(snap.key, screen.clone());
        self.screen = Some(screen);
        out
    }

    /// The whole table of the screen now.
    async fn ui(&mut self) -> Result<String, String> {
        let snap = self.snap().await?;
        self.adopt(snap, "ui");
        let screen = self.screen.as_ref().expect("just adopted");
        Ok(format!(
            "{} · {} rows\n{}",
            screen.title(),
            screen.len(),
            screen.table().trim_end()
        ))
    }

    /// After an action: the settled screen, described as what changed.
    ///
    /// Reads back to back, with no fixed waits: WDA answers a source request
    /// once the app is idle, so a read is already paced by the app. The
    /// screen has settled when it differs from before the action and two
    /// reads in a row agree. One that still looks as before after
    /// [NO_CHANGE] is reported unchanged; one that keeps changing (a
    /// spinner, a clock) is taken as it is after [STILL_MOVING].
    async fn settled(&mut self, note: &str) -> Result<String, String> {
        let t0 = tokio::time::Instant::now();
        let mut first_change: Option<tokio::time::Instant> = None;
        let mut prev: Option<Snap> = None;
        loop {
            let snap = self.snap().await?;
            let now = tokio::time::Instant::now();
            let changed = snap.key != self.key || Some(&snap.rows) != self.last_rows.as_ref();
            // An app's launch card ("card:<bundle>…") is mid-animation.
            let moving = snap.rows.iter().any(|r| r.id.starts_with("card:"));
            if changed && !moving {
                let since = *first_change.get_or_insert(now);
                let agreed = prev
                    .as_ref()
                    .is_some_and(|p| p.key == snap.key && p.rows == snap.rows);
                if agreed || now - since >= STILL_MOVING {
                    return Ok(self.adopt(snap, note));
                }
            } else if !changed && now - t0 >= NO_CHANGE {
                return Ok(self.adopt(snap, note));
            }
            if now - t0 >= SETTLE_MAX {
                return Ok(self.adopt(snap, note));
            }
            prev = Some(snap);
        }
    }

    /// Where `target` (or x,y) is, and how to say it.
    ///
    /// A target by ref or label is looked up in a fresh read (0.1–0.2 s), not
    /// in the rows last seen: a list that moved since (a banner slid in at the
    /// top) would otherwise take the tap on the wrong row. A ref is found by
    /// its selector, nearest where it was; if it is gone, nothing is tapped
    /// and the reply says what changed.
    async fn point(
        &mut self,
        target: Option<&str>,
        x: Option<i64>,
        y: Option<i64>,
    ) -> Result<(f64, f64, String), String> {
        if let (Some(x), Some(y)) = (x, y) {
            return Ok((x as f64, y as f64, format!("{x},{y}")));
        }
        let t = target.ok_or("tap: give `target` or `x` and `y`")?;
        if let Some((x, y)) = t
            .split_once(',')
            .and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)))
        {
            return Ok((x, y, t.to_string()));
        }
        let fresh = self.snap().await?;
        if let Some(n) = t.strip_prefix('@').and_then(|n| n.parse::<u32>().ok()) {
            let r = self
                .screen
                .as_ref()
                .and_then(|s| s.get(n))
                .ok_or_else(|| format!("no @{n} on the screen last seen; call `ui`"))?;
            let (sel, ox, oy) = (r.sel.clone(), r.x, r.y);
            let hit = fresh
                .rows
                .iter()
                .filter(|row| match sel.strip_prefix('#') {
                    Some(id) => row.id == id,
                    None => row.label == sel,
                })
                .min_by_key(|row| (row.x - ox).abs() + (row.y - oy).abs());
            return match hit {
                Some(row) => Ok((
                    row.x as f64,
                    row.y as f64,
                    format!("@{n} ({},{})", row.x, row.y),
                )),
                None => Err(self.adopt(
                    fresh,
                    &format!("@{n} is no longer on screen; nothing tapped"),
                )),
            };
        }
        match wda::pick(&fresh.rows, t) {
            Some(r) => Ok((r.x as f64, r.y as f64, format!("{t:?} ({},{})", r.x, r.y))),
            None => Err(format!("nothing labelled {t:?} on screen")),
        }
    }

    async fn type_text(&mut self, t: &str) -> Result<String, String> {
        let (w, sid) = self.client().await?;
        w.send_keys(t, Some(&sid)).await.map_err(werr)?;
        self.settled("typed").await
    }

    async fn tap(&self, x: f64, y: f64) -> Result<(), String> {
        let t0 = std::time::Instant::now();
        let (_, sid) = self.client().await?;
        let t1 = t0.elapsed().as_millis();
        let r = wda::tap(&*self.provider, &sid, x, y).await;
        if std::env::var_os("MOBAX_IOS_TRACE").is_some() {
            eprintln!(
                "tap: session {t1} ms, action {} ms",
                t0.elapsed().as_millis() - t1
            );
        }
        r
    }

    /// A WDA client and its session.
    async fn client(&self) -> Result<(WdaClient<'_>, String), String> {
        let cached = self.sid.lock().unwrap().clone();
        if let Some(sid) = cached {
            return Ok((WdaClient::new(&*self.provider), sid));
        }
        let (w, sid) = wda::session(&*self.provider).await?;
        *self.sid.lock().unwrap() = Some(sid.clone());
        Ok((w, sid))
    }

    /// The screen in points: from the last read, else WDA's window size.
    async fn size(&self) -> (i64, i64) {
        if self.size.0 > 0 && self.size.1 > 0 {
            return self.size;
        }
        let s = match self.client().await {
            Ok((w, sid)) => w.window_size(Some(&sid)).await.unwrap_or_default(),
            Err(_) => Value::Null,
        };
        let f = |k: &str| s.get(k).and_then(Value::as_f64).unwrap_or(0.0) as i64;
        match (f("width"), f("height")) {
            (w, h) if w > 0 && h > 0 => (w, h),
            _ => (390, 844),
        }
    }

    async fn swipe(
        &mut self,
        dir: Option<&str>,
        x: Option<i64>,
        y: Option<i64>,
        ms: Option<i64>,
        fling: bool,
    ) -> Result<String, String> {
        let (w, h) = self.size().await;
        let (x, y) = (x.unwrap_or(w / 2), y.unwrap_or(h / 2));
        let (dx, dy) = match dir {
            Some("up") => (0, -h * 2 / 5),
            Some("down") => (0, h * 2 / 5),
            Some("left") => (-w * 3 / 5, 0),
            Some("right") => (w * 3 / 5, 0),
            _ => return Err("swipe: `direction` is up, down, left or right".into()),
        };
        let clamp = |v: i64, max: i64| v.clamp(1, max - 1);
        let (x2, y2) = (clamp(x + dx, w), clamp(y + dy, h));
        let (_, sid) = self.client().await?;
        wda::swipe(
            &*self.provider,
            &sid,
            (x, y),
            (x2, y2),
            ms.unwrap_or(250),
            fling,
        )
        .await?;
        self.settled(&format!("swiped {}", dir.unwrap_or(""))).await
    }

    async fn go(&mut self, key: &str) -> Result<String, String> {
        if key == "back" {
            let rows = match &self.last_rows {
                Some(r) => r.clone(),
                None => wda::screen(&*self.provider).await?.1,
            };
            let back = rows
                .iter()
                .find(|r| r.id == "BackButton" || (r.class == "Button" && r.label == "Back"));
            match back {
                Some(r) => {
                    let (x, y) = (r.x as f64, r.y as f64);
                    self.tap(x, y).await?;
                }
                None => {
                    // The edge swipe every UINavigationController takes.
                    let (w, h) = self.size().await;
                    let (_, sid) = self.client().await?;
                    wda::swipe(
                        &*self.provider,
                        &sid,
                        (1, h / 2),
                        (w * 3 / 4, h / 2),
                        250,
                        false,
                    )
                    .await?;
                }
            }
            return self.settled("back").await;
        }
        if key == "enter" {
            let (w, sid) = self.client().await?;
            w.send_keys("\n", Some(&sid)).await.map_err(werr)?;
            return self.settled("enter").await;
        }
        if key == "hide_keyboard" {
            let (_, sid) = self.client().await?;
            wda::hide_keyboard(&*self.provider, &sid)
                .await
                .map_err(|e| format!("{e}\nTap the keyboard's own close button (a row on screen), or `go enter`."))?;
            return self.settled("keyboard hidden").await;
        }
        let key = key.to_string();
        let k = key.clone();
        {
            let (w, sid) = self.client().await?;
            w.press_button(&k, Some(&sid)).await.map_err(werr)
        }?;
        self.settled(&key).await
    }

    /// Through WDA, so its idea of the active app follows; on a simulator
    /// simctl stands in when WDA is not up.
    async fn app(&self, verb: &str, app: &str) -> Result<(), String> {
        let action = if verb == "open" {
            "launch"
        } else {
            "terminate"
        };
        match self.client().await {
            Ok((_, sid)) => wda::app(&*self.provider, &sid, action, app).await,
            Err(e) => match &self.sim {
                Some(u) => sim::simctl(&[action, u, app]).map(|_| ()),
                None => Err(e),
            },
        }
    }

    /// `wait`: until a row shows `text`. `watch`: until the screen differs
    /// from the last one seen, or shows `text`.
    async fn wait(
        &mut self,
        text: Option<String>,
        timeout: Option<i64>,
        watch: bool,
    ) -> Result<String, String> {
        if !watch && text.is_none() {
            return Err("wait: `target` is required".into());
        }
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(timeout.unwrap_or(10_000).max(0) as u64);
        loop {
            let snap = self.snap().await?;
            let hit = match &text {
                Some(t) => {
                    let t = t.to_lowercase();
                    snap.rows
                        .iter()
                        .any(|r| r.line().to_lowercase().contains(&t))
                }
                None => snap.key != self.key || Some(&snap.rows) != self.last_rows.as_ref(),
            };
            if hit {
                let note = match &text {
                    Some(t) => format!("{t:?} is on screen"),
                    None => "changed".to_string(),
                };
                let title = snap.title.clone();
                let out = self.adopt(snap, &note);
                return Ok(if watch {
                    out
                } else {
                    format!("{note} · {title}")
                });
            }
            if tokio::time::Instant::now() >= deadline {
                let what = text.map_or("a change".to_string(), |t| format!("{t:?}"));
                return Err(format!("ERR:TIMEOUT:no {what} within the timeout"));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    async fn batch(&mut self, args: &Value) -> Reply {
        let steps = args["steps"].as_array().cloned().unwrap_or_default();
        if steps.is_empty() {
            return (true, "batch: `steps` is empty".into());
        }
        let before = self.screen.clone();
        let mut lines = Vec::new();
        for (n, step) in steps.iter().enumerate() {
            let tool = step["tool"].as_str().unwrap_or("");
            if !matches!(
                tool,
                "tap" | "type" | "swipe" | "go" | "open" | "close" | "wait"
            ) {
                lines.push(format!("{}. {tool}: not allowed in a batch", n + 1));
                return (true, lines.join("\n"));
            }
            let (failed, text) = self.call(tool, step).await;
            let first = text.lines().next().unwrap_or("").to_string();
            lines.push(format!("{}. {first}", n + 1));
            if failed {
                return (true, lines.join("\n"));
            }
        }
        if let Some(now) = &self.screen {
            lines.push(now.describe("after the batch", now.change_from(before.as_ref())));
        }
        (false, lines.join("\n"))
    }

    async fn screenshot(&self) -> Result<Vec<u8>, String> {
        if let Some(u) = &self.sim {
            let path = std::env::temp_dir().join(format!("mobax-ios-{u}.png"));
            let p = path.to_string_lossy().to_string();
            sim::simctl(&["io", u, "screenshot", "--type=png", &p])?;
            let png = std::fs::read(&path).map_err(|e| format!("{p}: {e}"))?;
            let _ = std::fs::remove_file(&path);
            return Ok(png);
        }
        WdaClient::new(&*self.provider)
            .screenshot(None)
            .await
            .map_err(|e| format!("WDA: {e}"))
    }

    async fn info(&self) -> Result<String, String> {
        let status = WdaClient::new(&*self.provider)
            .status()
            .await
            .map_err(|e| format!("WDA: {e}"))?;
        let os = status
            .pointer("/value/os/version")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let (w, h) = self.size().await;
        let kind = match &self.sim {
            Some(u) => format!("simulator {u}"),
            None => "iPhone".to_string(),
        };
        Ok(format!("{kind} · iOS {os} · {w}x{h} pt"))
    }
}

/// The first node of type `ty`, depth first.
fn find_type<'a>(n: &'a Value, ty: &str) -> Option<&'a Value> {
    let t = n["type"].as_str().unwrap_or("");
    if t.strip_prefix("XCUIElementType").unwrap_or(t) == ty {
        return Some(n);
    }
    n["children"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|k| find_type(k, ty))
}

/// Every element type in the tree, without the XCUIElementType prefix.
fn any_type(n: &Value, out: &mut Vec<String>) {
    if let Some(t) = n["type"].as_str() {
        let t = t.strip_prefix("XCUIElementType").unwrap_or(t);
        if !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    for k in n["children"].as_array().into_iter().flatten() {
        any_type(k, out);
    }
}

fn werr(e: idevice::IdeviceError) -> String {
    format!("WDA: {e}")
}

fn text(is_error: bool, t: &str) -> Value {
    json!({"content": [{"type": "text", "text": t}], "isError": is_error})
}

fn error(id: &Value, code: i32, msg: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}}).to_string()
}
