# iPhone·iOS 시뮬레이터 제어 스터디 (go-ios 실측, idevice 이식)

날짜: 2026-11-06 · 브랜치: `ios/idevice-spike` (origin/main `4500069`에서) · 기기: iPhone 11 (`iPhone12,1`), iOS 18.5, 호스트 macOS

## 질문

1. Windows 호스트에서도 iPhone을 XCUITest나 접근성 기반으로 제어할 수 있는가?
2. 서명 없이 어디까지 되는가? 서명이 필요하면 무엇이 필요한가?
3. Rust로 가져올 수 있는가? mobax CLI는 의존성 없는 std라서 따로 둬야 한다.

## 방법

- go-ios(`danielpaulus/go-ios` 6910f3b)를 소스로 빌드해 실기기에서 직접 돌렸다.
- 접근성 서비스는 go-ios CLI의 `ax`가 요소 3개만 찍는 데모라서, 라이브러리로 순회하는 작은 Go 프로그램을 따로 만들었다.
- 그 결과를 바탕으로 idevice(`jkcoxson/idevice` 0.1.68) 위에 `mobax-ios` 크레이트를 만들었다. 접근성 클라이언트는 go-ios `ios/accessibility`를 옮긴 것이다.

## 결과 (go-ios, 실기기)

| 항목 | 결과 |
|---|---|
| 터널 `tunnel start --userspace` | sudo 없이 연결됐다. 커널 터널은 sudo, Windows에서는 관리자 셸과 `wintun.dll`이 필요하다 |
| 스크린샷 | 828×1792 PNG, 약 1초 |
| syslog | 실시간으로 받아지고 `Mafia42` 프로세스 로그도 보인다 |
| 접근성 순회 | 마피아42 채널 목록 15개 요소, 요소당 약 30ms |
| 요소 위치(frame) | 없다. 속성 목록에 위치 항목이 없다 |
| 접근성 Activate | 오류 없이 반환되지만 채널 셀은 반응하지 않았다 |
| HID 좌표 탭 | go-ios와 idevice 모두 iOS 27 이상(`universalhidservice`)만 지원한다. iOS 18에서는 안 된다 |

### 접근성 순회

- 서비스는 `com.apple.accessibility.axAuditDaemon.remoteserver`다. lockdown(usbmuxd)으로 열리고 터널이 필요 없다.
- 설치할 것도 없다. Xcode Accessibility Inspector와 같은 경로다.
- 포커스를 처음 요소, 다음 요소로 옮기면 기기가 `hostInspectorCurrentElementChanged:`로 그 요소를 알려 준다.
- 화면 전체를 한 번에 받는 호출은 없다.
- 요소마다 다음이 나온다.
  - 말하는 텍스트: 레이블과 특성. 예: `로그아웃, 버튼`, `1 채널, 고정 텍스트`
  - 요소 핸들
  - 속성 이름 목록: `Label` `Value` `TraitsHumanReadable` `Identifier` `Hint` `UserInputLabels` `AXAction-2010` `ElementClassName` …
- go-ios의 `NewWithoutEventChangeListeners`로는 포커스 알림이 오지 않았다. `New`처럼 초기화 핸드셰이크를 마쳐야 한다.
  - 핸드셰이크 순서: capabilities 교환 → `deviceCapabilities` → `deviceApiVersion` → 감사 유형 → `deviceInspectorSupportedEventTypes` → `deviceInspectorCanNavWhileMonitoringEvents` → `deviceSetAppMonitoringEnabled:`
- Activate가 안 먹은 이유(추정): 채널 이름은 "고정 텍스트"로 노출되고, 탭을 받는 셀은 접근성 요소로 드러나지 않는다. 진짜 "버튼"으로 노출된 요소(로케일, Test1)는 부작용을 몰라 시험하지 않았다.

### WebDriverAgent

