# iPhone 제어 스터디 (go-ios 실측, idevice 이식)

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
- Windows에서는 `ios ui install wda --p12file … --profile … --path <공식 zip>`으로 서명한다. `.p12`를 한 번 내보내야 한다.

## mobax-ios 크레이트 (이 브랜치)

- idevice 기반이고 lockdown만 쓴다. 터널과 관리자 권한이 필요 없다.
- Windows에서는 Apple Mobile Device Service(TCP 27015)로 붙는다. idevice가 지원하지만 시험하지 않았다.
- 워크스페이스 멤버이지만 `default-members`와 CI(`-p mobax`)에는 들어가지 않는다.
- 동사:
  - `list`, `info`
  - `see PATH.png`: screenshotr. Developer Disk Image가 마운트돼 있어야 한다
  - `logs [--grep]`
  - `ax [--max N] [--attrs]`, `ax activate TEXT`
- `archive.rs`는 NSKeyedArchiver 인코더를 직접 구현했다. ns-keyed-archive는 `NSDictionary`만 쓰고 `NSNull`이 없는데, 접근성 서비스에는 `NSMutableDictionary` 래퍼와 `NSNull` 인자를 보낸다.
- 단위 테스트는 인코딩 왕복과 포커스 보고 파싱을 본다. 포커스 보고는 실측한 모양을 재현했다.
- **기기에서 아직 돌리지 않았다.** 확인할 위험:
  1. idevice의 DTX 클라이언트는 기기 쪽 호출(`hostInspector…`)이 응답을 기대해도 자동으로 응답하지 않는다. 핸들러 등록 API가 crate 내부용이라서다. 기기가 응답을 기다리며 멈추면 idevice에 핸들러 API를 열거나 DTX를 직접 다뤄야 한다.
  2. 응답과 기기 쪽 호출은 "데이터가 `:`로 끝나는 셀렉터 문자열인가"로 구분한다. 메시지 헤더의 대화 번호 접근자가 crate 내부용이라서다.
  3. iOS 17 이상 screenshotr는 DDI 마운트가 필요하다. 안 되면 터널 위 DVT 스크린샷으로 바꾼다.

## 다음 (기기가 연결되면)

1. `mobax-ios ax`로 go-ios 순회 결과를 재현한다. 요소 수, 시간, 위험 1·2를 확인한다.
2. `mobax-ios see`, `logs`를 확인한다.
3. 서명한 WDA를 설치하고 `ios ui run wda`로 실행해 다음을 본다.
   - 마피아42 채널 목록 트리에 위치가 나오는가
   - 좌표 탭으로 채널에 들어가지는가
4. Windows 호스트에서 1~3을 반복한다.
5. 결과에 따라 WDA HTTP 클라이언트를 `mobax-ios`에 붙여 `ui`, `tap`, `find`를 mobax와 같은 모양으로 맞춘다. 접근성 순회는 서명 없는 환경을 위한 대체 경로로 둔다.
