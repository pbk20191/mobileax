//! WebDriverAgent: the runner, a session, and the screen as a table.
//!
//! idevice starts the runner the way Xcode does (testmanagerd; on iOS 17+
//! through CoreDeviceProxy and a userspace TCP stack, so no admin rights) and
//! reaches WDA's port on the device through usbmuxd, without iproxy. The
//! runner lives as long as the process that started it: `mobax-ios wda`
//! stays in the foreground, and the other verbs talk to WDA directly.
//!
//! Coordinates are points, as WDA takes them (a screenshot is points × scale).

use std::{sync::Arc, time::Duration};

use idevice::{
    dvt::xctest::{TestConfig, WdaRunHandle, XCUITestService},
    installation_proxy::InstallationProxyClient,
    provider::IdeviceProvider,
    wda::{WdaClient, DEFAULT_WDA_PORT},
    IdeviceService,
};
use serde_json::Value;

const RUNNER_SUFFIX: &str = "WebDriverAgentRunner.xctrunner";
const READY_TIMEOUT: Duration = Duration::from_secs(90);

/// The installed WDA runner's bundle id: `want` if given, else the only app
/// whose id ends with `WebDriverAgentRunner.xctrunner`.
pub async fn runner_id(
    provider: &dyn IdeviceProvider,
    want: Option<&str>,
) -> Result<String, String> {
    if let Some(w) = want {
        return Ok(w.to_string());
    }
    let mut inst = InstallationProxyClient::connect(provider)
        .await
        .map_err(|e| format!("installation_proxy: {e}"))?;
    let apps = inst
        .get_apps(Some("User"), None)
        .await
        .map_err(|e| format!("installation_proxy: {e}"))?;
    let mut found: Vec<&String> = apps
        .keys()
        .filter(|id| id.ends_with(RUNNER_SUFFIX))
        .collect();
    match found.len() {
        0 => Err(format!(
            "no *{RUNNER_SUFFIX} installed; sign one with scripts/ios-wda-sign.sh and `mobax-ios install` it"
        )),
        1 => Ok(found.remove(0).clone()),
        _ => Err(format!("several WDA runners installed ({found:?}); pass --runner ID")),
    }
}

/// Starts the runner and returns once WDA answers `/status`.
pub async fn start(
    provider: Arc<dyn IdeviceProvider>,
    runner: &str,
) -> Result<WdaRunHandle, String> {
    let mut inst = InstallationProxyClient::connect(&*provider)
        .await
        .map_err(|e| format!("installation_proxy: {e}"))?;
    let cfg = TestConfig::from_installation_proxy(&mut inst, runner, None)
        .await
        .map_err(|e| format!("{runner}: {e}"))?;
    XCUITestService::new(provider)
        .run_until_wda_ready(cfg, READY_TIMEOUT)
        .await
        .map_err(|e| format!("starting {runner}: {e}"))
}

/// A WDA client with a session: the one WDA already has, or a new one.
pub async fn session(provider: &dyn IdeviceProvider) -> Result<(WdaClient<'_>, String), String> {
    let mut wda = WdaClient::new(provider).with_timeout(Duration::from_secs(30));
    let status = wda.status().await.map_err(|e| {
        format!(
            "WDA is not answering on port {DEFAULT_WDA_PORT} ({e}); start it with `mobax-ios wda`"
        )
    })?;
    let existing = status
        .get("sessionId")
        .or_else(|| status.get("value").and_then(|v| v.get("sessionId")))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let id = match existing {
        Some(id) => id,
        None => wda
            .start_session(None)
            .await
            .map_err(|e| format!("WDA session: {e}"))?,
    };
    Ok((wda, id))
}

/// The tree (in the JSON source's shape) and its rows.
///
/// The read leaves out `isVisible`: WDA works it out element by element,
/// which is most of a snapshot's time (1.35 s → 0.24 s on a Settings page).
/// rows() then drops what lies off screen or parked in a stack, and of two
/// controls that overlap the one drawn under (search suggestions over the
/// list they dim, a sheet over the screen it covers, a floating search bar
/// over the last cell). A view that is in the tree but hidden would hide
/// what is under it too; none has been seen, and reading `isVisible`
/// whenever controls overlap (the floating search bar alone does, on every
/// Settings screen) made the whole run slower than before.
pub async fn screen(provider: &dyn IdeviceProvider) -> Result<(Value, Vec<Row>), String> {
    let tree = source(provider).await?;
    let (rows, _) = build(&tree);
    Ok((tree, rows))
}

