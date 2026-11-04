# 에이전트 토큰·턴 벤치마크 — 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 대표 작업을 MCP(`mobax mcp`)로 수행할 때 드는 호출 수와 응답 토큰(근사치)을 재고, 기준선을 저장해 이후 기능의 효과를 숫자로 비교할 수 있게 한다.

**Architecture:**
- std-only Python 스크립트(`scripts/agent-bench.py`)가 `mobax mcp`를 stdio JSON-RPC로 띄운다.
- 시나리오 JSON(`bench/agent/*.json`)에 적힌 도구 호출 순서를 그대로 재생한다. 이 순서는 지금 도구로 에이전트가 해야 하는 최소 호출 순서다.
- 호출마다 응답 크기, 근사 토큰, 시간을 기록하고 마지막에 확인 단계로 성공 여부를 판정한다.
- LLM은 돌리지 않는다. 결정적이고 비용이 없다.

**Tech Stack:** Python 3 표준 라이브러리, `mobax mcp`, 에뮬레이터 `Medium_Phone`(SDK 36, 한국어)

**Spec:** `docs/superpowers/specs/2026-11-04-agent-first-direction.md`

## Global Constraints

- **Python 3 표준 라이브러리만 쓴다** (`scripts/wire-smoke.py`와 같은 방식).
- **근사 토큰** = ASCII 문자 수 / 4(올림) + 비ASCII 문자 수.
  - 정확한 토크나이저가 아니라 비교용 일관 척도다.
  - 함수는 하나(`approx_tokens`)에 두고 바꾸지 않는다. 바꾸면 기준선을 다시 만든다.
- **세션 고정 비용도 기록한다.** `tools/list` 응답(도구 설명)은 세션마다 맥락에 실리는 고정 비용이다.
- **기기:** `android emulator start Medium_Phone`. 새 AVD는 만들지 않는다. 화면 문구는 한국어 기준이고, 시나리오의 문구는 이 기기에서 확인한 값이다.
- **시나리오끼리 상태가 새지 않게 한다.** 각 시나리오는 `setup`(앱 강제 종료, 홈 이동)으로 시작한다.
- **판정:** 호출 수와 토큰은 회차 사이 ±5% 안이어야 한다(시계·배터리 문구 등의 미세한 차이 허용). 시간은 3회 중앙값으로 비교한다.

## Review Focus

1. **애니메이션 중 호출로 결과가 회차마다 달라지는 경우.** 동작 도구의 기본 settle을 믿고, 스크립트가 임의의 sleep을 넣지 않는다. 3회 결과의 토큰 편차가 5%를 넘으면 실패로 보고한다. → Task 3 Step 2
2. **이전 시나리오의 상태(열린 앱, 팝업)가 다음 시나리오를 오염시키는 경우.** `setup`으로 막는다. 순서를 바꿔 돌려도 결과가 같은지 확인한다. → Task 3 Step 3
3. **확인 단계 없이 "호출이 성공"만으로 통과하는 경우.** 모든 시나리오는 `check`가 필수이고, 실패하면 결과에 `ok: false`로 남긴다. → Task 1 Step 1(테스트), Task 2
4. **MCP 서버가 오류 응답을 보내도 집계되는 경우.** `isError: true`인 응답은 그 시나리오를 실패로 판정한다. → Task 1 Step 1
5. **`mobax mcp` 프로세스가 응답 없이 멈추는 경우.** 호출마다 30초 타임아웃을 두고, 넘으면 시나리오 실패로 기록하고 다음으로 넘어간다. → Task 1 Step 3

---

### Task 1: 하네스 (`scripts/agent-bench.py`)

**Files:**
- Create: `scripts/agent-bench.py`
- Create: `scripts/test_agent_bench.py`

