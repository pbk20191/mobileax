# 데몬 instrumentation 이동 — 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 데몬을 `app_process`에서 설치된 APK의 instrumentation 프로세스(`am instrument -w`)로 옮긴다. UiAutomation은 공식 API로 얻고, monkey는 정식으로 켠다. 모든 verb의 동작은 유지한다.

**Architecture:**
- 같은 APK(`dev.mobileax.daemon`)에 `androidx.test.runner.MonitoringInstrumentation`(AndroidJUnitRunner의 부모) 서브클래스를 둔다. AMS가 fork한 프로세스의 main looper 위에서 지금의 서버·코루틴 구조를 그대로 돌린다.
- instrumentation, context, 인자는 `InstrumentationRegistry`로 얻는다. UiAutomation은 `UiDevice`와 같은 인스턴스를 쓴다. 데몬 경로 중 androidx.test로 바꿔도 결과와 성능이 같은 곳은 바꾼다(spec 5.1).
- shell 권한이 필요한 일은 두 갈래로 처리한다. 권한 검사는 `adoptShellPermissionIdentity()`로 통과하고, shell uid 자체가 필요한 일은 `executeShellCommand*`(shell uid로 실행)로 넘긴다.
- 호스트 쪽은 기동과 정리 경로만 바뀐다. 기존 jar push + `app_process` 대신 APK 설치 + `am instrument`를 쓴다.

**Tech Stack:** Kotlin(데몬, AGP, kotlinx-coroutines, androidx.test runner/monitor/uiautomator), Rust std-only(CLI), adb, GitHub Actions

**Spec:** `docs/superpowers/specs/2026-11-03-daemon-instrumentation-design.md`

## Global Constraints

- `minSdk = 31`, `compileSdk = 36`, `targetSdk = 36`. SDK 29~30용 갈래 코드는 쓰지 않는다.
- 패키지 `dev.mobileax.daemon`, 소켓 이름 `mobaxd_<port>`(abstract)는 그대로 둔다. 와이어 프로토콜(`docs/wire.md`)은 바뀌지 않는다.
- 릴리즈 APK도 `signingConfigs.debug`로 서명한다. 키 파일과 CI 시크릿은 쓰지 않는다.
- `app_process` 기동 경로는 남기지 않는다(전체 이동).
- 데몬은 기기당 하나다. 다른 `--port`로 `use`하면 오류를 낸다. 메시지에는 `mobax drop` 후 다시 하라는 안내를 넣는다.
- CLI는 third-party 의존성 없이 std만 쓴다(`mobax-cli/Cargo.toml` 주석).
- 데몬 의존성은 `androidx.test:runner`, `androidx.test:monitor`, `androidx.test.uiautomator:uiautomator`를 추가한다. 구현 시점의 최신 안정 버전을 정확한 버전으로 고정한다.
- `UiAutomation` 연결 플래그는 한 곳의 상수 `UA_FLAGS = UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES`로 두고, `Configurator.getInstance().uiAutomationFlags`에도 같은 값을 넣는다.
- androidx.test로 바꾼 경로가 아래 기준선을 해치거나 결과를 바꾸면 그 경로는 지금 코드를 유지하고, 이유를 코드 주석에 한 줄 남긴다(spec 5.1).
- **Task 1의 확인 1(소켓 도달)이나 확인 2(monkey 켜기·끄기)가 실패하면 프로젝트를 중단한다.** 대체 방식(TCP + `adb forward`, `setActivityController`)으로 가지 않는다.
- 성능 기준선(SDK 36 에뮬레이터 `Medium_Phone`)에서 잡음을 넘는 회귀가 없어야 한다.
  - 유휴 `ui` 약 265ms
  - tap→하위 화면 중앙값 약 976ms
  - Recents 키 응답 약 627ms
  - 드래그 약 126ms
  - 첫 `dump_active` 약 158ms
