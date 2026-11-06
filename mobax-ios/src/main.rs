//! mobax-ios — the iPhone side of mobileAX, experimental.
//!
//! What works without signing anything: screenshots, the syslog, and reading
//! the screen through the accessibility inspector. Taps on iOS 18 need
//! WebDriverAgent (see docs/ios.md).

mod archive;
mod ax;
mod device;

use std::{process::ExitCode, time::Instant};

use idevice::{screenshotr::ScreenshotService, syslog_relay::SyslogRelayClient, IdeviceService};

const USAGE: &str = "\
mobax-ios — iPhone over usbmuxd (experimental)

Usage: mobax-ios [--udid UDID] <verb> [args]

  list                     attached iPhones
  info                     model, iOS version, name
  see PATH.png             screenshot (needs the Developer Disk Image mounted)
  logs [--grep TEXT]       follow the syslog
  ax [--max N] [--attrs]   the screen as the accessibility inspector reads it
  ax activate TEXT         activate the first element whose spoken text starts with TEXT
";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let udid = take_opt(&mut args, "--udid").or_else(|| std::env::var("MOBAX_IOS_UDID").ok());
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match rt.block_on(run(udid.as_deref(), args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(udid: Option<&str>, mut args: Vec<String>) -> Result<(), String> {
    let verb = args.remove(0);
    if verb == "list" {
        for d in device::list().await.map_err(|e| format!("usbmuxd: {e}"))? {
            println!("{}\t{:?}", d.udid, d.connection_type);
        }
        return Ok(());
    }
    let provider = device::provider(udid).await?;
    match verb.as_str() {
        "info" => {
            let v = device::info(&provider).await.map_err(e("lockdown"))?;
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
        "see" => {
            let path = args.first().ok_or("see PATH.png")?;
            let mut shot = ScreenshotService::connect(&provider)
                .await
                .map_err(e("screenshotr"))?;
            let png = shot.take_screenshot().await.map_err(e("screenshot"))?;
            std::fs::write(path, &png).map_err(|err| format!("{path}: {err}"))?;
            println!("saved {} bytes to {path}", png.len());
        }
        "logs" => {
            let grep = take_opt(&mut args, "--grep");
            let mut log = SyslogRelayClient::connect(&provider)
                .await
                .map_err(e("syslog_relay"))?;
            loop {
                let line = log.next().await.map_err(e("syslog"))?;
                if grep.as_deref().is_none_or(|g| line.contains(g)) {
                    println!("{line}");
                }
            }
        }
        "ax" => {
            let mut ax = ax::Ax::connect(&provider)
                .await
                .map_err(e("accessibility"))?;
            ax.start().await.map_err(e("accessibility"))?;
            if args.first().map(String::as_str) == Some("activate") {
                let want = args.get(1).ok_or("ax activate TEXT")?;
                let found = ax
                    .elements(500)
                    .await
                    .map_err(e("accessibility"))?
                    .into_iter()
                    .find(|el| el.text().starts_with(want.as_str()));
                let el = found.ok_or_else(|| format!("no element starts with {want:?}"))?;
                ax.activate(&el).await.map_err(e("activate"))?;
                println!("activated {:?}", el.text());
            } else {
                let max = take_opt(&mut args, "--max")
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(200);
                let attrs = args.iter().any(|a| a == "--attrs");
                let t0 = Instant::now();
                let els = ax.elements(max).await.map_err(e("accessibility"))?;
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
            ax.turn_off().await.map_err(e("accessibility"))?;
        }
        other => return Err(format!("unknown verb {other:?}\n\n{USAGE}")),
    }
    Ok(())
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