**Interfaces:**
- Produces:
  - `approx_tokens(text: str) -> int`
  - `class McpClient(exe: str, device: str | None)`. `call(tool, args) -> (text: str, is_error: bool, ms: float)`, `tools_list_text() -> str`, `close()`. 호출마다 30초 타임아웃.
  - `run_scenario(client, scenario: dict) -> dict`. 반환값: `{"name", "ok", "calls", "tokens", "chars", "ms", "steps": [{"tool", "tokens", "ms", "error"}]}`
  - 시나리오 JSON 형식:
    ```json
    {"name": "…", "setup": [{"tool": "…", "args": {}}], "steps": [{"tool": "…", "args": {}}],
     "check": {"tool": "find", "args": {"target": "…"}}}
    ```
    `setup`은 집계하지 않는다. `steps`와 `check`는 집계한다. 에이전트도 확인 호출을 하기 때문이다.
  - CLI:
    ```
    scripts/agent-bench.py [--mobax EXE] [--device SERIAL] [--runs N] [--out FILE] [--compare BASELINE] [SCENARIO…]
    ```
    기본값은 `bench/agent/*.json` 전부, `--runs 3`이다. 표를 stdout에 찍고, `--out`이 있으면 JSON으로 저장한다.

- [ ] **Step 1: 실패하는 테스트 작성** (`scripts/test_agent_bench.py`, unittest)

```python
def test_approx_tokens(self):
    self.assertEqual(ab.approx_tokens(""), 0)
    self.assertEqual(ab.approx_tokens("abcd"), 1)
    self.assertEqual(ab.approx_tokens("abcde"), 2)
    self.assertEqual(ab.approx_tokens("설정"), 2)
    self.assertEqual(ab.approx_tokens("ab설정"), 3)

def test_failed_check_or_error_fails_the_scenario(self):
    fake = FakeClient({"open": ("ok", False), "find": ("NOT_FOUND", True)})
    r = ab.run_scenario(fake, {"name": "x", "setup": [], "steps": [{"tool": "open", "args": {}}],
                               "check": {"tool": "find", "args": {}}})
    self.assertFalse(r["ok"])
    self.assertEqual(r["calls"], 2)

def test_setup_is_not_counted(self):
    fake = FakeClient({"go": ("ok", False), "find": ("hit", False)})
    r = ab.run_scenario(fake, {"name": "x", "setup": [{"tool": "go", "args": {}}], "steps": [],
                               "check": {"tool": "find", "args": {}}})
    self.assertTrue(r["ok"])
    self.assertEqual(r["calls"], 1)
    self.assertEqual(r["tokens"], ab.approx_tokens("hit"))
```

`FakeClient`는 테스트 파일 안에 둔다. `call`이 미리 정한 `(text, is_error)`와 `ms=1.0`을 돌려준다.

- [ ] **Step 2: 실패 확인**

Run: `python -I scripts/test_agent_bench.py`
Expected: ImportError 또는 AttributeError로 FAIL

- [ ] **Step 3: 구현**

- `McpClient`:
  - `subprocess.Popen([exe, "mcp"] + (["--device", d] if d else []), stdin=PIPE, stdout=PIPE)`.
  - `initialize`, `notifications/initialized`를 보낸 뒤 `tools/call`을 줄 단위 JSON으로 주고받는다.
  - 응답 텍스트는 `result.content`의 text들을 이어 붙인다.
  - 30초 타임아웃은 읽기 스레드와 `queue.get(timeout=30)`으로 구현한다. 넘으면 `("TIMEOUT", True, 30000)`.
- `run_scenario`: `setup`을 실행하고(결과 무시), `steps`와 `check`를 집계한다. 하나라도 `is_error`면 `ok=False`.
- `--compare`:
  - 시나리오별 `calls`, `tokens`, `ms`(중앙값)의 증감을 표로 찍는다.
  - `tools/list` 근사 토큰도 한 줄로 비교한다.

- [ ] **Step 4: 통과 확인**

Run: `python -I scripts/test_agent_bench.py`
Expected: 3 tests OK

- [ ] **Step 5: Commit**

