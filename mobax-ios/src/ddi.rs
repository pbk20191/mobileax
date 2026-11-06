//! The Developer Disk Image: screenshotr and testmanagerd (so WDA) need it.
//!
//! iOS 17 and later take one "personalized" image, signed for this device by
//! Apple's TSS server at mount time; Xcode keeps it in
//! `/Library/Developer/DeveloperDiskImages/iOS_DDI/Restore`. Elsewhere, copy
//! that folder over, or use the `PersonalizedImages/Xcode_iOS_DDI_Personalized`
//! folder of github.com/doronz88/DeveloperDiskImage (Image.dmg,
//! Image.dmg.trustcache, BuildManifest.plist). Older iOS takes a
//! version-specific DeveloperDiskImage.dmg with its .signature.
//! Developer Mode must be on (Settings › Privacy & Security).

use std::path::{Path, PathBuf};

use idevice::{
    lockdown::LockdownClient, mobile_image_mounter::ImageMounter, provider::IdeviceProvider,
    IdeviceError, IdeviceService,
};
use plist::Value;

const XCODE_DDI: &str = "/Library/Developer/DeveloperDiskImages/iOS_DDI/Restore";

fn e(what: &'static str) -> impl Fn(IdeviceError) -> String {
    move |err| format!("{what}: {err}")
}

/// Developer Mode, and whether an image is mounted.
pub async fn status(provider: &dyn IdeviceProvider) -> Result<(Option<bool>, bool), String> {
    let mut m = ImageMounter::connect(provider)
        .await
        .map_err(e("image mounter"))?;
    let dev_mode = m.query_developer_mode_status().await.ok();
    let mounted = !m
        .copy_devices()
        .await
        .map_err(e("image mounter"))?
        .is_empty();
    Ok((dev_mode, mounted))
}

/// Mounts the image in `dir` (Xcode's on a Mac by default) unless one is mounted.
pub async fn mount(provider: &dyn IdeviceProvider, dir: Option<&Path>) -> Result<String, String> {
    let (dev_mode, mounted) = status(provider).await?;
    if mounted {
        return Ok("already mounted".into());
    }
    if dev_mode == Some(false) {
        return Err(
            "Developer Mode is off (Settings › Privacy & Security › Developer Mode)".into(),
        );
    }
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None if Path::new(XCODE_DDI).is_dir() => PathBuf::from(XCODE_DDI),
        None => {
            return Err("no image folder; pass `ddi mount DIR` (see `mobax-ios --help`)".into())
        }
    };

    let mut m = ImageMounter::connect(provider)
        .await
        .map_err(e("image mounter"))?;
    let legacy = dir.join("DeveloperDiskImage.dmg");
    if legacy.is_file() {
        let image = read(&legacy)?;
        let sig = read(&dir.join("DeveloperDiskImage.dmg.signature"))?;
        m.mount_developer(&image, sig).await.map_err(e("mount"))?;
        return Ok(format!("mounted {}", legacy.display()));
    }

    let manifest_path = dir.join("BuildManifest.plist");
    let manifest = read(&manifest_path)?;
    let (image_path, trust_path) = personalized_paths(&manifest, &dir)?;
    let image = read(&image_path)?;
    let trust = read(&trust_path)?;
    let ecid = {
        let pairing = provider.get_pairing_file().await.map_err(e("pairing"))?;
        let mut l = LockdownClient::connect(provider)
            .await
            .map_err(e("lockdown"))?;
        l.start_session(&pairing).await.map_err(e("lockdown"))?;
        l.get_value(Some("UniqueChipID"), None)
            .await
            .map_err(e("lockdown"))?
            .as_unsigned_integer()
            .ok_or("lockdown: no UniqueChipID")?
    };
    m.mount_personalized(provider, image, trust, &manifest, None, ecid)
        .await
        .map_err(e("mount (needs Apple's TSS server online)"))?;
    Ok(format!("mounted {}", image_path.display()))
}

fn read(p: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(p).map_err(|err| format!("{}: {err}", p.display()))
}

/// The image and trust cache a personalized-image folder holds: the paths its
/// BuildManifest names (Xcode's layout), else Image.dmg and its trustcache.
fn personalized_paths(manifest: &[u8], dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let v: Value =
        plist::from_bytes(manifest).map_err(|err| format!("BuildManifest.plist: {err}"))?;
    let path = |key: &str| {
        v.as_dictionary()?
            .get("BuildIdentities")?
            .as_array()?
            .first()?
            .as_dictionary()?
            .get("Manifest")?
            .as_dictionary()?
            .get(key)?
            .as_dictionary()?
            .get("Info")?
            .as_dictionary()?
            .get("Path")?
            .as_string()
            .map(|p| dir.join(p))
    };
    let image = path("PersonalizedDMG").filter(|p| p.is_file());
    let trust = path("LoadableTrustCache").filter(|p| p.is_file());
    match (image, trust) {
        (Some(i), Some(t)) => Ok((i, t)),
        _ => Ok((dir.join("Image.dmg"), dir.join("Image.dmg.trustcache"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_xcodes_layout_and_falls_back() {
        let tmp = std::env::temp_dir().join(format!("mobax-ios-ddi-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("Firmware")).unwrap();
        std::fs::write(tmp.join("022.dmg"), b"").unwrap();
        std::fs::write(tmp.join("Firmware/022.dmg.trustcache"), b"").unwrap();
        let manifest = |img: &str| {
            let xml = format!(
                "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>BuildIdentities</key><array><dict>\
                 <key>Manifest</key><dict>\
                 <key>PersonalizedDMG</key><dict><key>Info</key><dict><key>Path</key><string>{img}</string></dict></dict>\
                 <key>LoadableTrustCache</key><dict><key>Info</key><dict><key>Path</key><string>Firmware/022.dmg.trustcache</string></dict></dict>\
                 </dict></dict></array></dict></plist>"
            );
            xml.into_bytes()
        };
        let (i, t) = personalized_paths(&manifest("022.dmg"), &tmp).unwrap();
        assert_eq!(
            (i, t),
            (tmp.join("022.dmg"), tmp.join("Firmware/022.dmg.trustcache"))
        );
        let (i, _) = personalized_paths(&manifest("missing.dmg"), &tmp).unwrap();
        assert_eq!(i, tmp.join("Image.dmg"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
