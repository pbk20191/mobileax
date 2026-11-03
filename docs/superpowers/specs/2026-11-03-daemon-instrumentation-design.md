# 데몬을 instrumentation 프로세스로 전체 이동 — 설계

날짜: 2026-11-03 · 대상: `src/dev/mobileax/daemon/` (on-device 데몬), `mobax-cli/src/device/daemon.rs` (기동·정리), 릴리즈 파이프라인

## 1. 의도와 이유

지금 데몬은 `CLASSPATH=/data/local/tmp/mobax.jar app_process … dev.mobileax.daemon.Main`으로 뜬다. 시스템이 모르는 프로세스(AMS `ProcessRecord` 없음)가 shell uid(2000)로 돌면서, 필요한 것을 숨겨진 API로 직접 조립한다.

- `UiAutomationConnection`을 리플렉션으로 생성하고, `UiAutomation` 생성자 시그니처를 SDK별로 찾고, `connect`를 재시도한다.
- `ActivityThread.systemMain()`으로 시스템 컨텍스트를 만들고, shell(`com.android.shell`)의 `ApplicationInfo`를 빌려 Application을 바꿔 끼운다(#23).
- `setRunAsMonkey`는 AMS가 "instrumentation 프로세스이면서 UiAutomation이 있어야 한다"를 요구해서(`ActivityManagerService.setUserIsMonkey`) 한 번도 성공하지 못했다. 그래서 매번 `Unknown process` 경고가 난다.

사용자가 원하는 것은 두 가지다.
1. **monkey 상태를 정식 경로로 켠다.** instrumentation 등록을 통해서다. `setActivityController(imAMonkey=true)` 같은 우회로는 원하지 않는다.
2. **공식 API 위에 선 구조.** 기기에 APK 설치가 필요해지더라도 감수한다. 사용자가 이 둘 중 공식 API 쪽을 명시적으로 골랐다.

사용자 결정: 예비 경로(app_process 방식)는 남기지 않고 **전체 이동**한다. 작업은 별도 브랜치(`daemon/instrumentation`)에서 한다.

## 2. 목표 / 비목표 / 안티목표

**목표**
- 데몬은 설치된 APK의 instrumentation으로 돈다. `am instrument -w --no-hidden-api-checks … dev.mobileax.daemon/<Instrumentation>`으로 띄우고, AMS가 프로세스를 fork한다.
- `UiAutomation`은 `Instrumentation.getUiAutomation()`으로 얻는다. 리플렉션으로 연결 객체를 조립하는 코드는 지운다.
- monkey가 켜진다. `setUserIsMonkey(true)`가 성공하고, 기기 전체에서 `ActivityManager.isUserAMonkey()`가 true가 된다.
- 지금의 모든 verb가 같은 결과를 낸다. 대상 verb는 `ui`, `see`, `tap`, `type`, `fill`, `swipe`, `go`, `open`, `close`, `install`, `uninstall`, `cp`, `apps`, `links`, `sms`, `calls`, `contacts`, `calendar`, `notif`, `clip`, `location`, `prop`, `settings`, `logs`, `events`, `watch`, `info`, `show`, `mirror`/`see` 스트림(jpeg, h264, tilejpeg), `shell` 경유 명령, MCP 도구다.
- 와이어 프로토콜과 호스트 쪽(CLI, MCP, viewer, TUI)은 바뀌지 않는다. 바뀌는 건 데몬 기동·정리 경로뿐이다.

**비목표**
- 대상 앱 프로세스 안으로 들어가는 instrumentation(`targetPackage`=다른 앱). 서명이 같아야 해서 범위 밖이다.
- 한 기기에서 데몬 여러 개를 띄우는 것. UiAutomation이 기기당 하나뿐이고, 같은 패키지의 instrumentation은 동시에 하나만 돈다. `--port`는 소켓 이름(`mobaxd_<port>`)으로만 남는다.
- Android 9~11(SDK 28~30) 지원. 아래 제약 참고.
- 이번 작업 안에서 성능 최적화. 이동만 한다.

**안티목표** (기술적으로 돌아가도 이러면 실패)
- 인식 경로가 눈에 띄게 느려진다. 기준선과 비교해 잡음 범위를 넘으면 실패다.
  - 유휴 `ui` 약 265ms
  - tap→하위 화면 중앙값 약 976ms
  - Recents 키 응답 약 627ms
  - 드래그 약 126ms
  - 첫 `dump_active` 약 158ms
- 일반적인 사용에서 기기 화면을 사람이 눌러야 한다. OEM 설정의 "USB로 설치 허용" 같은 일회성 설정은 예외로 문서화한다.
- `mobax drop` 후에도 기기가 monkey 상태로 남는다.
- 데몬이 죽었는데 CLI는 "daemon up"이라고 한다.

## 3. 제약

- **minSdk 31** (사용자 결정). `adoptShellPermissionIdentity()`(29+)와 `executeShellCommandRw`/`Rwe`(31+, `cp` 업로드의 표준 입력과 `shell`의 표준 에러·종료 코드에 필요)가 모두 공개 API로 있는 버전이다. 29~30용 갈래 코드는 쓰지 않는다. 검증할 기기가 SDK 36 에뮬레이터뿐이라, 그 갈래는 검증 없이 배포될 것이기 때문이다.
- **UiAutomation은 기기당 하나만 연결된다.** 사용자의 UiAutomator 테스트와 동시에 쓸 수 없다. 이건 지금도 같다.
- **`am instrument -w` 프로세스가 살아 있어야 한다.** `UiAutomationConnection`이 이 shell 프로세스 안에 있어서, 이게 죽으면 UiAutomation도 죽는다. `nohup … &`로 분리해 띄운다. 지금의 `app_process` 기동 방식과 같다.
- **데몬은 앱 uid로 돈다.**
  - `/data/local/tmp`를 읽고 쓸 수 없다(SELinux `shell_data_file`). 기기 로그 파일 `/data/local/tmp/mobax.log`도 데몬이 직접 쓸 수 없다.
  - `Runtime.exec`로 띄운 자식 프로세스도 앱 uid라, `dumpsys` 같은 shell 권한 명령이 실패하거나 결과가 달라진다.
  - shell 권한은 `adoptShellPermissionIdentity()`와 `executeShellCommand()`(shell uid로 실행되고 결과가 파이프로 옴)로 얻는다.
- **abstract 소켓 도달성.** CLI는 adb server를 통해 `localabstract:mobaxd_<port>`에 붙는다. adbd(SELinux `adbd` 도메인)가 앱 도메인이 연 abstract 소켓에 `connectto` 할 수 있어야 한다. Chrome devtools(`localabstract:chrome_devtools_remote`)가 이 방식이라 될 것으로 보지만, **가장 먼저 검증할 위험**이다.
- **숨겨진 API.** `--no-hidden-api-checks`로 instrumentation 프로세스의 검사를 끈다. 남는 숨겨진 API 호출(예: `IPackageManager` 직접 호출)은 이 플래그에 기댄다.
- **monkey 해제.** 제 기억으로는 instrumentation이 끝나면 AMS가 `mUserIsMonkey`를 되돌린다. **미확인이라 구현 중에 AMS 소스나 실기기로 확인한다.** 되돌리지 않으면 `drop`이 직접 `setUserIsMonkey(false)`를 불러야 한다.

## 4. 사용자가 정한 방식

- 데몬 전체를 instrumentation 프로세스로 옮긴다.
- 설치가 필요해도 공식 API 쪽으로 간다.
- monkey는 instrumentation 등록을 통해 켠다.
- app_process 방식은 예비로 남기지 않는다.
- 브랜치를 따서 진행한다.
- minSdk는 31로 올린다.
- **7절의 먼저 할 확인 1번(소켓 도달)이나 2번(monkey 켜기·끄기)이 실패하면 프로젝트를 접는다.** TCP와 `adb forward`, `setActivityController` 같은 대체 방식으로 가지 않는다. 브랜치는 버리고, 무엇이 왜 실패했는지 기록만 남긴다.

## 5. 구성 요소별 이동

현재 `src/dev/mobileax/daemon/` 기준이다.

| 영역 | 지금 | 이동 후 |
|---|---|---|
| 진입점 `Main.kt` | `main()`: hidden-api 해제, looper 준비, `systemMain()`, Application 교체, UiAutomation 리플렉션 연결 | `Instrumentation` 서브클래스. `onCreate`에서 `start()`, `onStart`에서 서버를 켜고 `getUiAutomation()`. `setRunAsMonkey(true)` 또는 `setUserIsMonkey(true)`를 부른다. `initializeMainlineModules`, `setShouldDefuse`, slow-log 같은 #23 초기화 중 프로세스가 이미 해 주는 것(ActivityThread.main 경로)은 지운다. |
| `Binders.kt` | 시스템 컨텍스트, shell Application, `ServiceManager` 리플렉션 | 컨텍스트는 `Instrumentation.getContext()`(자기 패키지) 하나만 쓴다. binder 직접 호출은 남는 곳만 유지한다. |
| `Loop.kt` | main looper 위의 코루틴 | instrumentation은 `onStart`가 main이 아닌 스레드에서 돈다. 어느 looper를 쓸지는 빌더 판단(8절)이다. |
| `Input`, `Gestures`, `Dumper`, `Waits`, `UiEvents`, `NodeActions`, `Traverse`, `UiWatch` | 우리가 만든 UiAutomation | 같은 코드, `getUiAutomation()`이 준 객체. 3-arg `injectInputEvent` 리플렉션은 유지하고 검증한다. |
| `Screenshot`, `Streams`, `TileStreamer`, `Frames` | 미러 가상 디스플레이(shell 신원 wrapper) | 자기 패키지 컨텍스트로 만든다. `CAPTURE_VIDEO_OUTPUT` 같은 권한은 `adoptShellPermissionIdentity`로 얻는다. |
| `Installer`, `Pm`, `Manifest` | shell uid로 PackageInstaller/IPackageManager | `adoptShellPermissionIdentity` 아래에서 같은 호출. 설치할 APK 파일 접근은 아래 파일 작업 참고. |
| `Transfers` (`cp`), `ProcStream`, `Logx`(파일) | `/data/local/tmp` 직접 읽기·쓰기, `Runtime.exec` | `executeShellCommand`(shell uid의 `cat`, `sh -c`)로 파이프를 통해 바이트를 옮긴다. 다른 방식도 빌더 판단(8절)이다. |
| `Notifications`, `Location`, `SettingsApi`, `Screenshot.findSecureWindow` | `Runtime.exec("dumpsys"/"settings"/…)` | `executeShellCommand`로 바꾼다. |
| `Providers`, `SettingsDirect`, `Clipboard`, `Props`, `Am`, `Wm`, `State` | shell uid binder 호출 | `adoptShellPermissionIdentity` 아래에서 유지한다. 앱 uid라 동작이 달라지는 곳(백그라운드 클립보드 읽기, `SystemProperties.set` 등)은 `executeShellCommand`로 돌린다. |
| `MobaxServer`, `Conn`, `Dispatch`, `Server` | abstract 소켓 서버 | 그대로. 소켓 이름도 그대로. |
| `AndroidManifest.xml` | AGP를 위한 빈 `<application>` | `<instrumentation android:name=… android:targetPackage="dev.mobileax.daemon">`. 권한 선언은 최소로 하고, shell 권한은 adopt로 얻는다. |
| 빌드 | 릴리즈 APK를 `build/mobax.jar`로 복사 (서명 없음) | 같은 APK를 설치용 산출물로 쓴다. 서명은 8절. |

## 6. 기동·정리 (`mobax-cli/src/device/daemon.rs`)

**`mobax use`**
- 빠른 경로는 유지한다. `ping`에 응답하면 그대로 쓴다.
- 느린 경로:
  1. 이전 데몬을 정리한다(`am force-stop dev.mobileax.daemon`, 남은 `am instrument` 프로세스).
  2. 설치된 버전과 CLI가 가진 APK를 비교한다. 다르면 `/data/local/tmp`에 push하고 `pm install -r -t`로 설치한다(shell이 하므로 가능).
  3. `nohup am instrument -w --no-hidden-api-checks -e port N dev.mobileax.daemon/<Instrumentation> > /data/local/tmp/mobax.log 2>&1 &`로 띄운다.
  4. ping을 기다린다.
- 버전 비교는 "jar가 바뀌어도 옛 데몬을 그대로 씀" 문제도 함께 해결해야 한다. 버전 문자열이 같아도 APK 내용이 다르면 다시 설치한다.

**`mobax drop`**
- instrumentation을 끝내고(`am force-stop`), monkey가 꺼진 것까지 확인한다.

**로그**
- 데몬은 `/data/local/tmp/mobax.log`를 직접 쓸 수 없다.
- `am instrument -w`의 표준 출력은 shell이 그 파일로 받으므로, 부팅 줄(`mobaxd listening …`, `ready` 등)을 `Instrumentation.sendStatus`로 내보내면 그 파일에 남는다. 지금 CLI가 그 파일을 읽어 기동 실패를 보여 주는 동작을 유지하는 게 목적이다. 방식은 빌더 판단이다.

## 7. 검증

**먼저 할 것 (위험 순, 각각 짧은 스파이크로 확인).** 1번이나 2번이 실패하면 여기서 멈춘다(4절).
1. 앱 uid 프로세스가 연 `localabstract` 소켓에 adb server 경유로 붙을 수 있는가. 에뮬레이터 SDK 36 기준.
2. `am instrument -w`로 띄운 프로세스에서 `setUserIsMonkey(true)`가 성공하고 `isUserAMonkey()`가 true인가. force-stop 후에는 false로 돌아오는가.
3. `getUiAutomation()` 객체로 3-arg `injectInputEvent`(애니메이션 대기 없음)가 동작하는가.
4. `adoptShellPermissionIdentity` 아래에서 미러 가상 디스플레이, `pm install`, settings 쓰기, SMS 프로바이더 읽기가 되는가.

**완료 판정**
- 2절 목표의 verb 전체를 에뮬레이터(SDK 36)에서 다시 돌려 이전과 같은 결과가 나온다. #23 때 돌린 스모크 목록과 이번 검증을 위해 추가한 verb가 대상이다.
- 2절 안티목표의 지연 기준선과 비교해 회귀가 없다. 같은 에뮬레이터, 같은 방법으로 측정한다.
- `mobax drop` 후 monkey가 false다. 데몬 프로세스와 `am` 프로세스가 남지 않는다.
- Rust 쪽 `cargo test`, `cargo clippy`, Gradle 빌드가 통과한다.
- 릴리즈 드라이런(`gh workflow run release.yml --ref <branch> -f tag=vX-dryrun`)이 통과하고 자산 구성이 맞다.

## 8. 정해 둔 세부 사항 (따로 표시한 것 외에는 내 판단)

- **서명 (사용자 결정).** 릴리즈 APK도 AGP debug 키(`signingConfigs.debug`)로 서명한다. 서명은 설치를 통과하는 용도일 뿐 신뢰의 근거가 아니다. 키 파일을 저장소에 두지 않고, CI 시크릿도 쓰지 않는다.
  - 그 결과 빌드한 곳(각자의 PC, CI 러너)마다 서명이 다르다. 기기의 서명이 다르면(`INSTALL_FAILED_UPDATE_INCOMPATIBLE`) `use`가 자동으로 지우고 다시 설치한다. 데몬 패키지에는 보존할 데이터가 없다.
- **monkey와 "같은 결과" 목표가 부딪칠 때.** monkey는 사용자가 원한 의도된 변화라 monkey가 우선한다. monkey 때문에 앱이나 Settings 동작이 달라지는 경우는 실패로 보지 않고 `docs/sharp-edges.md`에 적는다. 2절의 "같은 결과"는 데몬 이동 때문에 생기는 차이에만 적용한다.
- **`drop`과 패키지.** 지금의 jar 처리와 같은 의미로 둔다. `drop`은 패키지를 지우고, 지금 `keep_jar`가 하던 일은 "패키지 유지"가 된다.
  - `apps`, `devices`, MCP 출력에서는 데몬 자신의 패키지(`dev.mobileax.daemon`)를 숨긴다.
  - `devices`의 `jar_present`는 "설치 여부"로 바꾼다.
- **다른 `--port`로 `use`할 때.** 데몬이 이미 포트 N에서 돌고 있는데 포트 M으로 `use`하면 오류로 멈춘다. 다른 클라이언트가 쓰는 데몬을 몰래 끊지 않기 위해서다. 오류 메시지에는 `mobax drop` 후 다시 하라는 안내를 넣는다.

## 9. 빌더에게 맡기는 것

- `Instrumentation` 안에서 서버와 코루틴 루프를 어느 looper·스레드에 둘지. 현재 `Loop.kt`의 main looper 전제를 어떻게 옮길지.
- 파일 작업(`cp`, 설치할 APK, 프로세스 스트리밍)을 `executeShellCommand` 파이프로 할지 다른 방식으로 할지. 단, 2절 안티목표의 성능 기준은 지켜야 한다.
- 설치 버전 비교 방법(versionCode, APK 해시 등).
- 부팅 로그를 호스트로 전달하는 방법(6절).
- 릴리즈 자산 이름을 `mobax.jar`로 유지할지 `mobaxd.apk`로 바꿀지. 바꾸면 `release.yml`, `scripts/mcpb-stage.mjs`, CLI의 `locate_jar`, 설치 스크립트를 함께 맞춘다.
- 문서 갱신 범위(`docs/architecture.md`, `docs/sharp-edges.md`, `README.md`의 app_process 언급).
