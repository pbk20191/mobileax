//! Finding the iPhone through usbmuxd and opening lockdown services on it.
//!
//! usbmuxd is the daemon on macOS and Linux, and Apple Mobile Device Service
//! (TCP 27015) on Windows: a service with iTunes, a startup task of the
//! Microsoft Store Apple Devices app. `USBMUXD_SOCKET_ADDRESS` overrides
//! the address. Everything here goes through
//! lockdown, so no iOS 17 tunnel (and no admin rights) is needed.

use idevice::{
    lockdown::LockdownClient,
    provider::{IdeviceProvider, UsbmuxdProvider},
    usbmuxd::{UsbmuxdAddr, UsbmuxdConnection, UsbmuxdDevice},
    IdeviceError, IdeviceService, ReadWrite,
};

const LABEL: &str = "mobax-ios";

pub async fn list() -> Result<Vec<UsbmuxdDevice>, String> {
    mux()
        .await?
        .get_devices()
        .await
        .map_err(|e| format!("usbmuxd: {e}"))
}

/// A usbmuxd connection at `USBMUXD_SOCKET_ADDRESS`, else the platform's
/// default (`UsbmuxdConnection::default()` ignores the variable).
async fn mux() -> Result<UsbmuxdConnection, String> {
    let addr = UsbmuxdAddr::from_env_var().map_err(|e| format!("USBMUXD_SOCKET_ADDRESS: {e}"))?;
    addr.connect(0).await.map_err(|e| {
        let hint = if cfg!(windows) {
            " (is Apple Mobile Device Service running? It comes with the Apple Devices app or iTunes)"
        } else {
            ""
        };
        format!("usbmuxd: {e}{hint}")
    })
}

/// The device with `udid`, or the only one attached.
pub async fn provider(udid: Option<&str>) -> Result<UsbmuxdProvider, String> {
    let mut mux = mux().await?;
    let dev = match udid {
        Some(u) => mux.get_device(u).await.map_err(|e| format!("{u}: {e}"))?,
        None => {
            let mut all = mux
                .get_devices()
                .await
                .map_err(|e| format!("usbmuxd: {e}"))?;
            match all.len() {
                0 => return Err("no iPhone attached (is it unlocked and trusted?)".into()),
                1 => all.remove(0),
                _ => return Err("more than one iPhone attached; pass --udid".into()),
            }
        }
    };
    let addr = UsbmuxdAddr::from_env_var().map_err(|e| format!("USBMUXD_SOCKET_ADDRESS: {e}"))?;
    Ok(dev.to_provider(addr, LABEL))
}

/// Starts the lockdown service `name` and returns its socket, with TLS on it
/// when lockdown asks for that.
pub async fn start_service(
    provider: &dyn IdeviceProvider,
    name: &str,
) -> Result<Box<dyn ReadWrite>, IdeviceError> {
    let pairing = provider.get_pairing_file().await?;
    let mut lockdown = LockdownClient::connect(provider).await?;
    let legacy = lockdown.start_session(&pairing).await?;
    let (port, ssl) = lockdown.start_service(name).await?;
    let mut service = provider.connect(port).await?;
    if ssl {
        service.start_session(&pairing, legacy).await?;
    }
    service
        .get_socket()
        .ok_or(IdeviceError::NoEstablishedConnection)
}

/// Lockdown values for `info`.
pub async fn info(provider: &dyn IdeviceProvider) -> Result<plist::Value, IdeviceError> {
    let pairing = provider.get_pairing_file().await?;
    let mut lockdown = LockdownClient::connect(provider).await?;
    lockdown.start_session(&pairing).await?;
    lockdown.get_value(None, None).await
}
