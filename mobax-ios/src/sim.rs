//! iOS Simulator, on a Mac: `xcrun simctl` for the device, WDA for the screen.
//!
//! A simulator shares the Mac's network, so WDA's port is on localhost and
//! [SimProvider] hands WdaClient a TCP socket instead of a usbmuxd one. The
//! simulator build of WDA (Appium's WebDriverAgentRunner-Build-Sim-*.zip)
//! embeds XCTest, so `simctl launch` starts it: no xcodebuild, no signing, and
//! it keeps running after we exit.

use std::{future::Future, pin::Pin, process::Command, time::Duration};

use idevice::{
    pairing_file::PairingFile, provider::IdeviceProvider, wda::WdaClient, Idevice, IdeviceError,
};

pub const WDA_BUNDLE: &str = "com.facebook.WebDriverAgentRunner.xctrunner";

/// Reaches ports on localhost, where a simulator's servers listen.
#[derive(Debug)]
pub struct SimProvider {
    pub udid: String,
}

impl IdeviceProvider for SimProvider {
    fn connect(
        &self,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Idevice, IdeviceError>> + Send>> {
        let label = format!("sim {}", self.udid);
        Box::pin(async move {
            let s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
            Ok(Idevice::new(Box::new(s), label))
        })
    }

    fn label(&self) -> &str {
        "mobax-ios-sim"
    }

    fn get_pairing_file(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<PairingFile, IdeviceError>> + Send>> {
        // Lockdown services (the accessibility inspector, the disk image) are
        // device-only.
        Box::pin(async { Err(IdeviceError::NotFound) })
    }
}

/// Runs `xcrun simctl ARGS` and returns its stdout.
pub fn simctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("xcrun")
        .arg("simctl")
        .args(args)
        .output()
        .map_err(|e| format!("xcrun simctl: {e} (simulators need Xcode on a Mac)"))?;
    if !out.status.success() {
        return Err(format!(
            "simctl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Booted simulators as (udid, name, runtime).
pub fn booted() -> Result<Vec<(String, String, String)>, String> {
    let json = simctl(&["list", "devices", "booted", "-j"])?;
    let v: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("simctl list: {e}"))?;
    let mut out = Vec::new();
    for (runtime, devs) in v["devices"].as_object().into_iter().flatten() {
        let runtime = runtime.rsplit('.').next().unwrap_or(runtime).to_string();
        for d in devs.as_array().into_iter().flatten() {
            if d["state"] == "Booted" {
                out.push((
                    d["udid"].as_str().unwrap_or("").to_string(),
                    d["name"].as_str().unwrap_or("").to_string(),
                    runtime.clone(),
                ));
            }
        }
    }
    Ok(out)
}

/// The booted simulator `want` names (udid or name), or the only one booted.
pub fn pick(want: Option<&str>) -> Result<String, String> {
    let all = booted()?;
    match want {
        Some(w) => all
            .into_iter()
            .find(|(u, n, _)| u == w || n == w)
            .map(|(u, ..)| u)
            .ok_or_else(|| format!("no booted simulator {w:?} (`xcrun simctl boot` it)")),
        None => match all.len() {
            0 => Err("no booted simulator (`xcrun simctl boot NAME`)".into()),
            1 => Ok(all.into_iter().next().unwrap().0),
            _ => Err("several simulators booted; pass --sim UDID".into()),
        },
    }
}

/// Installs `app` if given, then starts WDA unless it already answers.
pub async fn start_wda(udid: &str, app: Option<&str>) -> Result<String, String> {
    let provider = SimProvider { udid: udid.into() };
    let wda = WdaClient::new(&provider);
    if let Some(app) = app {
        simctl(&["install", udid, app])?;
    } else if let Ok(s) = wda.status().await {
        return Ok(format!("WDA {} already up", version(&s)));
    }
    if simctl(&["get_app_container", udid, WDA_BUNDLE]).is_err() {
        return Err(format!(
            "{WDA_BUNDLE} is not installed; fetch Appium's simulator build with \
             scripts/ios-wda-sim.sh and pass `wda --app PATH.app`"
        ));
    }
    simctl(&["launch", "--terminate-running-process", udid, WDA_BUNDLE])?;
    let s = wda
        .wait_until_ready(Duration::from_secs(60))
        .await
        .map_err(|e| format!("WDA did not come up on localhost:8100: {e}"))?;
    Ok(format!("WDA {} up", version(&s)))
}

fn version(status: &serde_json::Value) -> &str {
    status
        .pointer("/value/build/version")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
}