- 기기 작업은 저장소 규칙을 따른다.
  - 에뮬레이터는 `android emulator start Medium_Phone`으로 띄운다. 새 AVD는 만들지 않는다.
  - 프로세스는 이 저장소 `target\` 바이너리만 PID로 종료한다. 이미지 이름으로 `taskkill`하지 않는다.

## Review Focus

1. **`am instrument` 프로세스가 죽은 경우** (USB 재연결, `adb kill-server` 등): 데몬도 죽는다. 다음 명령은 "no daemon … start it with `mobax use`"로 실패해야 하고, `use`는 재설치 없이 다시 띄워야 한다. → Task 7 Step 7
2. **기기에 다른 서명으로 설치돼 있는 경우** (다른 PC나 CI에서 만든 빌드): `use`가 자동으로 지우고 다시 설치해야 한다. → Task 7 Step 1(파싱 테스트), Step 7(기기 확인)
3. **읽을 수 없는 파일을 `cp`로 받거나 쓸 수 없는 경로로 보내는 경우**: 오류로 끝나야 한다. "pulled 0 bytes" 같은 성공으로 끝나면 안 된다. → Task 4 Step 4
4. **데몬이 비정상 종료한 경우** (크래시, `am force-stop`): 기기가 monkey 상태로 남으면 안 된다. → Task 1 확인 2, Task 8 Step 3
5. **다른 UiAutomation이 이미 연결돼 있는 경우** (사용자의 UiAutomator 테스트 실행 중): `use`가 6초 타임아웃으로 멈추지 말고 원인을 담은 오류를 내야 한다. → Task 2 Step 5

---

### Task 1: 위험 확인 스파이크 (버리는 코드)

이 작업의 코드는 커밋하지 않는다. 결과만 계획 문서 끝의 "스파이크 결과" 절에 적고 커밋한다.

**Files:**
- Create (임시, 커밋 안 함): `src/dev/mobileax/daemon/SpikeInstrumentation.kt`
- Modify (임시, 커밋 안 함): `AndroidManifest.xml`, `build.gradle.kts`(`minSdk = 31`, release `signingConfig = signingConfigs.getByName("debug")`)
- Modify (커밋): 이 계획 문서 끝에 "스파이크 결과" 절 추가

**Interfaces:**
- Produces: 확인 1~5의 성공·실패 기록. 실패한 항목이 있으면 이후 Task의 범위가 바뀐다.

- [ ] **Step 1: 스파이크 instrumentation 작성**

`SpikeInstrumentation : androidx.test.runner.MonitoringInstrumentation`이 하는 일. 스파이크 동안에만 `build.gradle.kts`에 Task 2의 androidx.test 의존성을 임시로 추가한다.
- `onCreate`(main 스레드)에서 `LocalServerSocket("mobaxd_spike")`를 연다.
- 별도 스레드에서 accept하고, 길이 접두(`uint32 BE`) 프레임 `ping`에 `pong`으로 답한다. 와이어 형식과 같다.
- `start()`는 부르지 않는다.
- 아래 결과를 `sendStatus(0, Bundle().apply { putString("spike", line) })`로 내보낸다.
  - `ActivityManager.getService().setUserIsMonkey(true)`(리플렉션) 결과
  - `ActivityManager.isUserAMonkey()` 값
  - Work 스레드에서 `getUiAutomation()`으로 연결한 결과
  - 3-arg `injectInputEvent(ev, false, false)` 리플렉션 결과
  - `adoptShellPermissionIdentity()` 아래에서 확인 4의 각 항목 결과
  - 확인 6: `Configurator.getInstance().uiAutomationFlags = UA_FLAGS` 후 `UiDevice.getInstance(this)`를 만들고, 그 `uiAutomation`이 `getUiAutomation(UA_FLAGS)`와 같은 인스턴스(`===`)인지
  - 확인 7: `super.onCreate(arguments)`만 부르고 `start()`는 부르지 않은 상태에서, 5분 동안 프로세스와 `am instrument -w`가 살아 있는지. `MonitoringInstrumentation`이 스스로 `finish()`하지 않는지를 본다.

manifest에는 다음을 추가한다.
```xml
<instrumentation android:name="dev.mobileax.daemon.SpikeInstrumentation" android:targetPackage="dev.mobileax.daemon" />
```

- [ ] **Step 2: 빌드·설치·실행**

```bash
./gradlew -q mobaxJar
adb push build/mobax.jar /data/local/tmp/mobaxd.apk
adb shell pm install -r -t /data/local/tmp/mobaxd.apk
adb shell "nohup am instrument -w --no-hidden-api-checks dev.mobileax.daemon/.SpikeInstrumentation > /data/local/tmp/spike.log 2>&1 &"
```

Expected: `pm install` → `Success`. `spike.log`에 `INSTRUMENTATION_STATUS: spike=…` 줄들이 찍힌다.

- [ ] **Step 3: 확인 1, 소켓 도달 (실패하면 중단)**

```bash
adb forward tcp:19999 localabstract:mobaxd_spike
```
`ping` 프레임을 보내 `pong`을 받는다. 이어서 CLI의 adb server 경로(`host:transport:SERIAL` + `localabstract:mobaxd_spike`)로도 확인한다. `mobax-core::adb_server::device_stream`을 쓰는 임시 Rust 테스트나 `mobax dev ping --port`로 하고, 커밋하지 않는다.

확인이 끝나면 `adb forward --remove tcp:19999`로 정리한다.

Expected: 두 경로 모두 `pong`.

- [ ] **Step 4: 확인 2, monkey 켜기·끄기 (실패하면 중단)**

- `spike.log`에서 `setUserIsMonkey` 성공과 `isUserAMonkey=true`를 확인한다.
- `adb shell am force-stop dev.mobileax.daemon` 후 monkey 설정 없이 스파이크를 다시 띄워서 `isUserAMonkey=false`가 보고되는지 확인한다. 끝난 instrumentation이 표시를 되돌리는지 보는 것이다.
- 크래시 경로도 확인한다. 스파이크가 일부러 `throw`하게 한 번 실행한 뒤 다시 띄워 false인지 본다.

Expected: 켜짐 → true. force-stop 후와 크래시 후 → false.

AMS main 소스상으로는 instrumentation이 끝나 UiAutomation 연결이 정리될 때(`SHUTDOWN_UI_AUTOMATION_CONNECTION_MSG`) `mUserIsMonkey = false`로 되돌린다. 이 확인은 그 동작이 에뮬레이터에서도 같은지 보는 것이다. **false로 돌아오지 않으면** 실패로 보지 말고, `drop`이 `setUserIsMonkey(false)`를 직접 불러야 한다는 결론으로 기록한다.

- [ ] **Step 5: 확인 3~5 (실패해도 중단하지 않고 사용자에게 보고)**

- **확인 3:** `getUiAutomation()` 객체로 3-arg `injectInputEvent`를 호출해 `true`를 받고, 홈 화면이 탭에 반응하는지 본다.
- **확인 4 (adopt 아래 권한 작업):**
  - 미러 가상 디스플레이 생성. `DisplayManager.createVirtualDisplay`, 자기 패키지 컨텍스트로 한다.
  - `Settings.Global.putInt`
  - `content://sms` 쿼리
  - PackageInstaller 세션 생성 후 abandon
  - `IClipboard.getPrimaryClip`/`setPrimaryClip`
- **확인 5:** `UiAutomation.executeShellCommandRwe("sh -c 'cat /system/build.prop; echo __MOBAX_EXIT:\$? >&2'")`의 stderr에 `Permission denied`와 `__MOBAX_EXIT:1`이 오는지 본다.

**클립보드에 대해:** shell uid는 `ClipboardService`의 uid 예외(`Clipboard.kt` 주석)로 통과했지만, 앱 uid는 adopt만으로 그 예외에 해당하지 않을 수 있다. 실패하면 `executeShellCommand`로 할 수 있는 shell 측 수단이 있는지 확인한다. 예를 들어 이 SDK의 `cmd clipboard` 존재 여부를 본다. 결과를 기록한다.

- [ ] **Step 5b: 확인 6~7 (실패해도 중단하지 않고 사용자에게 보고)**