- 이 기기에서는 탭과 글자 입력에 WDA가 필요하다. 트리도 위치와 함께 한 번에 받을 수 있다.
- go-ios의 `ui download wda`는 deviceboxhq.com의 제3자 빌드를 받는다. 이 빌드는 쓰지 않고, Appium 공식 릴리스(`appium/WebDriverAgent` v16.15.3의 `WebDriverAgentRunner-Runner.zip`)를 썼다.
- `scripts/ios-wda-sign.sh`가 그 과정을 담는다: 번들 ID 변경, 프로파일 포함, 키체인 인증서로 서명.
  - 와일드카드 팀 프로파일로 서명과 `codesign --verify --deep --strict`까지 통과했다.
  - 설치와 실행은 기기 연결이 끊겨 확인하지 못했다.
- 2026-10 이후 WDA는 `appium/appium-ios` 모노레포(`packages/webdriveragent`)로 옮겨졌다. 옛 저장소의 마지막 릴리스는 v16.15.3이고, 새 릴리스는 `appium-webdriveragent@X.Y.Z` 태그로 같은 zip들을 단다. 스크립트는 `scripts/ios-wda-tag.sh`로 태그를 골라 모노레포에서 받는다.
  - 시뮬레이터 빌드 16.17.11로 `wda`, `ui`, `tap`, MCP `ui`/`go back`이 그대로 동작했다.
- 무료 Apple ID는 개발자 포털에서 프로파일을 받을 수 없다. Xcode가 빌드하면서 만들어 주는 것이 유일한 길이라 `--xcode` 모드를 두었다.
  - 같은 릴리스의 소스를 받아 `xcodebuild build-for-testing -allowProvisioningUpdates -allowProvisioningDeviceRegistration`으로 빌드한다. 연결된 iPhone(`--udid`)을 Xcode가 등록하고 프로파일을 만든다.
  - 팀 ID는 Apple Development 인증서의 OU에서 읽는다. 팀이 여럿이면 `--team`을 받는다.
  - 번들 ID는 전 세계에서 겹치면 안 되므로 기본값에 팀 ID를 넣는다(`com.mobileax.<team>.WebDriverAgentRunner`).
  - 무료 계정 제약: 프로파일 7일, 기기 3대, 주당 App ID 10개. 만료되면 다시 실행한다.
  - 모노레포 tarball에서 `packages/webdriveragent`만 풀어 쓴다.
  - 서명 없이(`CODE_SIGNING_ALLOWED=NO`) Xcode 27에서 옛 저장소 v16.15.3 소스가 20초에 빌드되고 러너 번들 ID가 `<id>.xctrunner`로 나오는 것까지 확인했다. 프로비저닝은 기기가 없어 확인하지 못했다.
- Windows에서는 `ios ui install wda --p12file … --profile … --path <공식 zip>`으로 서명한다. `.p12`를 한 번 내보내야 한다.

## 목표

에이전트가 Android 에뮬레이터, Android 실기기, iOS 시뮬레이터, iPhone 실기기를 같은 모양의 동사로 다룬다. 핵심은 토큰 초절약 구조다.

- iOS 쪽은 `mobax-ios`가 맡는다.
- 실기기와 시뮬레이터 모두 WDA를 공통 경로로 쓴다.
- 화면 모델은 mobax(Android)와 같은 mobax-core `Screen`이다: `@N` 번호, 동작 후 바뀐 행만 응답.
- appium-mcp는 비교 기준으로만 둔다. iOS 제어 수단(WDA)은 같지만, 화면을 XML 페이지 소스로 다루는 구조라 절약 목표와 맞지 않는다.

## mobax-ios 크레이트 (이 브랜치)

- idevice 0.1.68 위에 있다. 워크스페이스 멤버이지만 `default-members`와 CI(`-p mobax`)에는 들어가지 않는다.
- 대상 선택: `--udid`/`MOBAX_IOS_UDID`는 실기기, `--sim[=UDID|이름]`/`MOBAX_IOS_SIM`은 시뮬레이터다. 둘 다 없으면 연결된 iPhone, 없으면 부팅된 시뮬레이터 하나를 쓴다.
- 동사:

