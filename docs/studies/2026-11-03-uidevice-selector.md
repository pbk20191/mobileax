# UiDevice/By 셀렉터 스터디 (main, app_process 데몬)

날짜: 2026-11-03 · 브랜치: `study/uidevice-selector` (origin/main `c05f457`에서) · 기기: 에뮬레이터 `Medium_Phone` SDK 36

## 질문

1. 설치 없이 shell uid로 도는 main의 `app_process` 데몬에서 공식 `androidx.test.uiautomator`(`UiDevice`, `By`, `UiObject2`)를 쓸 수 있는가?
2. 쓸 수 있다면, 지금의 호스트 쪽 셀렉터(`mobax-core/src/selector.rs`, dump JSON 위에서 매칭)를 `By` 기반으로 바꿀 이유가 있는가?

## 방법 (스파이크, 병합하지 않음)

- `build.gradle.kts`에 `androidx.test.uiautomator:uiautomator:2.4.0`을 추가했다. 소스를 복사하지 않고 공식 라이브러리를 그대로 썼다.
- `ByStudy.kt`: `Instrumentation`을 상속한 shim이 `getUiAutomation()`/`getUiAutomation(flags)`에서 데몬이 이미 연결한 UiAutomation을, `getContext()`/`getTargetContext()`에서 시스템 컨텍스트를 돌려준다. `Configurator.uiAutomationFlags`는 데몬과 같은 값(`FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES`)으로 맞췄다. 이 shim을 `UiDevice.getInstance(shim)`에 넘긴다.
- 임시 wire verb `by_find KEY=VALUE`는 `UiDevice.findObjects(By…)`를 실행하고 개수, 첫 결과의 `visibleBounds`, 기기 안 소요 시간을 돌려준다.
- 같은 화면에서 `mobax find '<호스트 셀렉터>'`와 비교했다. 측정 스크립트는 각 질의를 10회 실행했다.

## 결과

### 1. 실현 가능성: 된다

- shim Instrumentation만으로 공식 uiautomator 2.4.0이 `app_process` 데몬 안에서 동작했다. `InstrumentationRegistry`, 실제 instrumentation, 소스 복사가 모두 필요 없었다.
- `UiDevice` 초기화는 27ms, APK 크기는 +36KB였다(194KB → 231KB, R8 적용 후).
- 시험한 `By`는 `text`, `textContains`, `text(Pattern)`, `res`, `clazz`, `clickable`, `hasDescendant`, `desc`이고 모두 정상 응답했다.
- 단서: shim은 `UiDevice`가 Instrumentation에서 위 네 메서드만 쓴다는 데 기대고 있다. SDK 36에서만 확인했다. `UiDevice`의 다른 기능(`executeShellCommand`, 화면 회전, `pressKeyCode`)은 시험하지 않았다.

### 2. 속도: 비긴다

| 질의 (설정 첫 화면) | `By` 기기 안 시간(중앙값) | `by_find` 전체 왕복 | 호스트 `find` 전체 왕복 |
|---|---|---|---|
| `text=네트워크 및 인터넷` | 4.3ms | 181ms | 165ms |
| `textContains=네트워크` | 4.7ms | 165ms | 180ms |
| `res=android:id/title` | 4.1ms | 168ms | 183ms |
| `clickable` | 4.0ms | 162ms | 165ms |

- 비교 기준: 호스트 방식의 기기 쪽 비용은 `dump_active` 한 번이고, 같은 화면에서 중앙값 3.6ms였다(`mobax dev bench`). 전체 왕복은 둘 다 CLI 프로세스 시작이 대부분이다.
- 앱 서랍(노드 51개)에서도 `By.clickable` 4.1~4.9ms, `dump_active` 3.4ms로 차이가 없었다.
- 예외: 화면을 막 연 직후 첫 `By` 조회는 448ms가 걸렸다. live 트리를 처음 훑으며 접근성 캐시를 채우기 때문으로 보인다. dump 쪽은 전환 대기(`waitOutTransition`)를 거친다.

### 3. 의미 차이

| 항목 | `By` / UiObject2 | 지금 셀렉터 |
|---|---|---|
| 보이지 않는 노드 | 기본으로 제외(보이는 노드만) | 기본으로 포함, `--visible`/`:visible`로 걸러야 함. 설정 화면 `clickable`이 10 대 21이었고, 차이 11개는 스크롤 아래 화면 밖 항목(bounds가 화면 밖이거나 뒤집힘) |
| 텍스트 | 정확·부분·접두·접미, `Pattern`(정규식, 대소문자 무시 가능) | 정확·부분·접두·접미. 정규식 없음, 대소문자 구분 |
| 구조 | `hasChild`/`hasDescendant`/`hasParent`/`hasAncestor`, `depth` | `:in(SEL)`(조상). `:has()` 없음 |
| `hasDescendant` 결과 | 그 후손을 가진 모든 조상이 매치(예: 12개) | 해당 없음 |
| 위치 관계 | 없음 | `:below`, `:right-of`, `:near` |
| 대상 | 조회 시점의 live 트리. 결과 객체가 금방 낡을 수 있음(`StaleObjectException`) | 에이전트가 본 그 dump(`ui`의 `@N`과 같은 스냅샷) |
| 문자열 문법 | 없음(코드 API) | CSS/Playwright 계열 문자열 |

## 판단

- **`By`로 매칭 엔진을 바꿀 이유는 약하다.**
  - 속도 이득이 없다.
  - CLI, MCP, 스크립트에는 문자열 문법이 여전히 필요해서 파서는 그대로 남는다.
  - 에이전트에게 쓸모가 큰 위치 관계(`:below`, `:near`)를 잃는다.
  - live 조회는 에이전트가 본 스냅샷과 어긋날 수 있다.
- **`By`에서 가져올 것:**
  1. **보이는 노드만 기본으로.** 화면 밖 노드를 누르거나 세는 실수를 막는다. 기존 스크립트의 결과가 바뀌는 의미 변경이라 결정이 필요하다. 예를 들어 `--all-nodes`로 예전 동작을 남길 수 있다.
  2. **정규식과 대소문자 무시.** `Pattern`에 해당하는 연산자.
  3. **`:has(SEL)`.** `hasDescendant`에 해당한다. 가장 가까운 조상만 고르는 규칙을 함께 정해야 한다(`By`처럼 모든 조상이 매치되면 모호해진다).
- **`UiDevice`를 다른 용도로 쓰는 길은 열려 있다.** main에서도 shim으로 동작하니, 필요해지면 설치 없이 들여올 수 있다. 예를 들어 `UiObject2.scrollUntil`/`fling` 같은 스크롤 동작이다.
- 셀렉터 파서의 기존 버그(따옴표 안의 `]`·`(`·연산자 문자, 모르는 속성 이름의 무음 실패, `:below`가 거리순이 아님, 관계 조건의 반복 탐색)는 이 결정과 별개로 고칠 일이다.