`spike.log`에서 확인 6의 `same=true`와 확인 7의 5분 생존을 본다. 확인 7이 실패하면 `start()`를 부르고 `onStart()`를 오버라이드해 반환하지 않게 하는 방식을 시험한다. 예를 들어 `CountDownLatch`로 종료 요청까지 기다린다. 결과를 기록한다.

- [ ] **Step 6: 결과 기록, 중단 판정**

- 이 계획 문서 끝에 "스파이크 결과" 절을 만들고 확인 1~5와 클립보드 결과를 한 줄씩 적는다.
- 확인 1이나 2가 실패하면 여기서 멈추고 사용자에게 보고한다. 브랜치는 버리는 쪽으로 사용자와 정한다.
- 확인 3~7이나 클립보드가 실패하면 멈추고 사용자에게 보고한다. 해당 verb나 진입 방식을 어떻게 할지 정해야 하기 때문이다.
- 임시 파일은 되돌린다(`git checkout -- AndroidManifest.xml build.gradle.kts`, 스파이크 파일 삭제).

```bash
git add docs/superpowers/plans/2026-11-03-daemon-instrumentation.md
git commit -m "docs: instrumentation spike results"
```

---

### Task 2: 빌드 설정과 instrumentation 진입점

**Files:**
- Modify: `build.gradle.kts`
  - `minSdk = 31`
  - `implementation`에 `androidx.test:runner`, `androidx.test:monitor`, `androidx.test.uiautomator:uiautomator`를 추가한다(정확한 버전 고정)
  - release `signingConfig = signingConfigs.getByName("debug")`
  - 머리 주석과 `mobaxJar` 설명을 instrumentation 기준으로 바꾼다.
- Modify: `AndroidManifest.xml`. `<instrumentation android:name="dev.mobileax.daemon.MobaxInstrumentation" android:targetPackage="dev.mobileax.daemon" />`를 넣는다. 주석은 "installed and run via `am instrument`"로 바꾼다.
- Create: `src/dev/mobileax/daemon/MobaxInstrumentation.kt`
- Modify: `src/dev/mobileax/daemon/Main.kt`. 진입점 역할을 없애고 init 로직(`initDaemon`, `enableInteractiveWindows`)만 `internal object DaemonInit`으로 남긴다. 파일 이름은 `DaemonInit.kt`로 바꿔도 된다.
- Modify: `src/dev/mobileax/daemon/Binders.kt`. `systemContextLazy`, `appContextLazy`, `shellApplication()`을 지우고 `service()`, `asInterface()`, `iPackageManager()` 등 binder 도우미만 남긴다.
- Modify: `src/dev/mobileax/daemon/Logx.kt`. `boot`/`bootWarn`이 `System.err`와 함께 `Instrumentation.sendStatus`로도 내보낸다.
- Modify: `src/dev/mobileax/daemon/Screenshot.kt`. `initDisplayManager()`가 `Binders.appContext()` 대신 `ctx`를 쓰고, shell 이름 wrapper를 지운다.
- Modify: `proguard-rules.pro`. `MobaxInstrumentation`을 keep한다. manifest에서 이름으로 참조되기 때문이다. androidx.test가 리플렉션으로 찾는 클래스 때문에 R8 경고나 런타임 오류가 나면 해당 keep 규칙만 추가한다.
- Create: `src/dev/mobileax/daemon/Daemon.kt`

**Interfaces:**
- Produces:
  - `class MobaxInstrumentation : androidx.test.runner.MonitoringInstrumentation`
  - `Daemon.kt`의 최상위 선언. 상태를 갖지 않고 `InstrumentationRegistry`에서 읽기만 한다. 이후 Task의 `Shell`과 각 핸들러가 쓴다.
    - `internal const val UA_FLAGS: Int = UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES`
    - `internal val instr: Instrumentation get() = InstrumentationRegistry.getInstrumentation()`
    - `internal val ctx: Context get() = instr.context`
    - `internal val ua: UiAutomation get() = instr.getUiAutomation(UA_FLAGS)` (같은 플래그면 캐시된 인스턴스가 나온다)
    - `internal val device: UiDevice by lazy { Configurator.getInstance().uiAutomationFlags = UA_FLAGS; UiDevice.getInstance(instr) }`
  - `Logx.status(line: String)`
  - 인자 `-e port N`(기본 9008)
- Consumes: `Loop.init()`, `MobaxServer`, `Dispatch`, `Server.Handlers`(`sysCtx` 이름은 `ctx`로 바꾼다)

- [ ] **Step 1: `MobaxInstrumentation` 작성**

- **`onCreate(arguments: Bundle)`** (main 스레드, ActivityThread의 main looper가 이미 돈다):
  1. `super.onCreate(arguments)`. `InstrumentationRegistry`가 이때 등록된다.
  2. `port = InstrumentationRegistry.getArguments().getString("port")?.toInt() ?: 9008`
  3. `Loop.init()`
  4. `MobaxServer(port, Dispatch(ready, onQuit = { … }))`를 시작한다. `onQuit`은 서버를 닫고 `finish(0, Bundle())`을 부른다.
  5. `Loop.scope.launch { ready.complete(DaemonInit.init()) }`
- `start()`는 부르지 않는다. `onStart`를 쓰지 않기 때문이다. Task 1 확인 7이 다른 방식을 정했으면 그 방식을 따른다.
- **`DaemonInit.init()`** (`withContext(Loop.Work)`):
  1. `ua`를 처음 읽어 연결한다(`getUiAutomation(UA_FLAGS)`). 현재 `connect(1)`과 같은 플래그다. 이어서 `device`를 만들어 같은 인스턴스를 쓰게 한다.
  2. `ua.adoptShellPermissionIdentity()`
  3. `ua.setRunAsMonkey(true)`. 실패하면 `Logx.bootWarn`만 남긴다.
  4. `enableInteractiveWindows(ua)`
  5. 지금처럼 핸들러를 조립하고 warmup을 띄운다.