async fn source(provider: &dyn IdeviceProvider) -> Result<Value, String> {
    // XML, not JSON: an eighth of the bytes and half the time (see xml.rs).
    let body = get(
        provider,
        "/source?format=xml&excluded_attributes=visible,accessible,traits,customActions,nativeFrame,placeholderValue",
    )
    .await?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| format!("WDA source: {e}"))?;
    let xml = v["value"].as_str().ok_or("WDA source: no XML")?;
    crate::xml::tree(xml).map_err(|e| format!("WDA source: {e}"))
}

/// Whether `top`, later in the tree, may be drawn over `a`: it is not
/// inside `a`, and covers a third of it.
fn covers(top: &Row, a: &Row) -> bool {
    if top.path.starts_with(&a.path) {
        return false;
    }
    let (al, at, ar, ab) = a.bounds;
    let (bl, bt, br, bb) = top.bounds;
    let w = ar.min(br) - al.max(bl);
    let h = ab.min(bb) - at.max(bt);
    // A third of `a` itself: a small button on a cell does not hide the cell.
    w > 0 && h > 0 && w * h * 3 > a.area()
}

/// Taps a point: a W3C touch held for 50 ms. 0.4 s against 0.52 s for
/// `/wda/tap`; held for 0 ms (as WdaClient's fallback sends it) a UISwitch
/// does not always take it, held for 50 ms it took 6 of 6.
pub async fn tap(
    provider: &dyn IdeviceProvider,
    session: &str,
    x: f64,
    y: f64,
) -> Result<(), String> {
    touch(
        provider,
        session,
        serde_json::json!([
            {"type": "pointerMove", "duration": 0, "x": x, "y": y},
            {"type": "pointerDown"},
            {"type": "pause", "duration": 50},
            {"type": "pointerUp"},
        ]),
    )
    .await
}

/// Drags from one point to another over `ms`. Without `fling` the finger
/// then creeps one more point over 50 ms before lifting: it leaves at no
/// speed, so the content stops where the finger did. Holding still does not
/// do that (XCTest keeps the last speed and the list coasts for 1.5 s); the
/// creep does, and a scroll then goes as far as the drag, every time. With
/// `fling` the finger lifts moving, for a long scroll.
pub async fn swipe(
    provider: &dyn IdeviceProvider,
    session: &str,
    from: (i64, i64),
    to: (i64, i64),
    ms: i64,
    fling: bool,
) -> Result<(), String> {
    let mut actions = vec![
        serde_json::json!({"type": "pointerMove", "duration": 0, "x": from.0, "y": from.1}),
        serde_json::json!({"type": "pointerDown"}),
        serde_json::json!({"type": "pointerMove", "duration": ms.max(50), "x": to.0, "y": to.1}),
    ];
    if !fling {
        let step = |a: i64, b: i64| (b - a).signum();
        actions.push(serde_json::json!({"type": "pointerMove", "duration": 50,
            "x": to.0 + step(from.0, to.0), "y": to.1 + step(from.1, to.1)}));
    }
    actions.push(serde_json::json!({"type": "pointerUp"}));
    touch(provider, session, Value::Array(actions)).await
}

async fn touch(
    provider: &dyn IdeviceProvider,
    session: &str,
    actions: Value,
) -> Result<(), String> {
    let body = serde_json::json!({"actions": [{
        "type": "pointer", "id": "finger1",
        "parameters": {"pointerType": "touch"},
        "actions": actions,
    }]})
    .to_string();
    request(
        provider,
        "POST",
        &format!("/session/{session}/actions"),
        Some(&body),
    )
    .await
    .map(|_| ())
}

/// Dismisses the on-screen keyboard (where the keyboard has a way to).
pub async fn hide_keyboard(provider: &dyn IdeviceProvider, session: &str) -> Result<(), String> {
    request(
        provider,
        "POST",
        &format!("/session/{session}/wda/keyboard/dismiss"),
        Some("{}"),
    )
    .await
    .map(|_| ())
}

/// `launch`, `terminate` or `activate` an app with WDA's own endpoints.
/// (WdaClient's app calls go through Appium's `mobile:` scripts, which WDA
/// alone answers with 404.)
pub async fn app(
    provider: &dyn IdeviceProvider,
    session: &str,
    action: &str,
    bundle_id: &str,
) -> Result<(), String> {
    let body = serde_json::json!({ "bundleId": bundle_id }).to_string();
    request(
        provider,
        "POST",
        &format!("/session/{session}/wda/apps/{action}"),
        Some(&body),
    )
    .await
    .map(|_| ())
}

