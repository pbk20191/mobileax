//! mobax-ios — the iPhone side of mobileAX, experimental.
//!
//! Real iPhones and iOS Simulators answer to the same verbs. WebDriverAgent
//! gives the whole screen with positions, taps and typing: on an iPhone it
//! needs a signed runner (scripts/ios-wda-sign.sh) and the Developer Disk
//! Image, on a simulator Appium's prebuilt simulator build. The accessibility
//! inspector (iPhone only) needs nothing installed and no signing, but reads
//! one element at a time and has no positions.

mod archive;
mod ax;
mod ddi;
mod device;
mod dtx;
mod mcp;
mod sim;
mod wda;
mod xml;

use std::{path::Path, process::ExitCode, sync::Arc, time::Instant};

use idevice::{
    installation_proxy::InstallationProxyClient, provider::IdeviceProvider,
    screenshotr::ScreenshotService, syslog_relay::SyslogRelayClient, wda::WdaClient,
    IdeviceService,
};

const USAGE: &str = "\
mobax-ios — iPhone over usbmuxd (experimental)

Usage: mobax-ios [--udid UDID | --sim[=UDID|NAME]] <verb> [args]
       (or MOBAX_IOS_UDID / MOBAX_IOS_SIM; with neither, the attached
       iPhone, else the booted simulator)