| 동사 | iPhone | 시뮬레이터 |
|---|---|---|
| `list`, `info`, `apps`, `install PATH` | lockdown, installation_proxy | `simctl` |
| `ddi`, `ddi mount [DIR]` | mobile_image_mounter (iOS 17+는 TSS 개인화) | 해당 없음 |
| `see PATH.png` | screenshotr, 실패하면 WDA | `simctl io screenshot` |
| `logs [--grep]` | syslog_relay | `simctl spawn log stream` |
| `wda` | testmanagerd로 러너 실행, 포그라운드 유지 | `simctl launch`, 계속 떠 있음 |
| `ui [--json]`, `tap X Y`/`tap TEXT`/`tap #id`, `type`, `swipe`, `go home` | WDA (usbmuxd로 기기 포트 8100) | WDA (localhost:8100) |
| `launch`, `kill` | WDA `/wda/apps/*` | `simctl` (WDA 불필요) |
| `mcp` | MCP 서버 (아래) | 같음 |
| `ax`, `ax activate` | 접근성 인스펙터 | 해당 없음 |

- `ui` 표는 mobax와 같은 열이다: 동사(`tap`/`fill`/`-`), 요소 유형, 라벨(값이 다르면 ` · "값"`), `#id`, 중심 좌표, `[플래그]`. 좌표는 WDA가 받는 포인트 단위다.
- iOS 17+에서 WDA 러너 실행은 CoreDeviceProxy와 사용자 공간 TCP 스택을 쓴다. 관리자 권한과 커널 터널이 필요 없다(idevice `xctest`).
- WDA HTTP는 usbmuxd로 기기 포트에 바로 붙는다. iproxy가 필요 없다.
- `dtx.rs`: 접근성 인스펙터용 DTX 클라이언트를 직접 구현했다. idevice의 DTX 클라이언트는 아래 두 가지가 crate 내부용이라 쓸 수 없었다.
  - 응답과 기기 쪽 호출을 대화 번호로 구분하기
  - 응답을 기다리는 기기 쪽 호출에 ack 보내기
  - 읽기는 별도 태스크가 하므로 시간 제한으로 기다려도 프레임이 중간에 잘리지 않는다.
  - 이전 문서의 위험 1·2는 이 구현과 단위 테스트로 대체했다(기기 검증은 아직).
- `archive.rs`: NSKeyedArchiver 인코더. `NSMutableDictionary` 래퍼와 `NSNull`을 쓴다.
- 스크립트:
  - `scripts/ios-wda-sign.sh`: 실기기용 Appium WDA 서명. 인증서 이름이 정확히 일치하는지 먼저 확인한다(codesign은 부분 문자열로도 고른다).
  - `scripts/ios-wda-sim.sh`: 시뮬레이터용 Appium WDA 빌드를 받는다. 서명이 필요 없다.
- 단위 테스트 21개: 아카이브 왕복, 포커스 보고 파싱, DTX(조각 결합, 기기 호출 ack와 큐, 오류 응답), XML 소스 읽기, WDA 소스 → 표(흡수, 중복 제거, 스위치 손잡이, 스크롤 막대, 키보드, 숨은 셀 더미, 가림), 요소 고르기, DDI 경로.
- idevice WdaClient 중 쓰지 않는 것: `launch_app`/`terminate_app`/`tap`은 Appium 서버 전용 `mobile:` 명령을 보내서 WDA 단독으로는 404가 난다(탭은 W3C 동작으로 대체돼 스위치에 들쭉날쭉하게 먹혔다). WDA 자체 엔드포인트(`/wda/tap`, `/wda/apps/launch|terminate`)로 직접 보낸다.

## MCP 서버와 절약 구조