async fn get(provider: &dyn IdeviceProvider, path: &str) -> Result<Vec<u8>, String> {
    request(provider, "GET", path, None).await
}

/// A request to WDA, for what WdaClient does not cover. Like WdaClient it
/// reads a Content-Length body or until the connection closes.
async fn request(
    provider: &dyn IdeviceProvider,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<Vec<u8>, String> {
    let mut dev = provider
        .connect(DEFAULT_WDA_PORT)
        .await
        .map_err(|e| format!("WDA port {DEFAULT_WDA_PORT}: {e}"))?;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some(b) = body {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            b.len()
        ));
    }
    req.push_str("\r\n");
    req.push_str(body.unwrap_or(""));
    dev.send_raw(req.as_bytes())
        .await
        .map_err(|e| format!("WDA: {e}"))?;
    let mut resp = Vec::new();
    let read = async {
        loop {
            let chunk = dev
                .read_any(64 * 1024)
                .await
                .map_err(|e| format!("WDA: {e}"))?;
            if chunk.is_empty() {
                break;
            }
            resp.extend_from_slice(&chunk);
            if head_and_length(&resp).is_some_and(|(head, len)| resp.len() >= head + len) {
                break;
            }
        }
        Ok::<(), String>(())
    };
    tokio::time::timeout(Duration::from_secs(60), read)
        .await
        .map_err(|_| "WDA: timed out".to_string())??;
    let (head, _) = head_and_length(&resp)
        .or_else(|| find(&resp, b"\r\n\r\n").map(|i| (i + 4, resp.len() - i - 4)))
        .ok_or("WDA: no HTTP response")?;
    let status = String::from_utf8_lossy(&resp[..head]);
    let code = status.split_whitespace().nth(1).unwrap_or("");
    if !code.starts_with('2') {
        // WDA's error is JSON with a traceback; its message is what matters.
        let body: Value = serde_json::from_slice(&resp[head..]).unwrap_or(Value::Null);
        let msg = body
            .pointer("/value/message")
            .and_then(Value::as_str)
            .map(|m| m.split(" UserInfo=").next().unwrap_or(m).to_string())
            .unwrap_or_else(|| String::from_utf8_lossy(&resp[head..]).into_owned());
        let what = path.rsplit('/').next().unwrap_or(path);
        return Err(format!("WDA {what}: HTTP {code}: {msg}"));
    }
    Ok(resp[head..].to_vec())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Header length and Content-Length, once the headers are in.
fn head_and_length(resp: &[u8]) -> Option<(usize, usize)> {
    let end = find(resp, b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&resp[..end]);
    let len = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| v.trim().parse().ok())?
    })?;
    Some((end + 4, len))
}

/// One row of the `ui` table.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// `tap`, `fill` or `-`, as in mobax's Android table.
    pub verb: &'static str,
    /// The XCUIElementType without its prefix: Button, Cell, StaticText…
    pub class: String,
    pub label: String,
    /// The value when it differs from the label: a field's text, a slider's level.
    pub value: String,
    /// The accessibility identifier, when it says more than the label.
    pub id: String,
    pub x: i64,
    pub y: i64,
    /// left, top, right, bottom.
    pub bounds: (i64, i64, i64, i64),
    pub flags: Vec<&'static str>,
    /// Child indices from the root: tells a descendant from an element
    /// drawn over another.
    pub path: Vec<u16>,
}

impl Row {
    /// The table line. No column padding: it costs tokens, and a wider row
    /// appearing would change every line and defeat the change lists.
    pub fn line(&self) -> String {
        let mut s = format!("{}  {}  {}", self.verb, self.class, quote(&self.label));
        if !self.value.is_empty() {
            s.push_str(&format!(" · {}", quote(&self.value)));
        }
        if !self.id.is_empty() {
            s.push_str(&format!("  #{}", self.id));
        }
        s.push_str(&format!("  {},{}", self.x, self.y));
        if !self.flags.is_empty() {
            s.push_str(&format!("  [{}]", self.flags.join(" ")));
        }
        s
    }

    /// What identifies the row across observations (for mobax-core's
    /// `@N` refs): verb, type, and the id or else the label.
    pub fn ident(&self) -> String {
        let key = if self.id.is_empty() {
            &self.label
        } else {
            &self.id
        };
        format!("{}|{}|{}", self.verb, self.class, key)
    }

