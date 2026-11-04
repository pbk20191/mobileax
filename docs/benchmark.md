# Benchmark

Measured on 2026-05-24 with `n=50` on the Android 15 emulator
`Google sdk_gphone64_arm64` (`1440x3120`, SDK 35), host macOS/aarch64.
Numbers are p50 with p95 in parentheses. `mobax` uses one warm daemon socket;
`adb` starts a fresh adb CLI process per sample.

Reproduce the mobileAX side with `mobax bench -n 50` or machine-readable
`mobax bench -n 50 --json`. Reproduce raw ADB baselines with
`scripts/bench-adb.sh -n 50`; the script keeps the exact ADB commands
inspectable.

The default `mobax bench` suite follows the hot path: 768px JPEG screenshots
for agents, with WebP as a lossy export comparison. PNG is intentionally
left out of the headline `mobax` suite because Android's PNG encoder is a
debug/export path, not an interaction loop path.

## Headline Comparisons

| Command | mobax | adb | Speedup |
|---|---:|---:|---:|
| `screenshot size=768` JPEG vs `adb exec-out screencap -p` PNG | **8.02 ms** (10.76) | 2105.61 ms (2308.38) | **263×** |
| `screenshot max=1` JPEG vs `adb exec-out screencap -p` PNG | **52.60 ms** (71.23) | 2105.61 ms (2308.38) | **40×** |
| `wm_info` vs `adb shell wm size` | **1.34 ms** (2.29) | 45.81 ms (67.83) | **34×** |
| `state top` vs `adb shell dumpsys window` | **2.54 ms** (5.09) | 71.28 ms (107.80) | **28×** |
| `ping` vs `adb shell true` | **1.61 ms** (2.64) | 27.65 ms (36.33) | **17×** |

## mobileAX Warm-Socket Suite

| Command | p50 | p95 | bytes |
|---|---:|---:|---:|
| `ping` | 1.61 ms | 2.64 ms | 4 |
| `info` | 1.64 ms | 2.80 ms | 9 |
| `wm_info` | 1.34 ms | 2.29 ms | 81 |
| `state top` | 2.54 ms | 5.09 ms | 60 |
| `wait_for_idle idle_ms=0 timeout_ms=1000` | 1.49 ms | 3.40 ms | 15 |
| `dump_active` | 4.58 ms | 6.92 ms | 5960 |
| `dump` | 6.62 ms | 9.58 ms | 10317 |
| `screenshot size=480` JPEG q80 | 3.42 ms | 6.26 ms | 12069 |
| `screenshot size=768` JPEG q80 | 8.02 ms | 10.76 ms | 22414 |
| `screenshot size=1080` JPEG q80 | 12.33 ms | 17.34 ms | 34611 |
| `screenshot size=1080 q=95` JPEG | 11.89 ms | 19.83 ms | 83811 |
| `screenshot max=1` JPEG q80 | 52.60 ms | 71.23 ms | 142014 |
| `screenshot size=768 q=95` JPEG | 6.35 ms | 10.34 ms | 49390 |
| `screenshot size=768 fmt=webp` q80 | 62.26 ms | 78.54 ms | 12090 |

## Raw ADB Baseline

| Command | p50 | p95 | bytes |
|---|---:|---:|---:|
| `adb shell true` | 27.65 ms | 36.33 ms | 0 |
| `adb shell getprop ro.build.version.sdk` | 31.38 ms | 41.57 ms | 3 |
| `adb shell wm size` | 45.81 ms | 67.83 ms | 25 |
| `adb shell settings get system screen_brightness` | 45.33 ms | 57.92 ms | 4 |
| `adb shell dumpsys window` | 71.28 ms | 107.80 ms | 87667 |
| `adb exec-out screencap -p` | 2105.61 ms | 2308.38 ms | 2024218 |

## Agent Token/Turn Benchmark

What an agent pays per task over MCP (`mobax mcp`): calls and approximate
response tokens. `scripts/agent-bench.py` replays the fixed tool-call
sequences in `bench/agent/*.json` (the fewest calls today's tools need, then
a check). No LLM runs, so it is free and repeatable. Spec:
`docs/superpowers/specs/2026-11-04-agent-first-direction.md`.

