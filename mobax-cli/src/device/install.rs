// The daemon APK on a device: whether the installed one is ours, what
// `pm install` said, and how the instrumentation is started.

/// Where the APK is pushed before `pm install`.
pub(crate) const DEVICE_APK: &str = "/data/local/tmp/mobaxd.apk";
/// The [`fingerprint`] of the APK last installed, written after a successful install.
pub(crate) const DEVICE_FP: &str = "/data/local/tmp/mobaxd.fp";
pub(crate) const PKG: &str = "dev.mobileax.daemon";

/// FNV-1a 64 of the APK bytes, as 16 hex digits: enough to tell builds
/// apart (same version string or not), no crypto needed.
pub(crate) fn fingerprint(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub(crate) enum InstallOutcome {
    Ok,
    /// Installed under another key (another machine's or CI's debug key).
    SignatureMismatch,
    Failed(String),
}

pub(crate) fn parse_pm_install(stdout: &str) -> InstallOutcome {
    if stdout.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") {
        InstallOutcome::SignatureMismatch
    } else if stdout.lines().any(|l| l.trim() == "Success") {
        InstallOutcome::Ok
    } else {
        InstallOutcome::Failed(stdout.trim().to_string())
    }
}

/// The `am instrument -w` log says the instrumentation is over (it finished
/// or its process died): `INSTRUMENTATION_CODE:` is its last line.
pub(crate) fn instrumentation_ended(log: &str) -> bool {
    log.lines().any(|l| l.starts_with("INSTRUMENTATION_CODE:"))
}

/// Starts the daemon detached; `am instrument -w` stays alive for its whole
/// life (it holds the UiAutomation connection) and logs its status to `log`.
pub(crate) fn instrument_cmd(port: u16, log: &str) -> String {
    format!(
        "nohup am instrument -w --no-hidden-api-checks -e port {port} {PKG}/.MobaxInstrumentation > {log} 2>&1 &"
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn fingerprints_are_fnv1a_64_hex() {
        assert_eq!(super::fingerprint(b""), "cbf29ce484222325");
        assert_eq!(super::fingerprint(b"a"), "af63dc4c8601ec8c");
        assert_ne!(super::fingerprint(b"apk-1"), super::fingerprint(b"apk-2"));
    }

    #[test]
    fn pm_install_output_is_classified() {
        use super::{parse_pm_install, InstallOutcome::*};
        assert!(matches!(parse_pm_install("Performing Streamed Install\nSuccess\n"), Ok));
        assert!(matches!(parse_pm_install("Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: Existing package dev.mobileax.daemon signatures do not match newer version; ignoring!]"), SignatureMismatch));
        assert!(matches!(parse_pm_install("Failure [INSTALL_FAILED_OLDER_SDK: ...]"), Failed(m) if m.contains("INSTALL_FAILED_OLDER_SDK")));
    }

    #[test]
    fn a_log_tells_when_the_instrumentation_has_ended() {
        use super::instrumentation_ended;
        assert!(!instrumentation_ended("INSTRUMENTATION_STATUS: mobaxd=mobaxd listening on @mobaxd_9008\nINSTRUMENTATION_STATUS_CODE: 0\n"));
        assert!(instrumentation_ended("INSTRUMENTATION_STATUS: mobaxd=fatal: init failed: x\nINSTRUMENTATION_RESULT: mobaxd=fatal: init failed\nINSTRUMENTATION_CODE: 2\n"));
        assert!(instrumentation_ended("INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0\n"));
    }

    #[test]
    fn instrumentation_is_started_detached_with_the_port() {
        let c = super::instrument_cmd(9009, "/data/local/tmp/mobax.log");
        assert!(c.starts_with("nohup am instrument -w --no-hidden-api-checks -e port 9009 dev.mobileax.daemon/.MobaxInstrumentation"));
        assert!(c.ends_with("> /data/local/tmp/mobax.log 2>&1 &"));
    }
}