    /// How much this row is worth keeping over another with the same label
    /// on the same spot: a field or a switch says the most, then anything
    /// tappable, then text.
    fn rank(&self) -> u8 {
        match (self.verb, self.class.as_str()) {
            ("fill", _) | (_, "Switch" | "Toggle" | "Slider") => 3,
            ("tap", _) => 2,
            _ => 1,
        }
    }

    fn holds(&self, x: i64, y: i64) -> bool {
        let (l, t, r, b) = self.bounds;
        l <= x && x <= r && t <= y && y <= b
    }

    fn area(&self) -> i64 {
        let (l, t, r, b) = self.bounds;
        (r - l) * (b - t)
    }

    /// Whether `want` names this row: the label or id, exactly, then as a prefix.
    fn named(&self, want: &str, exact: bool) -> bool {
        let id = want.strip_prefix('#');
        match (id, exact) {
            (Some(id), true) => self.id == id,
            (Some(id), false) => self.id.starts_with(id),
            (None, true) => self.label == want || self.id == want,
            (None, false) => self.label.starts_with(want),
        }
    }
}

/// Longest label a line shows; the rest is elided. `pick` still sees it all.
const LABEL_MAX: usize = 60;

/// A label in double quotes, on one line, without Rust's escapes for
/// non-ASCII spaces (each costs tokens and says nothing).
fn quote(s: &str) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect::<String>()
        .split(' ')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut out: String = flat.chars().take(LABEL_MAX).collect();
    if flat.chars().count() > LABEL_MAX {
        out.push('…');
    }
    format!("\"{}\"", out.replace('"', "\\\""))
}

const TAPPABLE: &[&str] = &[
    "Button",
    "Cell",
    "Link",
    "Switch",
    "Toggle",
    "Tab",
    "Key",
    "SegmentedControl",
    "MenuItem",
    "PickerWheel",
    "Slider",
    "Stepper",
    "Icon",
];
const FILLABLE: &[&str] = &["TextField", "SecureTextField", "SearchField"];

/// The tree flattened to what can be read or acted on, in tree order:
/// elements with a label or value, and controls even without one.
/// Containers without text, elements off screen, hidden or zero-sized, and
/// scroll indicators are left out.
///
/// One thing on screen is often several elements: a settings row is a cell
/// holding a button with the label, an icon and a chevron, or a cell, a
/// switch and the switch's knob. So a control without a label takes the
/// first label inside it, an unlabelled control inside another is dropped
/// (tapping the outer one covers it), of rows with the same label on the
/// same spot only the one that says the most is kept, and text that repeats
/// part of a row around it goes.
#[cfg(test)]
pub fn rows(tree: &Value) -> Vec<Row> {
    build(tree).0
}

/// The rows, and whether two controls overlapped (the lower one is left
/// out, as drawn over).
fn build(tree: &Value) -> (Vec<Row>, bool) {
    let screen = rect(tree).map(|(_, _, w, h)| (w, h));
    let mut all = Vec::new();
    walk(tree, screen, false, &mut Vec::new(), &mut all);
    // The keyboard's globe and dictation keys sit below its frame.
    if let Some((_, top, _, _)) = keyboard(tree) {
        all.retain(|r| (r.y as f64) < top);
    }
    // Without WDA's `isVisible` (it costs most of a snapshot), hidden
    // elements show as stacks: reusable cells a table keeps off screen sit
    // at one stale frame. Three of a kind on one rect is such a stack.
    type Spot<'a> = (&'a str, (i64, i64, i64, i64));
    let mut stacks: std::collections::HashMap<Spot, usize> = std::collections::HashMap::new();
    for r in &all {
        *stacks.entry((r.class.as_str(), r.bounds)).or_default() += 1;
    }
    let stacked: Vec<bool> = all
        .iter()
        .map(|r| stacks[&(r.class.as_str(), r.bounds)] >= 3)
        .collect();
    let mut all: Vec<Row> = all
        .into_iter()
        .zip(stacked)
        .filter_map(|(r, s)| (!s).then_some(r))
        .collect();
    let mut keep = vec![true; all.len()];
    for i in 0..all.len() {
        for j in 0..all.len() {
            if i == j || !keep[i] || !keep[j] {
                continue;
            }
            let (a, b) = (&all[i], &all[j]);
            if a.label.is_empty() || b.label.is_empty() {
                continue;
            }
            if a.label == b.label && (a.holds(b.x, b.y) || b.holds(a.x, a.y)) {
                // The lower rank goes; on a tie the later (inner) one.
                if b.rank() < a.rank() || (b.rank() == a.rank() && j > i) {
                    keep[j] = false;
                    if all[i].id.is_empty() && !all[j].id.is_empty() {
                        all[i].id = all[j].id.clone();
                    }
                }
            } else if a.holds(b.x, b.y)
                && a.area() >= b.area()
                && ((b.verb == "-" && (a.label.contains(&b.label) || a.value.contains(&b.label)))
                    || a.value == b.label)
            {
                keep[j] = false;
            }
        }
    }
    // Of two controls that overlap, the later in the tree is drawn on top.
    let covered: Vec<bool> = (0..all.len())
        .map(|i| {
            keep[i]
                && all[i].verb != "-"
                && (i + 1..all.len())
                    .any(|j| keep[j] && all[j].verb != "-" && covers(&all[j], &all[i]))
        })
        .collect();
    let overlapping = covered.iter().any(|c| *c);
    let rows = all
        .into_iter()
        .zip(keep.into_iter().zip(covered))
        .filter_map(|(r, (k, c))| (k && !c).then_some(r))
        .collect();
    (rows, overlapping)
}