- **Approximate tokens** = ASCII chars / 4 (rounded up) + one per non-ASCII
  char. A fixed yardstick for comparing runs, not a tokenizer; changing it
  means re-recording the baseline.
- `setup` steps are not counted. They may call host adb (`{"adb": [...]}`),
  since MCP has no device shell.
- Tokens and time are medians over runs. Action replies report what changed
  on screen, which depends on timing, so tokens vary a little from run to run.
- **Stable means:** calls identical, total tokens within ±5%. The `±%`
  column is the run-to-run spread; above 5% it is marked `!` and printed as
  `UNSTABLE`, but does not fail the run. s2 (whether the 4th swipe still
  scrolls), s3 and s4 (camera start-up after the permission tap) routinely
  exceed it: the action replies differ, not the calls.
- **Fixed before this baseline** (`fix/open-settle`): `open` could report
  "settled" on an empty screen (a cold start past its 6 s settle) or on the
  screen before it (a late event from home); the next tap missed. A slow
  emulator (a long-running one with its guest swapping, or a busy host) makes
  every action several times slower: cold-boot it before measuring.
- s1 lands on 651 or 817 tokens per session (no spread within one): the
  tap's reply differs between sessions. Compare s1 across sessions with care.

```
ANDROID_HOME=… python -I scripts/agent-bench.py --runs 3 --compare bench/agent/baseline.json
```

Baseline (2026-11-04, emulator `Medium_Phone`, SDK 36, ko-KR, host GPU,
fresh cold boot, idle host; `mobax` from `fix/open-settle`, 3 runs;
`bench/agent/baseline.json`). A repeat run and a reversed-order run stayed
within ±5% on the total (+3.6%, +2.7%) with identical calls:

| Scenario | Calls | Tokens (p50) | ± (max−min) | Time (p50) |
|---|---:|---:|---:|---:|
| s1-settings-item | 3 | 651 | 0 | 2.3 s |
| s2-long-list | 8 | 1847 | 5 | 7.6 s |
| s3-search-form | 5 | 1297 | 1 | 4.4 s |
| s4-permission-popup | 5 | 526 | 116 | 2.9 s |
| s5-heavy-screen | 3 | 260 | 0 | 0.8 s |
| **total** | **24** | **4581** | | |
| `tools/list` (once per session) | | 2668 | | |

| Scenario | Task | Measures |
|---|---|---|
| s1 | Settings → "디스플레이 및 터치" | basic navigation |
| s2 | Accessibility list, last item (`tap` with `scroll: true`; the baseline used 4 swipes) | scrolling (later: find-with-scroll) |
| s3 | Settings search "배터리" → result | typing, result check |
| s4 | Fresh camera: location-tag prompt, location permission | popups (later: auto-handling) |
| s5 | Recents (two fixed cards), one `ui` | output size of a full-screen system UI |

Findings from writing the scenarios:

- A bare-word target (`다음`, `Gmail`) is read as a class-name selector and
  misses; scenarios use `:has-text("…")`.
- The tool list (2668, every session) is more than half of all five scenarios'
  replies together (4554): the cheapest cut is in tool descriptions.
- Settings search is its own package (`com.google.android.settings.intelligence`).
- `wait` returning at once while a page is still settling makes the first
  swipe land at a slightly different scroll position each run.

### Changes measured against the baseline

| Change | Calls | Scenario tokens | Per-session cost |
|---|---:|---:|---:|
| Tool description diet (`mcp/tool-diet`): shared arguments (`device`, `observe`, `timeout_ms`) explained once in the instructions; descriptions no longer repeat the instructions; no empty `required` | ±0 | ±0 (noise) | 2979 → 1959 (−34%): tools/list 2668 → 1605, instructions 311 → 354 |
| The server rejects unknown arguments itself (naming the ones the tool takes), so schemas drop `additionalProperties:false` | ±0 | ±0 | 1959 → 1820 (−39% overall): tools/list 1605 → 1466 |
| `tap`/`find` `scroll: true` (pages the biggest list with ACTION_SCROLL_FORWARD until the target shows); s2 now uses it | s2 8 → 4 | s2 1847 → 673 (−64%) | tools/list 1466 → 1513 (+47 for the argument) |