- 지울 것:
  - `disableHiddenApiRestrictions`: `--no-hidden-api-checks`가 대신한다.
  - `initLikeActivityThreadMain`, `setDdmAppName`, `logSlowMainLooper`(main looper는 이제 ActivityThread 소유), `connectedUiAutomation`, `createUiAutomation`, `connectWithRetry`, `invokeConnect`, `registerShutdownHook`
- 부팅 줄은 지금과 같은 문구로 남긴다: `mobaxd listening on @mobaxd_N`, `mobaxd ready (sdk=N)`.
- monkey 줄을 추가한다: `mobaxd monkey before=<false|true> after=<false|true>`. `before`는 `setRunAsMonkey` 전에 읽은 `ActivityManager.isUserAMonkey()`이고, `after`는 그 후의 값이다. `dumpsys`에는 이 상태가 나오지 않아서, Task 8의 해제 확인이 이 `before` 값을 쓴다.

- [ ] **Step 2: `Logx.status` 구현**

`InstrumentationRegistry`가 등록된 뒤라면(등록 전에는 `getInstrumentation()`이 예외를 던지므로 잡아서 구분한다) `instr.sendStatus(0, Bundle().apply { putString("mobaxd", line) })`를 부르고, 아니면 `System.err`만 쓴다. `boot`/`bootWarn`/`fatal`이 이를 부른다.

- [ ] **Step 3: 빌드**

Run: `./gradlew -q mobaxJar`
Expected: 종료 코드 0, `build/mobax.jar` 생성.

- [ ] **Step 4: 기기에서 수동 기동해 확인**

```bash
adb push build/mobax.jar /data/local/tmp/mobaxd.apk && adb shell pm install -r -t /data/local/tmp/mobaxd.apk
adb shell "nohup am instrument -w --no-hidden-api-checks -e port 9008 dev.mobileax.daemon/.MobaxInstrumentation > /data/local/tmp/mobax.log 2>&1 &"
```

그 뒤 `MOBILEAX_JAR`가 아닌 기존 CLI로 `mobax ui`, `mobax tap X Y`, `mobax see out.png`를 실행한다. 지금 CLI의 빠른 경로는 ping만 보므로 그대로 붙는다.

Expected:
- `mobax.log`에 `mobaxd listening`, `mobaxd ready`, `mobaxd monkey before=false after=true`가 있다.
- 세 명령이 정상이다.
- `see`의 미러 로그에 wrapper 언급이 없다(`adb logcat -d | grep "screenshot: display ctx"`).

- [ ] **Step 5: UiAutomation 연결 실패를 원인과 함께 보고하기 (Review Focus 5)**

`getUiAutomation`이 예외를 던지면(이미 다른 UiAutomation이 연결된 경우) 다음을 한다.
- `Logx.fatal("UiAutomation unavailable: <예외 메시지> — another UiAutomation (e.g. a UiAutomator test) may be connected", t)`
- `finish(2, Bundle().apply { putString("mobaxd", "fatal: …") })`

확인 방법: Step 4의 데몬이 떠 있는 상태에서 같은 명령으로 두 번째 instrumentation을 띄운다(`-e port 9010`). 같은 패키지라 AMS가 첫 번째를 정리할 수 있으니, 결과를 그대로 기록한다. 그다음 `uiautomator dump`를 데몬이 떠 있는 동안 실행해 데몬 로그와 `uiautomator` 출력을 확인한다.

Expected: `mobax.log`에 `fatal: UiAutomation unavailable` 줄이 있고, 데몬이 멈춘 채 남아 있지 않다.

- [ ] **Step 6: Commit**

```bash
git add build.gradle.kts AndroidManifest.xml proguard-rules.pro src/dev/mobileax/daemon/
git commit -m "daemon: run as an instrumentation (am instrument), UiAutomation from the framework"
```

---

### Task 3: shell 명령 실행 도우미 (`Shell`)와 `Runtime.exec` 교체

**Files:**
- Create: `src/dev/mobileax/daemon/Shell.kt`
- Modify:
  - `src/dev/mobileax/daemon/Notifications.kt`(`runDumpsys`)
  - `src/dev/mobileax/daemon/Location.kt`
  - `src/dev/mobileax/daemon/SettingsApi.kt`(exec 2곳)
  - `src/dev/mobileax/daemon/Screenshot.kt`(`findSecureWindow`)
  - `src/dev/mobileax/daemon/ProcStream.kt`(`runProcess`: `dumpsys`/`logcat`/`monitor`/`shell` 스트림)

**Interfaces:**
- Consumes: `ua`(`Daemon.kt`)
- Produces:
  - `internal object Shell`. `UiDevice.executeShellCommand`는 stdout만 주므로 쓰지 않는다. 이 이유를 `Shell.kt` 머리 주석에 남긴다.
  - `fun run(cmd: String, timeoutMs: Long = 10_000): ShellResult`
  - `data class ShellResult(val out: String, val err: String, val exit: Int)`
  - `fun open(cmd: String): ShellProc`. `ShellProc`는 `stdin: ParcelFileDescriptor`, `stdout: ParcelFileDescriptor`, `stderr: ParcelFileDescriptor`를 갖고, `suspend fun exitCode(): Int`, `fun kill()`을 제공한다.
  - 상수 `EXIT_MARK = "__MOBAX_EXIT:"`

- [ ] **Step 1: `Shell` 구현**

- `open`: `ua.executeShellCommandRwe("sh -c '<cmd>; echo $EXIT_MARK$? >&2'")`
  - `<cmd>`의 작은따옴표는 `'\''`로 이스케이프한다.
  - stderr를 읽을 때 마지막 `EXIT_MARK` 줄을 떼어 종료 코드로 쓰고, 나머지는 그대로 흘려보낸다.
- `kill`: 세 PFD를 닫는다. 남은 자식 프로세스는 shell 쪽에서 SIGPIPE로 끝난다. 그래도 남는 스트림(`logcat -f` 등)이 있으면 Step 3에서 `pkill -P`를 검토한다.
- `run`: `open` 위에서 stdout과 stderr를 끝까지 읽는다. `timeoutMs`가 지나면 `kill()`하고 `exit = -1`로 돌려준다.