/// `under`: an ancestor is a control.
fn walk(
    n: &Value,
    screen: Option<(f64, f64)>,
    under: bool,
    path: &mut Vec<u16>,
    out: &mut Vec<Row>,
) {
    // The on-screen keyboard is forty keys; the screen's title says it is
    // up, and `type` / `go enter` stand in for its keys (as on Android).
    let t = n.get("type").and_then(Value::as_str).unwrap_or("");
    if t.strip_prefix("XCUIElementType").unwrap_or(t) == "Keyboard" {
        return;
    }
    let mut inside = under;
    if let Some(mut r) = row(n, screen) {
        if r.label.is_empty() && r.verb != "-" {
            r.label = first_label(n).unwrap_or_default();
            if r.id == r.label {
                r.id.clear();
            }
        }
        // A switch row spans its label; only its knob toggles it.
        if r.class == "Switch" || r.class == "Toggle" {
            if let Some((kx, ky, kw, kh)) = knob(n, &r.class) {
                r.x = (kx + kw / 2.0).round() as i64;
                r.y = (ky + kh / 2.0).round() as i64;
            }
        }
        let bare = r.label.is_empty() && (r.verb == "-" || under);
        if r.verb != "-" {
            inside = true;
        }
        if !bare {
            r.path = path.clone();
            out.push(r);
        }
    }
    if let Some(kids) = n.get("children").and_then(Value::as_array) {
        for (i, k) in kids.iter().enumerate() {
            path.push(i as u16);
            walk(k, screen, inside, path, out);
            path.pop();
        }
    }
}

/// The on-screen keyboard's rect, when it is up.
pub fn keyboard(n: &Value) -> Option<(f64, f64, f64, f64)> {
    let t = n.get("type").and_then(Value::as_str).unwrap_or("");
    if t.strip_prefix("XCUIElementType").unwrap_or(t) == "Keyboard" {
        return rect(n);
    }
    n.get("children")
        .and_then(Value::as_array)?
        .iter()
        .find_map(keyboard)
}

/// The rect of the first `class` node under `n`: a switch's knob.
fn knob(n: &Value, class: &str) -> Option<(f64, f64, f64, f64)> {
    for k in n
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let t = k.get("type").and_then(Value::as_str).unwrap_or("");
        if t.strip_prefix("XCUIElementType").unwrap_or(t) == class {
            return rect(k);
        }
        if let Some(r) = knob(k, class) {
            return Some(r);
        }
    }
    None
}

/// The first label under `n`, depth first, that reads as words.
fn first_label(n: &Value) -> Option<String> {
    for k in n
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let label = k.get("label").and_then(Value::as_str).unwrap_or("").trim();
        if !label.is_empty() && !symbol_name(label) {
            return Some(label.to_string());
        }
        if let Some(t) = first_label(k) {
            return Some(t);
        }
    }
    None
}

/// An SF Symbol's name ("chevron.forward", "apple.id"), which is what an
/// unlabelled image reports: not something a person would read.
fn symbol_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.')
}

fn rect(n: &Value) -> Option<(f64, f64, f64, f64)> {
    let r = n.get("rect")?;
    let f = |k| r.get(k).and_then(Value::as_f64);
    Some((f("x")?, f("y")?, f("width")?, f("height")?))
}

