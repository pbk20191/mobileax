# Sharp edges

- Settings provider rejects our `app_process` identity; `SettingsDirect`
  uses `IActivityManager.getContentProviderExternal` (the `cmd content`
  path).
- `am start` goes via `IActivityTaskManager.startActivityAsUser` with
  `callingPackage="com.android.shell"`; the system Context can't host
  `startActivity` from our anonymous process.
- The instrumentation (`mobax use`'s default where `mobaxd-instr.apk` is
  found; `--instr` to insist, `--jar` to skip it):
  - It is two installed APKs (Android 10 / SDK 29 or later): the daemon
    (`dev.mobileax.daemon`) and its androidTest APK
    (`dev.mobileax.daemon.instr`), which instruments it. Some OEMs need
    "Install via USB" (or similar) enabled once before they can be
    installed; until then `use` runs the jar. `mobax drop` uninstalls both
    (`--keep-apk` keeps them).
  - One per device: UiAutomation has one connection. A second
    `mobax use --instr --port N` is refused; a UiAutomator test (or
    `uiautomator dump`) started while it runs is cut off.
  - Monkey mode is on while it runs (`ActivityManager.isUserAMonkey()`);
    AMS clears it when the daemon goes.
  - It is an app uid with adopted shell permissions: binder calls name its
    own package as `callingPackage`, it declares `QUERY_ALL_PACKAGES`, and
    what needs the shell uid goes through `Shell`. On SDK 34+ that is
    `executeShellCommandRwe`, which does no quoting, so the script goes in on
    stdin as one line; on SDK 29–33 the script, stdin data, stderr, exit
    code and pid are files in the app's external `files/shell/` directory
    and `executeShellCommand` runs `sh <file>`. `-e shell file` on
    `am instrument` forces the file path.
- Off-screen nodes: a list keeps a few rows past the screen in the tree. They
  are in `dump` with `off` (and `mobax find` prints `[off-screen below]`);
  `wait` and the daemon's node actions (`node_click`, `wait_for_text`, …)
  treat them as not there yet; `tap` scrolls toward one and taps it. Rows the
  list hasn't made yet are in no tree: scroll (`scroll: true`, `swipe`).
- The screenshot mirror is made on demand, not kept up: a mirror
  recomposites every frame the screen changes (SDK 36 while scrolling:
  surfaceflinger about twice the CPU, the emulator ~15% more). Streams make
  one and hold it; screenshots use one that is up, or make one when three
  come within 5 s; one unused for 30 s is closed. Single screenshots go
  through DisplayCapture below (~20-40 ms more than a warm mirror).
- Screenshots without a mirror (no CAPTURE_VIDEO_OUTPUT and no SurfaceControl
  display): `see` and element screenshots ask SurfaceFlinger for one frame at
  the size sent (`DisplayCapture`: `IWindowManager.captureDisplay` on 34+,
  `SurfaceControl.captureDisplay` on 31–33, `SurfaceControl.screenshot` on
  29–30), which needs only READ_FRAME_BUFFER; then UiAutomation.takeScreenshot.
  The 29–30 call can't turn the frame, so a turned screen there goes to
  UiAutomation. `setprop debug.mobax.mirror 0` before `mobax use` skips the
  mirror to try this path.
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
- `mobax logs QUERY` takes Android Studio's Logcat filter (`package:mine
  level:WARN -tag:chatty`, `~:` regex, `=:` exact, `|` `&` `( )`) and matches
  it on the host over `logcat -v threadtime -v uid`. `package:` (and `--pkg`)
  goes by the entry's uid, so it covers an app's earlier, dead processes and
  follows it across restarts; `package:mine` is the app in front when the
  command starts. A uid several packages share (Settings runs as system,
  uid 1000, with system_server) is told apart by process name, so there only
  processes running when the command starts count. On Android 11+ a query
  limited to packages is narrowed on the device (primary user's uids), so
  only those lines leave it: logcat `--uid` on Android 11+, the daemon's
  `logcat_uid` (logcat piped through grep) on Android 10 and older. Quote the query as one argument when it has spaces inside a
  value: `'message:"timed out"'`. `is:crash` is `AndroidRuntime` errors and
  native crash lines; `age:` is the same as `--since`.