- [ ] **Step 2: 호출 지점 교체**

다섯 파일의 `Runtime.getRuntime().exec(arrayOf(...))`와 `ProcessBuilder`를 `Shell.run(...)`이나 `Shell.open(...)`으로 바꾼다. 인자 배열은 공백으로 이어 붙이되, 각 인자는 Step 1과 같은 규칙으로 작은따옴표로 감싼다.

`ProcStream.runProcess`의 기존 동작은 그대로 지킨다.
- `exitTrailer`: 스트림 끝에 종료 코드 trailer를 보낸다.
- `spawnErr`: 프로세스를 못 띄우면 오류를 보낸다.

- [ ] **Step 3: 기기 확인**

Task 2 Step 4와 같은 방법으로 설치·기동한 뒤 다음을 실행한다.
- `mobax notif`
- `mobax location`
- `mobax settings global airplane_mode_on`
- `mobax settings put global mobax_test 1`, 이어서 `settings get`으로 확인한 뒤 `settings delete`로 정리한다.
- `mobax logs --tail 5`
- `mobax logs --follow`를 시작하고 2초 뒤 Ctrl-C로 끊는다.
- `mobax shell` REPL에서 `ls /sdcard`, `false`, `exit`

Expected:
- 각 출력이 Task 2 이전(main 브랜치 빌드)과 같은 형식이다.
- `false`의 종료 코드가 1로 보고된다.
- `logs --follow`를 끊은 뒤 `adb shell ps -A | grep logcat`에 남은 프로세스가 없다.

- [ ] **Step 4: Commit**

```bash
git add src/dev/mobileax/daemon/
git commit -m "daemon: run shell-uid commands through UiAutomation.executeShellCommandRwe"
```

---

### Task 4: 파일 전송(`cp`)을 shell 경유로

**Files:**
- Modify: `src/dev/mobileax/daemon/Transfers.kt`(`pull`, `push`)

**Interfaces:**
- Consumes: `Shell.open`, `Conn.copyChunks`, `Conn.drainChunks`, `errAndEnd`
- Produces: 와이어 변경 없음. 실패는 기존 `ERR:` 프레임으로 보낸다.

- [ ] **Step 1: `pull` 교체**

`File(path)`를 직접 읽는 대신 다음을 한다.
1. `Shell.open("cat <path>")`로 열고 stdout을 청크로 보낸다.
2. 끝나면 종료 코드를 확인한다.
3. 0이 아니면 stderr 첫 줄을 담아 `ERR:` 프레임으로 끝낸다.

크기를 미리 알려 주는 기존 동작이 있으면 `stat -c %s <path>`로 먼저 받는다. 이때 실패도 같은 오류로 처리한다.

- [ ] **Step 2: `push` 교체**

1. `Shell.open("mkdir -p <parent> && cat > <path> && chmod <mode> <path>")`의 stdin에 청크를 쓰고 닫는다.
2. 종료 코드를 확인한다.
3. `stat -c %s`로 받은 크기를 `expectedSize`와 비교한다.
4. 다르면 `ERR:size mismatch` 계열의 기존 오류 문구로 보낸다.

- [ ] **Step 3: 기기 확인 (정상 경로)**

- `mobax cp device:/system/etc/hosts h`
- `mobax cp up.txt device:/data/local/tmp/up.txt`, 이어서 `adb shell cat`으로 내용을 확인한다.
- 1MB 이상 파일을 왕복시킨다. `dd if=/dev/urandom of=big bs=1M count=8`로 만들고 받은 뒤 해시를 비교한다.

Expected: 해시가 같다. 8MB 왕복 시간을 기록하고 main 빌드와 비교한다. 2배를 넘으면 사용자에게 보고한다.

- [ ] **Step 4: 기기 확인 (오류 경로, Review Focus 3)**

- `mobax cp device:/system/build.prop x`
- `mobax cp up.txt device:/system/up.txt`

Expected: 둘 다 0이 아닌 종료 코드와 `Permission denied` 또는 `Read-only file system`이 담긴 오류. 로컬에 빈 파일 `x`가 남지 않는다. 남으면 CLI의 pull 경로에서 실패 시 파일을 지운다.

- [ ] **Step 5: Commit**

```bash
git add src/dev/mobileax/daemon/Transfers.kt
git commit -m "daemon: cp through a shell-uid cat; unreadable paths fail instead of 0 bytes"
```

---

### Task 5: shell uid에 기대던 binder 경로 정리

Task 1 확인 4와 클립보드 결과에 따라 진행한다. 결과가 "adopt 아래에서 그대로 됨"이면 해당 파일은 손대지 않고 확인만 한다.

**Files:**
- Modify (필요한 것만):
  - `src/dev/mobileax/daemon/Clipboard.kt`
  - `src/dev/mobileax/daemon/Providers.kt`
  - `src/dev/mobileax/daemon/SettingsDirect.kt`
  - `src/dev/mobileax/daemon/Props.kt`(`setprop`)
  - `src/dev/mobileax/daemon/Am.kt`
  - `src/dev/mobileax/daemon/Installer.kt`
  - `src/dev/mobileax/daemon/State.kt`
- Modify: `src/dev/mobileax/daemon/Pm.kt`. `list()`가 `ctx.packageName`(`dev.mobileax.daemon`)을 건너뛴다.

**Interfaces:**
- Consumes: `Shell.run`, `ctx`(`Daemon.kt`)
- Produces: 와이어 변경 없음

- [ ] **Step 1: `Pm.list`에서 자기 패키지 숨기기**

`for (p in pkgs)` 루프 첫머리에 `if (p.packageName == ctx.packageName) continue`를 넣는다.

- [ ] **Step 2: 스파이크에서 실패한 항목만 shell 경유로 바꾸기**