fn row(n: &Value, screen: Option<(f64, f64)>) -> Option<Row> {
    let s = |k: &str| {
        n.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let class = s("type");
    let class = class
        .strip_prefix("XCUIElementType")
        .unwrap_or(&class)
        .to_string();
    if matches!(class.as_str(), "Application" | "Window") {
        return None;
    }
    let (x, y, w, h) = rect(n)?;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    if let Some((sw, sh)) = screen {
        if cx < 0.0 || cy < 0.0 || cx > sw || cy > sh {
            return None;
        }
    }
    if flag(n, "isVisible") == Some(false) {
        return None;
    }
    let switch = class == "Switch" || class == "Toggle";
    let (mut label, value) = match (s("label"), s("value")) {
        // A switch's value is its state, shown as a flag.
        (l, _) if switch => (l, String::new()),
        (l, v) if l.is_empty() => (v, String::new()),
        (l, v) if v == l => (l, String::new()),
        (l, v) => (l, v),
    };
    if symbol_name(&label) {
        label.clear();
    }
    let name = s("name");
    let id = if name.is_empty() || label.contains(&name) {
        String::new()
    } else {
        name
    };
    let focused = flag(n, "isFocused") == Some(true);
    let verb = if FILLABLE.contains(&class.as_str())
        // A text view is editable or a paragraph; an empty or focused one is a field.
        || (class == "TextView" && (label.is_empty() || focused))
    {
        "fill"
    } else if TAPPABLE.contains(&class.as_str()) {
        "tap"
    } else {
        "-"
    };
    // Scroll indicators: thin, or labelled in the device's language with a
    // percentage for a value ("Vertical scroll bar, 3 pages" · "0%").
    let percent = value
        .strip_suffix('%')
        .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()));
    if verb == "-" && (w.min(h) < 8.0 || (class == "Other" && percent)) {
        return None;
    }
    // Bars are named for what they are ("Toolbar", in the device's
    // language); what they hold has its own rows.
    if verb == "-" && matches!(class.as_str(), "Toolbar" | "TabBar" | "NavigationBar") {
        return None;
    }
    let mut flags = Vec::new();
    if flag(n, "isEnabled") == Some(false) {
        flags.push("disabled");
    }
    if class == "SecureTextField" {
        flags.push("password");
    }
    if switch {
        flags.push(if s("value") == "1" { "on" } else { "off" });
    }
    if focused && verb == "fill" {
        flags.push("focused");
    }
    Some(Row {
        verb,
        class,
        label,
        value,
        id,
        x: cx.round() as i64,
        y: cy.round() as i64,
        bounds: (
            x.round() as i64,
            y.round() as i64,
            (x + w).round() as i64,
            (y + h).round() as i64,
        ),
        flags,
        path: Vec::new(),
    })
}

/// A boolean attribute; WDA's JSON source writes them as "1" and "0".
fn flag(n: &Value, k: &str) -> Option<bool> {
    match n.get(k)? {
        Value::Bool(b) => Some(*b),
        Value::Number(x) => Some(x.as_i64() != Some(0)),
        Value::String(s) => Some(s != "0" && s != "false"),
        _ => None,
    }
}

/// The table for `rows`.
pub fn table(rows: &[Row]) -> String {
    rows.iter().map(|r| r.line() + "\n").collect()
}

/// `rows` as mobax-core rows, for `@N` refs and change lists.
pub fn ui_rows(rows: &[Row]) -> Vec<mobax_core::ui_dump::UiRow> {
    rows.iter()
        .map(|r| mobax_core::ui_dump::UiRow {
            line: r.line(),
            ident: r.ident(),
            x: r.x,
            y: r.y,
            bounds: Some(r.bounds),
            sel: if r.id.is_empty() {
                r.label.clone()
            } else {
                format!("#{}", r.id)
            },
        })
        .collect()
}