`mobax-ios [--sim] mcp`. 도구 이름은 Android `mobax mcp`와 같다: `ui`, `tap`, `type`, `swipe`, `go`, `open`, `close`, `wait`, `watch`, `batch`, `screenshot`, `info`.

- **화면 모델:** mobax-core `Screen`을 그대로 쓴다. 이를 위해 `Screen::from_rows`를 추가했다. Android 동작은 바뀌지 않았다.
- **화면 키:** 앱 이름 + 내비게이션 바 제목 + 알림/시트. 키가 바뀌면 새 화면으로 보고 번호를 1부터 다시 매긴다.
- **좌표만 바뀐 행:** 위에 행이 끼어들거나 목록이 밀려 위치만 바뀐 행은 나열하지 않고 `↕ N rows moved (refs unchanged)` 한 줄로 센다. `@N`은 지금 위치를 가리킨다.
  - mobax-core `change_from`에서 바꿨으므로 Android MCP에도 적용된다.
  - 표 전체를 다시 보내는 기준(바뀐 줄 수)에도 넣지 않는다.
  - 설정 첫 화면으로 돌아왔는데 색인 행이 끼어 있던 경우: 893자(표 전체) → 218자(`+` 1줄, `-` 1줄, `↕ 8 rows moved`).
- **본 화면 기억:** 화면마다 마지막 모습을 기억한다. 이전 화면으로 돌아오면 표 대신 한 줄로 답한다(`back as last seen, 14 rows`). 그사이 바뀐 행이 있으면 그 행만 보낸다.
- **안정 판정:** 동작 뒤 고정 대기 없이 연달아 읽는다(WDA가 앱이 쉴 때까지 기다렸다 답하므로 읽기 자체가 속도를 맞춘다).
  - 동작 전과 달라졌고 두 번 연속 같으면 끝이다.
  - 1.2초 동안 그대로면 "변화 없음", 2초 넘게 계속 바뀌면(스피너, 시계) 그 상태로 답한다.
  - 앱 실행 카드(`card:<번들>…`)가 보이면 그 앱을 WDA로 activate한다(약 60ms). 앱이 WDA 밖에서 열리면 WDA가 실행 카드를 몇 초씩 읽는 일이 있었다.
- **누르기 직전 재확인:** `@N`이나 라벨로 누를 때는 한 번 새로 읽어(0.1~0.2초) 같은 행을 찾고 최신 좌표를 누른다.
  - `@N`은 선택자(`#id` 또는 라벨)로 찾고, 같은 것이 여럿이면 원래 좌표에 가장 가까운 것을 고른다.
  - 사라졌으면 누르지 않고 지금 화면의 변경분을 오류로 답한다.
  - 실제로 `ui`와 `tap` 사이에 설정 첫 화면 위에 색인 행이 끼어들어 목록이 밀렸고, 예전 좌표를 누른 기존 빌드는 다른 항목을 열었다.
- **행 줄이기 (WDA 트리 → 표):**
  - 라벨 없는 컨트롤은 안쪽 첫 라벨을 가져온다.
  - 컨트롤 안의 라벨 없는 컨트롤(아이콘, 화살표, 스위치 손잡이)은 뺀다.
  - 같은 라벨이 같은 자리에 겹치면 하나만 남긴다(입력란·스위치 > 탭 > 텍스트).
  - 컨테이너의 라벨이나 값을 되풀이하는 텍스트는 뺀다.
  - 스크롤 막대(값이 `N%`), 툴바·탭바·내비게이션 바 자체, 홈 화면 상태 막대는 뺀다.
  - 키보드는 행 대신 제목에 `keyboard open`으로만 표시한다. 엔터는 `go enter`.
  - 줄 맞춤 공백은 없고, 라벨은 60자에서 자르며, `\u{a0}` 같은 이스케이프는 쓰지 않는다.