```bash
git add scripts/agent-bench.py scripts/test_agent_bench.py
git commit -m "bench: agent token/turn harness over mobax mcp"
```

---

### Task 2: 시나리오 5개 (`bench/agent/`)

각 시나리오의 문구와 셀렉터는 기기에서 `mobax ui`로 확인해 정한다. 이 탐색 호출은 집계하지 않는다. `steps`에는 지금 도구로 에이전트가 할 최소 순서만 적는다.

**Files:**
- Create:
  - `bench/agent/s1-settings-item.json`
  - `bench/agent/s2-long-list.json`
  - `bench/agent/s3-search-form.json`
  - `bench/agent/s4-permission-popup.json`
  - `bench/agent/s5-heavy-screen.json`

| 시나리오 | 내용 | 측정하려는 것 |
|---|---|---|
| s1 | 설정 열기 → "디스플레이" → `check`: 어두운 테마 항목이 보임 | 기본 탐색 비용 |
| s2 | 설정 → 앱 → 전체 앱 목록에서 아래쪽 앱(예: "Chrome"이 아닌 목록 하단 항목) 찾기 | 스크롤 반복 비용. 이후 `--scroll`의 효과 비교 대상 |
| s3 | 설정 검색 → "배터리" 입력 → 결과 탭 → `check`: 배터리 화면 | 입력과 결과 확인 |
| s4 | `setup`: 카메라 앱 데이터 초기화(`mobax` 도구로 `shell pm clear …`, 불가능하면 `close`만) → 열기 → 권한 팝업 처리 → `check`: 카메라 화면 | 팝업 처리 비용. 이후 자동 처리의 효과 비교 대상 |
| s5 | 최근 앱 화면 `ui` 한 번 | 무거운 화면의 출력 크기 |

- [ ] **Step 1: 기기 준비**

Run: `android emulator start Medium_Phone` 후 `target/release/mobax use`
Expected: `daemon up`

- [ ] **Step 2: 시나리오별로 수동 탐색 후 JSON 작성.** 각 파일을 `scripts/agent-bench.py bench/agent/<file>`로 한 번씩 돌린다.
Expected: 모든 시나리오 `ok: true`

- [ ] **Step 3: s4의 `setup` 결정.** 카메라 앱 패키지와 권한 팝업이 실제로 뜨는지 확인한다. 뜨지 않으면 다른 기본 앱에서 팝업이 뜨는 흐름을 찾아 대체한다. 무엇으로 했는지는 파일의 `"note"` 필드에 적는다.

- [ ] **Step 4: Commit**

```bash
git add bench/agent/
git commit -m "bench: five agent scenarios on Medium_Phone (ko-KR)"
```

---

### Task 3: 기준선과 문서

**Files:**
- Create: `bench/agent/baseline.json` (`--out` 결과)
- Modify: `docs/benchmark.md` (agent 벤치마크 절 추가: 목적, 근사 토큰 정의, 실행법, 기준선 표)

- [ ] **Step 1: 기준선 측정**

Run: `python -I scripts/agent-bench.py --runs 3 --out bench/agent/baseline.json`
Expected: 5개 모두 `ok: true`, 표 출력, `tools/list` 근사 토큰 한 줄

- [ ] **Step 2: 반복 안정성 확인 (Review Focus 1)**

같은 명령을 한 번 더 실행하고 `--compare bench/agent/baseline.json`으로 비교한다.
Expected: 시나리오별 `calls`가 같고, `tokens` 차이가 ±5% 안

- [ ] **Step 3: 순서 독립성 확인 (Review Focus 2)**

시나리오를 역순으로 인자에 넘겨 실행하고 비교한다.
Expected: Step 2와 같은 기준 통과

- [ ] **Step 4: `docs/benchmark.md` 갱신 후 commit**

```bash
git add bench/agent/baseline.json docs/benchmark.md
git commit -m "bench: agent token/turn baseline"
```
