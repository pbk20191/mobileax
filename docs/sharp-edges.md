# Sharp edges

- The daemon is an installed APK (`dev.mobileax.daemon`, Android 10 / SDK 29
  or later) run by `am instrument`. Some OEMs need "Install via USB" (or
  similar) enabled once before `mobax use` can install it. `mobax drop`
  uninstalls it (`--keep-apk` keeps it).
- One daemon per device: UiAutomation has one connection. A second
  `mobax use --port N` is refused; a UiAutomator test (or `uiautomator dump`)
  started while the daemon runs is cut off.
- Monkey mode is on while the daemon runs (`ActivityManager.isUserAMonkey()`
  is true); some apps and Settings hide or block actions for a monkey. AMS
  clears it when the daemon goes.
- The daemon is an app uid with adopted shell permissions: binder calls name
  its own package as `callingPackage`, it declares `QUERY_ALL_PACKAGES` to
  see every package, and what needs the shell uid itself goes through `Shell`
  (a `sh` the framework starts). On SDK 34+ that is `executeShellCommandRwe`,
  which does no quoting, so the script goes in on stdin as one line; on SDK
  29–33 (Rwe is hidden there) the script, stdin data, stderr, exit code and
  pid are files in the app's external `files/shell/` directory and
  `executeShellCommand` runs `sh <file>`. Pid and exit code carry a random
  per-call nonce. `-e shell file` on `am instrument` forces the file path.
- SDK 29 (Android 10): the mirror virtual display is refused
  (`CAPTURE_VIDEO_OUTPUT` isn't covered by adopted permissions there), so
  `see` falls back to UiAutomation screenshots and the live streams don't
  start; and `injectInputEvent(…, waitForAnimations)` doesn't exist, so
  input waits for animations as before.
- Settings provider: `SettingsDirect` uses
  `IActivityManager.getContentProviderExternal` (the `cmd content` path).
- `am start` goes via `IActivityTaskManager.startActivityAsUser` with our
  package as `callingPackage`.
- `BroadcastReceiver` registrations from our Context aren't routed —
  `appCounts` cache TTL'd at 4.5 s instead.
- Most binder lookups go via reflection so the jar compiles against the
  public SDK. If a new SDK level renames a method, widen the matcher in
  the relevant helper.

## adb and the Android SDK

- Two SDKs on one machine (say Android Studio's and the one `android info`
  reports) ship different adb versions. Whichever client talks to the other's
  server kills and restarts it, which drops open connections and can hang a
  pipeline waiting on output. Point everything at one SDK, or pin
  `MOBILEAX_ADB`.
- A set-but-empty `ANDROID_HOME` makes `android info` report an empty SDK
  path; `mobax` drops empty `ANDROID_HOME` / `ANDROID_SDK_ROOT` before asking it.
- `android emulator start` has no `-gpu` flag; host GPU rendering has to come
  from `hw.gpu.mode=host` in the AVD's `config.ini`, or `ForceGpuHost = on`
  in `~/.android/advancedFeatures.ini` ([Emulators](emulator.md)).
- Running one AVD more than once needs `-read-only` on every instance, and a
  read-only emulator refuses snapshot save and load ("Snapshot save is
  disabled because "-read-only" was specified"). Keep the emulator you reset
  with snapshots as a single writable instance.
- After a snapshot load or a reboot done by another tool, the daemon on the
  device may be gone. `mobax` and the MCP server start it again on the next
  call; `@N` refs from before are refused and the current screen comes back.
- `logcat -t N` takes the last N lines *before* its priority, tag and regex
  filters, so `logcat -t 50 *:E` is often empty. `mobax logs --level / --tag /
  --grep / --exclude` reads the buffer and keeps the last N matching lines
  itself.
- `mobax logs --grep` is logcat's own regex (ECMAScript, on the device). It has
  no case-insensitive flag, so `-i` spells each letter both ways (`[aA]`);
  letters at the ends of a class range like `[a-z]` stay as written.
  `--exclude` is plain text, matched on the host.
- `mobax logs --since 5m` reads the device's clock, not the host's.