- **스위치 탭:** 행은 라벨까지 걸치지만 토글은 손잡이만 받으므로, 스위치 행의 좌표는 손잡이 중심이다.
- **소스 읽기:** 설정 화면 기준 1.35초 → 0.1~0.15초.
  - `isVisible`을 뺀다: 1.35초 → 0.24초. WDA가 요소마다 가시성을 계산하는 비용이다.
  - JSON 대신 XML로 받는다: 0.22초 → 0.10초. WDA의 JSON은 들여쓰기 포함 281KB, XML은 36KB다. `xml.rs`가 JSON 소스와 같은 모양으로 바꾼다. 7개 화면에서 표가 JSON 판과 같았다.
  - 잃는 숨김 판정은 두 규칙으로 메운다.
    - 같은 종류가 같은 자리에 3개 이상 겹치면 재사용 셀 더미로 보고 뺀다.
    - 트리에서 뒤에 오는(위에 그려지는) 후손 아닌 컨트롤이 앞 컨트롤의 1/3 이상을 덮으면 앞 컨트롤을 뺀다.
  - 남는 위험: 트리에 있지만 숨은 뷰가 아래 행을 지울 수 있다. 관찰된 적은 없다.
    - 겹칠 때만 `isVisible`을 켜고 다시 읽는 방식도 시험했다. 설정 화면은 떠 있는 검색 막대가 늘 마지막 셀과 겹쳐서 전체가 오히려 느려졌다(34~36초).
    - `isVisible`을 켜도 반투명 오버레이 뒤 목록은 "보임"으로 나온다.
- **입력:**
  - 탭: W3C 터치 50ms 누름. `/wda/tap` 0.52초 → 0.4초, 스위치 6/6 토글. 누름 0ms(idevice 대체 경로)는 스위치에 들쭉날쭉했다.
  - 스와이프: W3C 드래그 뒤 마지막 1pt를 50ms에 걸쳐 움직이고 뗀다.
    - 손가락이 속도 0으로 떨어져 관성 스크롤이 없다. 끝에서 멈추기(pause)만으로는 XCTest가 마지막 속도를 유지해 목록이 1.5초 넘게 미끄러졌다.
    - 스크롤 거리가 매번 같다(350pt 드래그 → 311pt).
    - `/wda/dragfromtoforduration` 1.45~2.9초 → 0.7초.
    - 멀리 넘기려면 `fling`.
  - 세션 id를 기억해 매 동작의 `/status` 왕복을 없앴다. WDA가 재시작해 세션이 사라지면 한 번 다시 받는다.
  - WDA 자체 앱 엔드포인트를 쓴다(`/wda/apps/launch|terminate|activate`).
- **시험했지만 넣지 않은 것:** `animationCoolOffTimeout: 0`(WDA가 동작 뒤 애니메이션 종료를 기다리지 않게 함).
  - 동작은 빨라지지만, 화면 전환 도중의 상태가 두 번 연속 똑같이 읽혀 "안정"으로 잘못 판정됐다(옛 화면 셀이 밀린 채 새 화면과 섞임).
  - 동작 동안만 끄거나 0.4~0.7초로 줄이는 방식은 시간 차이가 없었다.

### 측정 (iPhone 18 Pro 시뮬레이터, iOS 27.0)

화면 하나의 크기, XML 페이지 소스(Appium 계열이 보는 것) 대비:

| 화면 | XML 소스 | `ui` 표 |
|---|---|---|
| 설정 첫 화면 | 45,109자 | 797자, 13행 |
| 설정 › 검색 | 53,852자 | 665자, 15행 |
| 홈 화면 | 65,967자 | 813자, 23행 |
| 캘린더 | 27,141자 | 550자, 12행 |

동작 응답(MCP, 설정 시나리오):

| 동작 | 응답 |
|---|---|
| 새 화면으로 이동 | 표 전체, 약 800~1,000자 |
| 이전 화면으로 돌아오기 | 56자 |
| 스위치 토글 | 약 80자 (`~ @4 … [off]`) |
| 같은 화면에서 입력 | 바뀐 행만 |