- `Props.set`은 `SystemProperties.set`이 앱 uid에서 SELinux로 막히므로, 스파이크 결과와 무관하게 `Shell.run("setprop <k> <v>")`로 바꾼다.
- 클립보드 등 나머지는 스파이크에서 정한 shell 측 수단으로 바꾼다.
- 각 파일 머리 주석의 "shell UID"를 전제로 한 설명(예: `Clipboard.kt`)을 현재 경로에 맞게 고친다.

- [ ] **Step 3: 기기 확인**

| 명령 | 확인 방법 |
|---|---|
| `mobax clip` 읽기·쓰기 | `mobax shell`의 클립보드 쓰기 verb를 쓰거나 `cmd`로 넣은 뒤 읽는다 |
| `sms`, `calls`, `contacts`, `calendar` | 실행한다 |
| `prop` 읽기와 `prop debug.mobax.test 1` 쓰기 | 쓴 뒤 `getprop`으로 확인한다 |
| `open`, `close` | 실행한다 |
| `apps --3rd` | `dev.mobileax.daemon`이 목록에 없어야 한다 |
| `install`과 `uninstall` | `probe/sample/build/outputs/apk/debug/sample-debug.apk`로 한다 |
| `info` | 실행한다 |
| `links com.android.settings` | 실행한다 |

Expected: 출력 형식이 main 빌드와 같다.

- [ ] **Step 4: Commit**

```bash
git add src/dev/mobileax/daemon/
git commit -m "daemon: shell-only binder paths for an app-uid daemon; hide own package"
```

---

### Task 5b: androidx.test로 바꿀 수 있는 경로 바꾸기

**Files:**
- Modify (후보별): `src/dev/mobileax/daemon/Wm.kt`, `src/dev/mobileax/daemon/Gestures.kt`, `src/dev/mobileax/daemon/Server.kt`

**Interfaces:**
- Consumes: `device`, `ua`, `instr`(`Daemon.kt`)
- Produces: 와이어 변경 없음

각 후보를 바꾼 뒤 같은 명령을 main 빌드와 비교한다. 결과가 같고 지연이 ±10% 안이면 바꾼 것을 유지한다. 아니면 되돌리고, 코드에 이유를 한 줄 남긴다.

| 후보 | 지금 | androidx.test | 측정 |
|---|---|---|---|
| `wm_rotation` | `IWindowManager.freezeRotation` 리플렉션(`Wm.setRotation`) | `UiDevice.freezeRotation()`과 `setOrientation*`. 0~3 회전값을 다 표현할 수 있는지 먼저 확인한다 | 회전 후 `wm_info`의 rotation |
| `wait_for_idle` | `Gestures.waitForIdle` | `UiDevice.waitForIdle(timeout)` | 같은 화면에서 10회 지연 |
| `wait_for_text` | `Gestures.waitForText` | `device.wait(Until.hasObject(By.text(...)), timeout)`. 현재 매칭 규칙과 같은 `By` 조합이 있는지 먼저 확인한다 | 나타나는 텍스트·없는 텍스트 각각의 지연과 결과 |
| `wait_for_activity` | 현재 구현 | `device.currentPackageName`으로 대신할 수 있는 부분만 | 결과 비교 |
| `info`의 화면 크기 | `h.shot.sourceSize()` | `device.displayWidth`/`displayHeight` | 회전 상태별 값 비교 |
| 키 이름 → 키 코드 주입 | `Input.keyByName`(`wait=0`) | `device.pressKeyCode` | **유지 예상.** 동기 주입이라 애니메이션을 기다린다. Recents 키 지연으로 확인만 한다 |
| tap, swipe, 드래그 | `Input` 3-arg 주입 | `device.click`/`swipe`/`drag` | **유지 예상.** 같은 이유. 드래그 지연으로 확인만 한다 |
| dump | `Dumper` JSON | `device.dumpWindowHierarchy` | **유지.** XML이라 호스트 파서와 맞지 않는다. 측정하지 않는다 |

- [ ] **Step 1: 후보별로 바꾸고 측정**

위 표의 순서대로 한다. 후보마다 결과(바꿈 또는 유지, 측정값)를 이 계획 문서 끝 "androidx.test 대체 결과" 절에 한 줄로 적는다.

- [ ] **Step 2: 기기 확인**

바꾼 후보의 verb와 Task 2 Step 4의 세 명령을 다시 실행한다.
Expected: main 빌드와 같은 결과.

- [ ] **Step 3: Commit**

```bash
git add src/dev/mobileax/daemon/ docs/superpowers/plans/2026-11-03-daemon-instrumentation.md
git commit -m "daemon: use UiDevice where it matches the tuned paths"
```

---

### Task 6: 설치 지문과 서명 불일치 판정 (CLI, 순수 함수)

**Files:**
- Create: `mobax-cli/src/device/install.rs`
- Modify: `mobax-cli/src/device/mod.rs`(모듈 등록)

**Interfaces:**
- Produces:
  - `pub(crate) fn fingerprint(bytes: &[u8]) -> String`. FNV-1a 64비트를 소문자 16진 16자리로 돌려준다.
  - `pub(crate) enum InstallOutcome { Ok, SignatureMismatch, Failed(String) }`
  - `pub(crate) fn parse_pm_install(stdout: &str) -> InstallOutcome`
  - `pub(crate) const DEVICE_APK: &str = "/data/local/tmp/mobaxd.apk"`
  - `pub(crate) const DEVICE_FP: &str = "/data/local/tmp/mobaxd.fp"`
  - `pub(crate) const PKG: &str = "dev.mobileax.daemon"`
  - `pub(crate) fn instrument_cmd(port: u16, log: &str) -> String`

- [ ] **Step 1: 실패하는 테스트 작성** (`install.rs` 안 `#[cfg(test)] mod tests`)