Device
  list                       attached iPhones and booted simulators
  info                       model, iOS version, name
  apps                       user-installed apps
  install PATH               an .app folder or .ipa
  ddi                        Developer Mode and Developer Disk Image state (iPhone)
  ddi mount [DIR]            mount the image (default: Xcode's, on a Mac)
  see PATH.png               screenshot (iPhone: needs the image, or WDA running)
  logs [--grep TEXT]         follow the syslog

WebDriverAgent (start it once; the other verbs talk to it while it runs)
  wda [--runner BUNDLE_ID]   iPhone: start the runner and keep it running (Ctrl-C stops)
  wda [--app PATH.app]       simulator: (install and) start WDA; it stays up
  ui [--json]                the screen as a table: verb, type, label, #id, x,y
  tap X Y | tap TEXT|#id     tap a point, or the element so labelled
  type TEXT                  type into the focused field
  swipe X1 Y1 X2 Y2 [MS] [--fling]  drag over MS ms (default 250); stops dead unless --fling
  go home|volumeup|volumedown
  launch BUNDLE_ID | kill BUNDLE_ID   (simulator: no WDA needed)

Agents
  mcp                        MCP server on stdio: @N rows, replies with what changed

Accessibility inspector (iPhone; no signing, no positions)
  ax [--max N] [--attrs]     the screen, one element at a time
  ax activate TEXT           activate the first element whose spoken text starts with TEXT

Coordinates are points, as WDA takes them.
";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let udid = take_opt(&mut args, "--udid").or_else(|| std::env::var("MOBAX_IOS_UDID").ok());
    let sim = take_sim(&mut args);
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match rt.block_on(run(udid.as_deref(), sim, args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(
    udid: Option<&str>,
    sim: Option<Option<String>>,
    mut args: Vec<String>,
) -> Result<(), String> {
    let verb = args.remove(0);
    if verb == "list" {
        match device::list().await {
            Ok(devs) => devs
                .iter()
                .for_each(|d| println!("{}\tiphone\t{:?}", d.udid, d.connection_type)),
            Err(e) => eprintln!("usbmuxd: {e}"),
        }
        for (u, name, runtime) in sim::booted().unwrap_or_default() {
            println!("{u}\tsimulator\t{name} ({runtime})");
        }
        return Ok(());
    }
    // An explicit choice, else the attached iPhone, else the booted simulator.
    let sim = match (sim, udid) {
        (Some(want), _) => Some(sim::pick(want.as_deref())?),
        (None, Some(_)) => None,
        (None, None) => match device::provider(None).await {
            Ok(_) => None,
            Err(e) => Some(sim::pick(None).map_err(|se| format!("{e}; {se}"))?),
        },
    };
    if let Some(u) = &sim {
        if let Some(done) = run_sim(u, &verb, &mut args).await? {
            return Ok(done);
        }
    }
    let provider: Arc<dyn IdeviceProvider> = match &sim {
        Some(u) => Arc::new(sim::SimProvider { udid: u.clone() }),
        None => Arc::new(device::provider(udid).await?),
    };
    let p = &*provider;
    match verb.as_str() {
        "mcp" => mcp::serve(provider.clone(), sim.clone()).await?,
        "info" => {
            let v = device::info(p).await.map_err(e("lockdown"))?;
            let d = v.as_dictionary().ok_or("lockdown: no values")?;
            for k in [
                "DeviceName",
                "ProductType",
                "ProductVersion",
                "BuildVersion",
                "UniqueDeviceID",
            ] {
                let s = d.get(k).and_then(|v| v.as_string()).unwrap_or("?");
                println!("{k:<15} {s}");
            }
        }
        "apps" => {
            let mut inst = InstallationProxyClient::connect(p)
                .await
                .map_err(e("installation_proxy"))?;
            let apps = inst
                .get_apps(Some("User"), None)
                .await
                .map_err(e("installation_proxy"))?;
            let mut rows: Vec<(String, String)> = apps
                .iter()
                .map(|(id, info)| {
                    let name = info
                        .as_dictionary()
                        .and_then(|d| d.get("CFBundleDisplayName").or(d.get("CFBundleName")))
                        .and_then(|v| v.as_string())
                        .unwrap_or("")
                        .to_string();
                    (id.clone(), name)
                })
                .collect();
            rows.sort();
            for (id, name) in rows {
                println!("{id}\t{name}");
            }
        }
        "install" => {
            let path = args.first().ok_or("install PATH")?;
            idevice::utils::installation::install_package(p, path, None)
                .await
                .map_err(e("install"))?;
            println!("installed {path}");
        }
        "ddi" => match args.first().map(String::as_str) {
            None => {
                let (dev_mode, mounted) = ddi::status(p).await?;
                let dev_mode = match dev_mode {
                    Some(true) => "on",
                    Some(false) => "off",
                    None => "?",
                };
                println!("developer mode  {dev_mode}");
                println!(
                    "disk image      {}",
                    if mounted { "mounted" } else { "not mounted" }
                );
            }
            Some("mount") => println!("{}", ddi::mount(p, args.get(1).map(Path::new)).await?),
            Some(other) => {
                return Err(format!(
                    "ddi {other:?}: expected `ddi` or `ddi mount [DIR]`"
                ))
            }
        },
        "see" => {
            let path = args.first().ok_or("see PATH.png")?;
            let png = match ScreenshotService::connect(p).await {
                Ok(mut shot) => shot.take_screenshot().await.map_err(e("screenshot"))?,
                Err(err) => WdaClient::new(p)
                    .screenshot(None)
                    .await
                    .map_err(|wda_err| {
                        format!("screenshotr: {err} (try `mobax-ios ddi mount`); WDA: {wda_err}")
                    })?,
            };
            std::fs::write(path, &png).map_err(|err| format!("{path}: {err}"))?;
            println!("saved {} bytes to {path}", png.len());
        }
        "logs" => {
            let grep = take_opt(&mut args, "--grep");
            let mut log = SyslogRelayClient::connect(p)
                .await
                .map_err(e("syslog_relay"))?;
            loop {
                let line = log.next().await.map_err(e("syslog"))?;
                if grep.as_deref().is_none_or(|g| line.contains(g)) {
                    println!("{line}");
                }
            }
        }
        "wda" => {
            let runner = wda::runner_id(p, take_opt(&mut args, "--runner").as_deref()).await?;
            let t0 = Instant::now();
            let handle = wda::start(provider.clone(), &runner).await?;
            let build = handle
                .status()
                .pointer("/value/build/version")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            eprintln!(
                "WDA {build} up on device port {} in {:.1}s ({runner}); Ctrl-C stops it",
                handle.ports().http,
                t0.elapsed().as_secs_f64()
            );
            tokio::select! {
                r = handle.wait() => r.map_err(e("runner"))?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        "ui" => {
            let (tree, rows) = wda::screen(p).await?;
            if args.iter().any(|a| a == "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&tree).unwrap_or_default()
                );
            } else {
                print!("{}", wda::table(&rows));
            }
        }
        "tap" => {
            let (x, y) = match (args.first(), args.get(1)) {
                (Some(x), Some(y)) if x.parse::<f64>().is_ok() && y.parse::<f64>().is_ok() => {
                    (x.parse().unwrap(), y.parse().unwrap())
                }
                (Some(_), _) => {
                    let want = args.join(" ");
                    let (_, rows) = wda::screen(p).await?;
                    let r = wda::pick(&rows, &want)
                        .ok_or_else(|| format!("nothing labelled {want:?} on screen"))?;
                    (r.x as f64, r.y as f64)
                }
                _ => return Err("tap X Y | tap TEXT".into()),
            };
            let (_, s) = wda::session(p).await?;
            wda::tap(p, &s, x, y).await?;
            println!("tapped {x},{y}");
        }
        "type" => {
            let text = args.join(" ");
            let (wda, s) = wda::session(p).await?;
            wda.send_keys(&text, Some(&s)).await.map_err(e("type"))?;
        }
        "swipe" => {
            let n: Vec<i64> = args.iter().filter_map(|a| a.parse().ok()).collect();
            let [x1, y1, x2, y2, rest @ ..] = n.as_slice() else {
                return Err("swipe X1 Y1 X2 Y2 [MS]".into());
            };
            let ms = rest.first().copied().unwrap_or(250);
            let (_, s) = wda::session(p).await?;
            let fling = args.iter().any(|a| a == "--fling");
            wda::swipe(p, &s, (*x1, *y1), (*x2, *y2), ms, fling).await?;
        }
        "go" => {
            let button = args.first().ok_or("go home|volumeup|volumedown")?;
            let (wda, s) = wda::session(p).await?;
            wda.press_button(button, Some(&s)).await.map_err(e("go"))?;
        }
        "launch" | "kill" => {
            let id = args.first().ok_or_else(|| format!("{verb} BUNDLE_ID"))?;
            let (_, s) = wda::session(p).await?;
            let action = if verb == "launch" {
                "launch"
            } else {
                "terminate"
            };
            wda::app(p, &s, action, id).await?;
        }
        "ax" => {
            let mut ax = ax::Ax::connect(p).await?;
            let err = |e: dtx::Error| format!("accessibility: {e}");
            ax.start().await.map_err(err)?;
            if args.first().map(String::as_str) == Some("activate") {
                let want = args.get(1).ok_or("ax activate TEXT")?;
                let found = ax
                    .elements(500)
                    .await
                    .map_err(err)?
                    .into_iter()
                    .find(|el| el.text().starts_with(want.as_str()));
                let el = found.ok_or_else(|| format!("no element starts with {want:?}"))?;
                ax.activate(&el).await.map_err(err)?;
                println!("activated {:?}", el.text());
            } else {
                let max = take_opt(&mut args, "--max")
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(200);
                let attrs = args.iter().any(|a| a == "--attrs");
                let t0 = Instant::now();
                let els = ax.elements(max).await.map_err(err)?;
                for (i, el) in els.iter().enumerate() {
                    let id = ax
                        .attribute(el, "Identifier")
                        .await
                        .ok()
                        .flatten()
                        .filter(|s| !s.is_empty());
                    let id = id.map(|s| format!("  #{s}")).unwrap_or_default();
                    let acts: Vec<&str> = el.actions().collect();
                    let acts = if acts.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", acts.join(","))
                    };
                    println!("{i:>3}  {:?}{id}{acts}", el.text());
                    if attrs {
                        println!("       {}", el.attributes.join(" "));
                    }
                }
                eprintln!("{} elements in {} ms", els.len(), t0.elapsed().as_millis());
            }
            ax.turn_off().await.map_err(err)?;
        }
        other => return Err(format!("unknown verb {other:?}\n\n{USAGE}")),
    }
    Ok(())
}

/// The verbs a simulator does through simctl; None for those it shares with
/// an iPhone (the WDA ones).
async fn run_sim(udid: &str, verb: &str, args: &mut Vec<String>) -> Result<Option<()>, String> {
    match verb {
        "info" => {
            let (_, name, runtime) = sim::booted()?
                .into_iter()
                .find(|(u, ..)| u == udid)
                .ok_or("simulator shut down")?;
            println!("DeviceName      {name}\nProductVersion  {runtime}\nUniqueDeviceID  {udid}\nSimulator       yes");
        }
        "apps" => {
            let listing = sim::simctl(&["listapps", udid])?;
            // simctl prints an old-style plist; plutil reads it.
            let mut plutil = std::process::Command::new("plutil")
                .args(["-convert", "json", "-o", "-", "-"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("plutil: {e}"))?;
            use std::io::Write;
            plutil
                .stdin
                .take()
                .unwrap()
                .write_all(listing.as_bytes())
                .map_err(|e| format!("plutil: {e}"))?;
            let out = plutil
                .wait_with_output()
                .map_err(|e| format!("plutil: {e}"))?;
            let v: serde_json::Value =
                serde_json::from_slice(&out.stdout).map_err(|e| format!("simctl listapps: {e}"))?;
            let mut rows: Vec<(String, String)> = v
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, a)| a["ApplicationType"] == "User")
                .map(|(id, a)| {
                    let name = a["CFBundleDisplayName"]
                        .as_str()
                        .or(a["CFBundleName"].as_str())
                        .unwrap_or("");
                    (id.clone(), name.to_string())
                })
                .collect();
            rows.sort();
            for (id, name) in rows {
                println!("{id}\t{name}");
            }
        }
        "install" => {
            let path = args.first().ok_or("install PATH")?;
            sim::simctl(&["install", udid, path])?;
            println!("installed {path}");
        }
        "see" => {
            let path = args.first().ok_or("see PATH.png")?;
            sim::simctl(&["io", udid, "screenshot", "--type=png", path])?;
            println!("saved {path}");
        }
        "logs" => {
            let grep = take_opt(args, "--grep");
            let mut child = std::process::Command::new("xcrun")
                .args([
                    "simctl", "spawn", udid, "log", "stream", "--style", "compact",
                ])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("log stream: {e}"))?;
            use std::io::BufRead;
            for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
                let line = line.map_err(|e| format!("log stream: {e}"))?;
                if grep.as_deref().is_none_or(|g| line.contains(g)) {
                    println!("{line}");
                }
            }
        }
        "wda" => {
            let app = take_opt(args, "--app");
            eprintln!("{}", sim::start_wda(udid, app.as_deref()).await?);
        }
        "launch" => {
            let id = args.first().ok_or("launch BUNDLE_ID")?;
            sim::simctl(&["launch", udid, id])?;
        }
        "kill" => {
            let id = args.first().ok_or("kill BUNDLE_ID")?;
            sim::simctl(&["terminate", udid, id])?;
        }
        "ax" | "ddi" => {
            return Err(format!(
                "{verb} is for a real iPhone; a simulator has WDA (`ui`)"
            ))
        }
        _ => return Ok(None),
    }
    Ok(Some(()))
}

/// Removes `--sim` or `--sim=X` from `args`; MOBAX_IOS_SIM stands in for it.
fn take_sim(args: &mut Vec<String>) -> Option<Option<String>> {
    if let Some(i) = args
        .iter()
        .position(|a| a == "--sim" || a.starts_with("--sim="))
    {
        let a = args.remove(i);
        return Some(a.strip_prefix("--sim=").map(str::to_string));
    }
    std::env::var("MOBAX_IOS_SIM")
        .ok()
        .map(|v| (!v.is_empty() && v != "1").then_some(v))
}

/// Formats an idevice error under `what`, for `map_err`.
fn e(what: &'static str) -> impl Fn(idevice::IdeviceError) -> String {
    move |err| format!("{what}: {err}")
}

/// Removes `--name VALUE` from `args` and returns VALUE.
fn take_opt(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.remove(i);
    (i < args.len()).then(|| args.remove(i))
}