- 응답 시간: 14단계 시나리오(설정 탐색, 스위치 토글, 스와이프, 검색 입력, 뒤로 가기)를 기존 빌드와 같은 조건에서 번갈아 두 번씩 돌렸다.

| | 1회 | 2회 |
|---|---|---|
| 기존 빌드 | 38.4초 (오류 1) | 38.6초 |
| 새 빌드 | 23.0초 | 22.5초 |

  - 41% 단축이다. 화면 전환 탭은 약 1.3~1.9초다. 내역은 재확인 읽기 0.1초 + WDA 동작 0.7~0.9초 + 안정 확인 읽기 0.13초 × 2~3이다.
  - 스위치 탭은 2.9초 → 2.1초, 스와이프는 4.2초 → 1.2초, 뒤로 가기는 2.4~2.7초 → 1.1~1.2초다.
  - 남은 큰 몫은 WDA 동작 호출 자체다(XCTest가 터치를 합성하고 앱이 쉴 때까지 기다림). 설정 앱은 Spotlight 색인 중에 진행 표시가 돌아 이 대기가 길어진다(검색 화면 진입 4.7~5.1초).
- 도구 정의(tools/list)는 2,840자다.

## 결과 (iOS 시뮬레이터, 2026-11-07)

iPhone 18 Pro 시뮬레이터, iOS 27.0, Appium WDA 16.15.3 시뮬레이터 빌드(arm64).

- `simctl launch`만으로 WDA가 떴다. xcodebuild와 서명이 필요 없다.
- 확인한 동사: `list`, `info`, `wda --app`(새 설치 포함), `ui`, `tap`(좌표, 라벨, `#id`), `swipe`, `type`(한글 포함), `go home`, `launch`, `kill`, `see`.
- `ui` 한 번(전체 소스 조회)에 약 3.4초 걸렸다. 홈 화면 기준.
- WDA JSON 소스는 `isVisible`, `isEnabled`를 `"1"`/`"0"` 문자열로 준다.

## 다음 (iPhone이 연결되면)

1. `mobax-ios ddi mount` → `see`. iOS 18.5에서 Xcode의 개인화 DDI가 붙는지 본다.
2. `install`로 서명한 WDA를 설치하고 `mobax-ios wda`로 실행한다. 확인할 것:
   - testmanagerd 경로(iOS 17+ 사용자 공간 터널)가 이 기기에서 도는가
   - 마피아42 채널 목록이 `ui`에 위치와 함께 나오는가
   - `tap TEXT`로 채널에 들어가지는가
3. `mobax-ios ax`로 go-ios 순회 결과(요소 15개, 요소당 약 30ms)를 재현한다. 자체 DTX로 포커스 보고가 오는지 본다.
4. Windows 호스트에서 1~3을 반복한다.
5. 실기기 응답 시간: USB 위에서는 전송량 차이가 더 크게 작용할 것으로 본다(XML 36KB 대 JSON 281KB). 연결마다 usbmux 연결을 새로 여는 비용도 재서, 크면 연결을 유지한다.
6. Android와 iOS를 MCP 서버 하나로 합칠지 정한다. 예: `mobax mcp`가 iOS 대상이면 `mobax-ios mcp`를 자식 프로세스로 띄운다. 지금은 의존성(tokio, rustls) 때문에 따로 둔다.
7. 남은 표 잡음:
   - 반투명 시트·오버레이 뒤의 텍스트 행(캘린더 "새로운 기능" 시트 뒤 시간 행)
   - 오버레이 아래쪽에 흐리게 보이는 목록 행
8. 스크롤 응답: 스크롤하면 나간 행과 들어온 행이 기준(행 수의 절반)을 넘어 표 전체를 다시 보낸다(약 800자). 나간 행은 `- @3 @4 @5`처럼 번호만 적으면 절반 가까이 줄 것으로 본다.