```rust
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
fn instrumentation_is_started_detached_with_the_port() {
    let c = super::instrument_cmd(9009, "/data/local/tmp/mobax.log");
    assert!(c.starts_with("nohup am instrument -w --no-hidden-api-checks -e port 9009 dev.mobileax.daemon/.MobaxInstrumentation"));
    assert!(c.ends_with("> /data/local/tmp/mobax.log 2>&1 &"));
}
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p mobax-cli install::`
Expected: FAIL. 모듈이나 함수가 아직 없어서다.

- [ ] **Step 3: 구현**

- `fingerprint`: 오프셋 `0xcbf29ce484222325`, 소수 `0x100000001b3`를 쓰고 `format!("{:016x}")`로 낸다.
- `parse_pm_install`:
  - `Success`가 있으면 `Ok`
  - `INSTALL_FAILED_UPDATE_INCOMPATIBLE`이 있으면 `SignatureMismatch`
  - 그 외는 `Failed(trimmed stdout)`

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p mobax-cli install::`
Expected: PASS (3 tests)

- [ ] **Step 5: Commit**

```bash
git add mobax-cli/src/device/install.rs mobax-cli/src/device/mod.rs
git commit -m "cli: daemon APK fingerprint and pm install outcome"
```

---

### Task 7: CLI 기동·정리를 설치 + instrument로

**Files:**
- Modify: `mobax-cli/src/device/daemon.rs`
  - `connect`의 빠른 경로와 느린 경로
  - `disconnect`
  - `devices`
  - `DEVICE_JAR` 제거
  - 모듈 머리 주석
- Modify: `mobax-cli/src/cli/args.rs`
  - `--keep-apk`를 추가하고 `--keep-jar`는 별칭으로 둔다.
  - `DisconnectOpts.keep_jar`를 `keep_apk`로 바꾼다.
- Modify: `mobax-cli/src/cli/usage.rs`(drop 줄)
- Modify: `devices` 출력에서 `jar_present`를 쓰는 곳(열 이름 `jar` → `apk`). `DeviceRow.jar_present`를 `apk_installed`로 바꾼다.

**Interfaces:**
- Consumes: Task 6의 `fingerprint`, `parse_pm_install`, `InstallOutcome`, `DEVICE_APK`, `DEVICE_FP`, `PKG`, `instrument_cmd`
- Produces: `pub(crate) fn connect(opts) -> io::Result<(String, u16)>`. 시그니처는 그대로다.

- [ ] **Step 1: 포트 충돌 판정 테스트**

`daemon.rs` tests에 추가한다.

```rust
#[test]
fn a_second_port_on_a_busy_device_is_refused() {
    let e = super::refuse_other_port("emulator-5554", 9009, Some(9008)).unwrap_err();
    let m = e.to_string();
    assert!(m.contains("9008") && m.contains("mobax drop"));
    assert!(super::refuse_other_port("emulator-5554", 9008, Some(9008)).is_ok());
    assert!(super::refuse_other_port("emulator-5554", 9009, None).is_ok());
}
```

Run: `cargo test -p mobax-cli a_second_port` → FAIL

- [ ] **Step 2: `refuse_other_port` 구현**

시그니처는 `fn refuse_other_port(serial: &str, port: u16, running: Option<u16>) -> io::Result<()>`이다.

오류 문구:
```
daemon already running on {serial} at port {running}; one per device — run `mobax drop` first
```

`running`은 `adb shell "cat /proc/$(pidof dev.mobileax.daemon)/cmdline"`이 아니라 기기의 `DEVICE_FP` 옆 파일 `/data/local/tmp/mobaxd.port`(기동 시 CLI가 씀)에서 읽는다.

Run: `cargo test -p mobax-cli a_second_port` → PASS

- [ ] **Step 3: `connect` 느린 경로 교체**

1. `refuse_other_port`를 확인한다. `pidof dev.mobileax.daemon`이 있을 때만 port 파일을 읽는다.
2. 이전 데몬을 정리한다. `am force-stop dev.mobileax.daemon`을 부르고, `pkill -f "am instrument.*dev.mobileax.daemon"`으로 남은 `am` 프로세스도 끝낸다. `pidof`가 비워질 때까지 최대 3초 기다린다.
3. 설치 여부를 판정한다. 로컬 APK의 `fingerprint`와 기기의 `cat DEVICE_FP`를 비교하고, `pm path PKG`가 비어 있지 않은지 본다. 둘 다 맞으면 설치를 건너뛴다.
4. 설치가 필요하면:
   - `push` → `DEVICE_APK`
   - `pm install -r -t DEVICE_APK`
   - 결과가 `SignatureMismatch`면 `pm uninstall PKG` 후 한 번 더 설치한다.
   - `Failed(m)`이면 `m`을 담아 오류로 끝낸다.
   - 성공하면 `echo <fp> > DEVICE_FP`
5. `echo <port> > /data/local/tmp/mobaxd.port`를 쓰고 `instrument_cmd(port, DEVICE_LOG)`로 기동한다.
6. ping 대기는 지금처럼 6초다. 실패하면 `DEVICE_LOG`를 보여 준다.

- [ ] **Step 4: `connect` 빠른 경로 보강**

ping이 성공해도 기기의 `DEVICE_FP`가 로컬 `fingerprint`와 다르면 느린 경로로 간다. "jar가 바뀌어도 옛 데몬을 씀" 문제를 막기 위해서다.
- 기기 쪽 읽기는 `adb_server::shell`(subprocess 없는 경로) 한 번이다.
- 로컬 지문은 APK 파일 크기와 수정 시각을 키로 `~/.mobileax/` 아래에 캐시해서, 매번 파일 전체를 읽지 않는다.

- [ ] **Step 5: `disconnect`와 `devices`**

- `disconnect`:
  1. `am force-stop PKG`
  2. 남은 `am instrument` 프로세스를 `pkill`
  3. `keep_apk`가 아니면 `pm uninstall PKG`, 그리고 `rm -f DEVICE_APK DEVICE_FP /data/local/tmp/mobaxd.port DEVICE_LOG`
- `devices`의 셸 프로브:
  - `test -f {DEVICE_JAR}` 대신 `pm path dev.mobileax.daemon >/dev/null && echo apk=1 || echo apk=0`
  - `pgrep -f mobaxd` 대신 `pidof dev.mobileax.daemon`

- [ ] **Step 6: 빌드와 테스트**

Run: `cargo build --release -q && cargo test -q && cargo clippy -q --all-targets -- -D warnings`
Expected: 모두 통과.

- [ ] **Step 7: 기기 확인 (Review Focus 1, 2)**

| 순서 | 명령 | 기대 결과 |
|---|---|---|
| 1 | `mobax drop` | |
| 2 | `mobax use emulator-5554` | `daemon up`. 처음이라 설치가 일어나고, `mobax.log`에 `mobaxd monkey before=false after=true`가 있다. |
| 3 | `mobax use` 한 번 더 | 빠른 경로로 100ms 안쪽에 끝난다. |
| 4 | `./gradlew -q mobaxJar`로 APK 내용을 바꾼다(코멘트 한 줄 변경). 그 뒤 `mobax use` | 재설치 후 새 데몬이 뜬다. pid가 바뀐다. |
| 5 | `adb shell pkill -f "am instrument"`, 이어서 `mobax ui` | "no daemon … start it with `mobax use`" |
| 6 | `mobax use` | 재설치 없이 다시 뜬다(`pm install` 호출 없음). |
| 7 | `mobax use --port 9009` | 포트 충돌 오류 |
| 8 | 서명이 다른 사본을 먼저 설치하고(아래), `mobax use` | `use`가 지우고 다시 설치한 뒤 `daemon up` |
| 9 | `mobax devices` | `apk` 열이 1, 데몬 실행 중으로 표시된다. |
| 10 | `mobax drop`, 이어서 `adb shell pm path dev.mobileax.daemon` | 출력이 비어 있다. |

8번 서명 불일치는 이렇게 만든다. `keytool`로 임시 키를 만들어 `apksigner sign`으로 다시 서명한 사본을 먼저 설치한 뒤 `use`를 실행한다. 임시 키와 사본은 scratchpad에만 두고 커밋하지 않는다.

- [ ] **Step 8: Commit**

```bash
git add mobax-cli/
git commit -m "cli: install the daemon APK and start it with am instrument"
```

---

### Task 8: 전체 검증, 릴리즈 파이프라인, 문서

**Files:**
- Modify: `.github/workflows/release.yml`. 빌드 단계 이름과 주석을 바꾼다. 자산 이름 `mobax.jar`는 유지한다. AGP가 러너에서 debug 키를 자동으로 만들기 때문에 시크릿은 필요 없다. 이 점을 주석으로 남긴다.
- Modify: `docs/architecture.md`, `docs/sharp-edges.md`, `README.md`. `app_process`, `CLASSPATH`, `/data/local/tmp/mobax.jar` 언급을 instrumentation 기준으로 바꾼다. `sharp-edges.md`에는 아래를 추가한다.
  - minSdk 31
  - 기기당 데몬 하나
  - 기기에 패키지가 설치됨(일부 OEM의 "USB로 설치" 설정)
  - monkey로 인한 앱·Settings 동작 변화
  - 다른 UiAutomation과 공존 불가
- Modify: `install.sh`, `scripts/mcpb-stage.mjs`. jar 경로를 쓰는 곳이 있으면 의미만 맞춘다. 이름은 그대로다.

- [ ] **Step 1: 기능 전체 확인 (SDK 36 에뮬레이터)**

spec 2절 목표의 verb 목록을 모두 실행하고, main 브랜치 빌드와 출력 형식이 같은지 본다. 결과는 표로 정리한다.

빠짐없이 포함할 것:
- `see`의 jpeg/h264/tilejpeg 스트림(`mobax see --codec h264`로 viewer를 띄워 10초 확인)
- `tui`
- `watch`
- `events`
- MCP(`mobax mcp`에 `tools/list`와 `ui`, `tap`, `batch` 호출)

- [ ] **Step 2: 성능 기준선 비교**

Global Constraints의 다섯 지표를 main 빌드와 이 브랜치 빌드로 각각 6회 측정해 중앙값을 비교한다.
- 측정에는 `MOBAX_TIMING=1`과 지난번과 같은 방법을 쓴다. 유휴 `ui`는 `mobax dev bench -n 20`이다.

Expected: 잡음 범위(±10%) 안. 넘으면 원인을 적고 사용자에게 보고한다.

- [ ] **Step 3: monkey 해제 확인 (Review Focus 4)**

- `mobax drop`, 이어서 `mobax use`. 새 데몬의 `mobax.log`에서 `monkey before=false`를 확인한다. 앞 데몬이 끝나면서 monkey가 풀렸다는 뜻이다.
- `adb shell am crash dev.mobileax.daemon`(이 명령이 없으면 `adb shell kill -SEGV $(pidof dev.mobileax.daemon)`), 이어서 `mobax use`. 같은 방법으로 확인한다.
- `mobax drop` 직후 `adb shell ps -A`에 남은 프로세스가 없는지 확인한다.

Expected: 두 경우 모두 `before=false`. 남은 `dev.mobileax.daemon`과 `am instrument` 프로세스가 없다.

- [ ] **Step 4: 테스트와 빌드**

Run: `cargo test -q && cargo clippy -q --all-targets -- -D warnings && ./gradlew -q mobaxJar`
Expected: 모두 통과.

- [ ] **Step 5: 문서 수정 후 commit**

```bash
git add .github/workflows/release.yml docs/ README.md install.sh scripts/
git commit -m "docs, release: the daemon is an instrumentation APK"
```

- [ ] **Step 6: 릴리즈 드라이런**

```bash
git push -u origin daemon/instrumentation
gh workflow run release.yml --ref daemon/instrumentation -f tag=v0.6.0-dryrun
```

Expected: 실행 성공, 자산 20개.

- [ ] **Step 7: 사용자에게 보고**

다음을 담아 보고하고 PR을 연다. merge는 사용자가 정한다.
- 1~3단계 결과표
- 성능 비교
- 스파이크 결과
- 남은 위험
