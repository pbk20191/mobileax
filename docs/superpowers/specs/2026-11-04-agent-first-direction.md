# 에이전트 퍼스트 방향 — 토큰은 적게, 빌트인은 풍부하게

날짜: 2026-11-04 · 참고: appium/appium-uiautomator2-server, openatx/android-uiautomator-server(-jar)

## 목표

- 에이전트가 주고받는 맥락을 최소로 한다.
- 대신 기기 쪽 빌트인 기능은 풍부하게 갖추고 충분히 활용한다.

둘은 충돌하지 않는다. 빌트인을 **"여러 턴에 걸친 일을 한 번에 끝내고 결과만 짧게 돌려주는 동사"**로 만들면 기능이 늘수록 토큰이 준다.

## 원칙

1. **턴을 줄인다.** 턴 하나를 없애는 것이 출력 몇 줄을 줄이는 것보다 크다. 에이전트는 턴마다 지시문과 이전 맥락을 다시 읽기 때문이다. 빌트인의 가치는 "몇 턴을 없애나"로 잰다.
2. **기본 출력은 최소로, 더 필요하면 그때 요청한다.**
   - 관찰은 바뀐 것만 보낸다(`observe_change`).
   - 요소는 `@N` 참조로 가리킨다.
   - 이미지는 요청할 때만, 필요한 영역만, 작게 보낸다.
3. **규칙으로 풀리는 판단은 기기와 호스트에서 결정적으로 한다.** 대기, 스크롤해서 찾기, 팝업 처리, 결과 확인이 해당한다. 처리한 결과는 한 줄로 보고한다.
4. **도구 목록도 토큰이다.** MCP 도구 설명은 세션마다 실린다. 도구 개수는 늘리지 않고, 인자나 `batch`로 확장한다.
5. **측정으로 판단한다.** "작업당 호출 수"와 "작업당 응답 토큰(근사치)"을 벤치마크로 재고, 기능마다 효과를 그 숫자로 확인한다.

## 참고 저장소와 비교

| | Appium UiAutomator2 server | openatx | mobileAX |
|---|---|---|---|
| 실행 | APK 2개 + `am instrument` | `app_process` jar로 옮김. APK는 IME용으로만 선택 설치 | main은 `app_process` |
| 통신 | HTTP W3C WebDriver | HTTP JSON-RPC | adb 소켓 위 바이너리, CLI·MCP |
| 요소 | 서버 쪽 요소 ID | 조회할 때마다 UiSelector로 찾음 | dump 스냅샷 + 호스트 셀렉터 + `@N` |

### 가져올 것 (턴·토큰 효과 순)

| 기능 | 출처 | 효과 | 형태 |
|---|---|---|---|
| 찾을 때까지 스크롤 | `scrollTo`, `ScrollToElement` | "스크롤 → ui → 판단" N회가 1회로 | `tap`/`find`의 `--scroll`. 결과는 한 줄 |
| 팝업 자동 처리 | openatx watcher | 예상 못 한 팝업마다 1~2턴이 0으로 | 세션 규칙으로 등록. 처리하면 다음 관찰에 `handled: …` 한 줄 |
| 토스트 포착 | `getLastToast` | 결과 확인 재조회 1~2턴이 0으로 | 동작 결과에 `toast: …` 한 줄 |
| 조건 대기, 동작 후 확인 | `Until` | 확인 1턴 | 있는 `act --until`과 `wait`를 MCP 인자로 정리 |
| 요소 단위 스크린샷 | `GetElementScreenshot` | 비전 토큰 감소 | `see @N` |
| 트리 상세도 조절 | compressed layout | 무거운 화면의 출력 감소 | 기본은 압축, `--all-nodes`로 확장 |
| 멀티터치, 다중 디스플레이 | 둘 다 | 해당 화면에서만 | 범용 `gesture`, `--display` |
| IME 입력 | AdbKeyboard | `SET_TEXT`가 막힌 입력칸 | 선택 설치 동반 APK, `type`이 실패할 때만 |

### 가져오지 않을 것

- **W3C 세션·요소 ID 모델:** 응답이 장황하고 왕복이 많다.
- **XPath:** 셀렉터가 길어지고 엔진이 무겁다.
- **동작 이력·예약 동작.**

## 실행 방식

- **main(`app_process`)을 핵심으로 둔다.** 설치 단계가 없어 기기 준비에 드는 턴과 실패가 없다. Android 10~13 실시간 화면은 SurfaceControl로 알림 없이 처리한다(`spike/surfacecontrol`).
- **동반 APK는 선택 설치로 둔다.** 설치된 앱이어야만 되는 기능(IME, 알림 리스너, 원하면 정식 monkey용 instrumentation 모드)만 담는다.
- **`daemon/instrumentation`, `daemon/projection-mirror`는 참고용으로 남긴다.** 범용 개선(`ready` 확인, `cp` 오류 처리, `use --port`, 조용한 스트림 정리, `Shell`의 종료 코드)은 main으로 옮긴다.

## 순서

1. **토큰·턴 벤치마크** (`docs/superpowers/plans/2026-11-04-agent-token-benchmark.md`)
2. **main 정리:** SurfaceControl 미러 정식 구현, `selector/regex-has-fixes` merge, 범용 개선 이식
3. **턴을 없애는 빌트인:** 찾을 때까지 스크롤, 팝업 자동 처리, 토스트 포착
4. **출력을 줄이는 빌트인:** 요소 스크린샷, 트리 상세도, MCP 도구 설명 다이어트
5. **필요할 때:** 멀티터치, 다중 디스플레이, 동반 APK