/// The row `want` names: an exact label or id match first, then a prefix,
/// preferring one that can be tapped.
pub fn pick<'a>(rows: &'a [Row], want: &str) -> Option<&'a Row> {
    for exact in [true, false] {
        let hits: Vec<&Row> = rows.iter().filter(|r| r.named(want, exact)).collect();
        if let Some(r) = hits.iter().find(|r| r.verb != "-").or(hits.first()) {
            return Some(r);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn el(ty: &str, label: &str, name: &str, x: f64, y: f64, w: f64, h: f64) -> Value {
        json!({"type": format!("XCUIElementType{ty}"), "label": label, "name": name,
               "value": null, "isEnabled": "1", "isVisible": "1",
               "rect": {"x": x, "y": y, "width": w, "height": h}, "children": []})
    }

    fn tree() -> Value {
        let mut cell = el("Cell", "", "", 0.0, 200.0, 414.0, 60.0);
        cell["children"] = json!([el("StaticText", "1 채널", "", 16.0, 210.0, 100.0, 40.0)]);
        let mut field = el("SecureTextField", "", "password", 20.0, 400.0, 300.0, 40.0);
        field["value"] = json!("••••");
        let mut hidden = el("Button", "가려짐", "", 0.0, 500.0, 10.0, 10.0);
        hidden["isVisible"] = json!("0");
        json!({"type": "XCUIElementTypeApplication", "label": "Mafia42", "name": "",
        "rect": {"x": 0, "y": 0, "width": 414, "height": 896},
        "children": [
            el("Button", "로그아웃", "logout", 300.0, 40.0, 100.0, 44.0),
            el("Other", "", "", 0.0, 100.0, 414.0, 50.0),
            cell,
            field,
            el("Button", "숨김", "", 0.0, 2000.0, 10.0, 10.0),
            hidden,
        ]})
    }

    #[test]
    fn flattens_to_readable_and_actionable_rows() {
        let rows = rows(&tree());
        let lines: Vec<String> = rows.iter().map(Row::line).collect();
        assert_eq!(
            lines,
            vec![
                "tap  Button  \"로그아웃\"  #logout  350,62",
                "tap  Cell  \"1 채널\"  207,230",
                "fill  SecureTextField  \"••••\"  #password  170,420  [password]",
            ]
        );
    }

    #[test]
    fn picks_exact_then_prefix_then_by_id() {
        let rows = rows(&tree());
        assert_eq!(pick(&rows, "로그아웃").map(|r| r.x), Some(350));
        assert_eq!(pick(&rows, "1 채").map(|r| (r.x, r.y)), Some((207, 230)));
        assert_eq!(
            pick(&rows, "#password").map(|r| r.class.as_str()),
            Some("SecureTextField")
        );
        assert!(pick(&rows, "없음").is_none());
    }

    #[test]
    fn a_fields_text_follows_its_label() {
        let mut f = el("SearchField", "검색", "", 20.0, 500.0, 300.0, 40.0);
        f["value"] = json!("Wi-Fi");
        let mut sw = el("Switch", "관련 콘텐츠 보기", "", 300.0, 160.0, 60.0, 40.0);
        sw["value"] = json!("1");
        let t = json!({"type": "XCUIElementTypeApplication",
                       "rect": {"x": 0, "y": 0, "width": 402, "height": 874}, "children": [f, sw]});
        let lines: Vec<String> = rows(&t).iter().map(Row::line).collect();
        assert_eq!(
            lines,
            vec![
                "fill  SearchField  \"검색\" · \"Wi-Fi\"  170,520",
                "tap  Switch  \"관련 콘텐츠 보기\"  330,180  [on]",
            ]
        );
    }

    /// A settings row as iOS 27's Settings app exposes it: a cell holding a
    /// button and a switch with the same label, and the label's text.
    #[test]
    fn one_thing_on_screen_is_one_row() {
        let mut sw = el("Switch", "관련 콘텐츠 보기", "", 330.0, 160.0, 60.0, 40.0);
        sw["value"] = json!("1");
        let mut cell = el("Cell", "", "관련 콘텐츠 보기", 0.0, 155.0, 402.0, 52.0);
        cell["children"] = json!([
            el("Button", "관련 콘텐츠 보기", "", 16.0, 160.0, 150.0, 40.0),
            el(
                "StaticText",
                "관련 콘텐츠 보기",
                "",
                16.0,
                160.0,
                150.0,
                40.0
            ),
            sw,
        ]);
        let t = json!({"type": "XCUIElementTypeApplication",
                       "rect": {"x": 0, "y": 0, "width": 402, "height": 874},
                       "children": [el("Other", "", "Home screen icons", 0.0, 0.0, 402.0, 874.0), cell]});
        let lines: Vec<String> = rows(&t).iter().map(Row::line).collect();
        assert_eq!(
            lines,
            vec!["tap  Switch  \"관련 콘텐츠 보기\"  360,180  [on]"]
        );
    }

    /// Reusable table cells parked off screen keep one stale frame; without
    /// `isVisible` they are told apart by piling up there.
    #[test]
    fn a_stack_of_parked_cells_is_hidden() {
        let parked: Vec<Value> = ["메시지", "사진", "설정"]
            .iter()
            .map(|t| el("StaticText", t, "", 100.0, 130.0, 66.0, 26.0))
            .collect();
        let mut kids = parked;
        kids.push(el("Button", "검색", "", 20.0, 300.0, 100.0, 44.0));
        let t = json!({"type": "Application", "rect": {"x": 0, "y": 0, "width": 402, "height": 874},
                       "children": kids});
        let lines: Vec<String> = rows(&t).iter().map(Row::line).collect();
        assert_eq!(lines, vec!["tap  Button  \"검색\"  70,322"]);
    }

    /// A switch row spans its label; the tap point is its knob.
    #[test]
    fn a_switch_is_tapped_on_its_knob_and_scroll_bars_go() {
        let mut sw = el(
            "Switch",
            "관련 콘텐츠 보기",
            "관련 콘텐츠 보기",
            36.0,
            166.0,
            330.0,
            29.0,
        );
        sw["value"] = json!("1");
        let mut knob = el("Switch", "", "", 305.0, 166.0, 63.0, 29.0);
        knob["value"] = json!("1");
        sw["children"] = json!([
            el("Button", "관련 콘텐츠 보기", "", 36.0, 170.0, 112.0, 21.0),
            knob
        ]);
        let mut bar = el(
            "Other",
            "수직 스크롤 막대, 3페이지",
            "",
            369.0,
            116.0,
            30.0,
            696.0,
        );
        bar["value"] = json!("0%");
        let t = json!({"type": "Application", "rect": {"x": 0, "y": 0, "width": 402, "height": 874},
                       "children": [sw, bar]});
        let lines: Vec<String> = rows(&t).iter().map(Row::line).collect();
        assert_eq!(
            lines,
            vec!["tap  Switch  \"관련 콘텐츠 보기\"  337,181  [on]"]
        );
    }

    /// The keyboard is the screen's title ("keyboard open"), not forty rows.
    #[test]
    fn the_keyboard_is_not_rows() {
        let mut kb = el("Keyboard", "", "", 0.0, 600.0, 402.0, 220.0);
        kb["children"] = json!([el("Key", "q", "q", 0.0, 610.0, 40.0, 40.0)]);
        let t = json!({"type": "Application", "rect": {"x": 0, "y": 0, "width": 402, "height": 874},
                       "children": [el("SearchField", "검색", "", 20.0, 520.0, 300.0, 40.0), kb,
                                    el("Button", "받아쓰기", "dictation", 340.0, 820.0, 40.0, 40.0)]});
        let lines: Vec<String> = rows(&t).iter().map(Row::line).collect();
        assert_eq!(lines, vec!["fill  SearchField  \"검색\"  170,540"]);
    }

    /// Of overlapping controls the later (drawn on top) is kept, and the
    /// screen is flagged for a read with `isVisible`.
    #[test]
    fn a_control_under_another_is_left_out_and_flagged() {
        let t = |els: Vec<Value>| {
            let (rows, overlapping) = build(&json!({"type": "Application",
                "rect": {"x": 0, "y": 0, "width": 402, "height": 874}, "children": els}));
            (
                rows.iter().map(|r| r.label.clone()).collect::<Vec<_>>(),
                overlapping,
            )
        };
        // Suggestions wider than the list cells they cover.
        assert_eq!(
            t(vec![
                el("Cell", "일반", "", 16.0, 125.0, 370.0, 53.0),
                el("Cell", "제안", "", 0.0, 124.0, 402.0, 59.0),
            ]),
            (vec!["제안".to_string()], true)
        );
        // Rows that touch, and a button inside a cell, are all there.
        let mut cell = el("Cell", "일반", "", 0.0, 200.0, 402.0, 50.0);
        cell["children"] = json!([el("Button", "정보", "", 300.0, 210.0, 40.0, 30.0)]);
        assert_eq!(
            t(vec![cell, el("Cell", "검색", "", 0.0, 250.0, 402.0, 50.0)]),
            (
                vec!["일반".to_string(), "정보".to_string(), "검색".to_string()],
                false
            )
        );
    }

    #[test]
    fn reads_content_length() {
        let r = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\ncontent-length: 2\r\n\r\n{}";
        assert_eq!(head_and_length(r), Some((r.len() - 2, 2)));
        assert_eq!(head_and_length(b"HTTP/1.1 200 OK\r\n"), None);
    }
}
