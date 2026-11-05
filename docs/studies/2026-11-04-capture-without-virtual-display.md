# 가상 디스플레이 없이 화면 캡처하기

날짜: 2026-11-04 · 브랜치: `spike/capture` (`daemon/dual-target`에서; 측정용 `CapSpike.kt`와 함께 지움, 결론은 `DisplayCapture.kt`로 들어감) · 기기: 에뮬레이터 `Medium_Phone` SDK 36, headless, `-gpu host`

## 질문

instrumentation 데몬(app uid, shell 권한 위임)에서 상주 미러(VirtualDisplay) 없이 필요할 때만 캡처한다면, `UiAutomation.takeScreenshot()`보다 빠른 길이 있는가?

## 방법

임시 wire 명령 `cap_spike mode=ua|wm|a11y [scale] [crop] n=10`(`CapSpike.kt`)으로 세 경로를 jar와 APK 두 빌드에서 10회씩 쟀다. 캡처는 버퍼가 데몬 손에 들어오기까지, 인코딩은 일반 Bitmap 변환과 JPEG(품질 80)까지다.

- `ua`: `UiAutomation.takeScreenshot()`. `UiAutomationConnection`이 있는 프로세스가 캡처해 일반 Bitmap으로 복사한 뒤 바인더로 넘긴다. 축소와 자르기는 CPU에서 한다.
- `wm`: 숨겨진 `IWindowManager.captureDisplay(displayId, CaptureArgs, ScreenCaptureListener)`(34+). `frameScale`과 `sourceCrop`이 합성 단계에서 적용되고, `HardwareBuffer`가 데몬으로 온다. WMS는 `READ_FRAME_BUFFER`를 `checkCallingPermission`으로 검사한다.
- `a11y`: UiAutomation의 접근성 연결(`IAccessibilityServiceConnection.takeScreenshot(displayId, RemoteCallback)`, 30+).

## 결과 (중앙값, ms)

| 경로 | 출력 | jar 캡처 / 인코딩 | APK 캡처 / 인코딩 |
|---|---|---|---|
| `ua` | 1080×2400 | 58.5 / 20.5 | 54.3 / 11.7 |
| `ua` 축소 | 383×852 | 58.4 / 5.4 | 54.8 / 3.5 |
| `wm` | 1080×2400 | 33.4 / 41.7 | 33.7 / 32.7 |
| `wm` 축소 | 383×852 | **11.7 / 6.1** | **11.3 / 6.0** |
| `wm` 자르기 | 1080×400 | 12.7 / 8.5 | 12.6 / 6.4 |
| `a11y` | | 응답 없음 | 응답 없음 |

- **`wm`은 APK에서도 된다.** shell 권한 위임이 WMS의 Java 권한 검사에 적용된다.
- 미리보기 크기(383×852)에서 `wm`은 약 17ms이고, `ua`는 약 60ms다. `ua`는 축소해도 캡처 비용이 줄지 않는다. 전체 해상도를 만들어 복사하고 넘긴 뒤에 줄이기 때문이다.
- 전체 해상도에서는 `wm`의 인코딩이 더 느리다. HARDWARE Bitmap을 일반 Bitmap으로 읽어 오는 비용(약 30ms)이 여기에 들어간다. 캡처와 인코딩을 합치면 두 경로가 비슷하다(75 대 66~79).
- `a11y`는 콜백이 오지 않는다. 서비스의 screenshot 권한(capability)을 검사하는 단계에서 막히는 것으로 보인다. oneway 호출이라 예외가 돌아오지 않는다. UiAutomation에는 이 권한이 없고, 창 단위 캡처(`takeScreenshotOfWindow`)도 같은 검사를 거치므로 이 길은 막혀 있다.

## 판단

- 상주 미러 없이 필요할 때 찍는 **정지 화면(`see`, 요소 스크린샷)** 은 `wm` 경로가 맞다. 축소와 자르기를 합성 단계로 넘기는 것이 핵심이다. 요소 스크린샷은 `sourceCrop`으로 그 요소만 합성하면 된다.
- 상주 미러는 768px 기준 약 3ms(SDK 29/33 측정)로 더 빠르다. 다만 미러를 계속 띄워 두는 비용이 든다. 스트림에는 계속 VirtualDisplay를 쓴다.
- 33 이하에는 `IWindowManager.captureDisplay`가 없다. 31–33에서는 `SurfaceControl.captureDisplay`를 써야 하고, 아직 시험하지 않았다.
- 다음: `see`가 미러가 없을 때 `wm` 경로를 먼저 쓰게 하고 SDK 31–35에서 확인한다. 화면이 안 바뀌었을 때(이벤트 번호가 같을 때) 다시 찍지 않는 캐시도 붙인다.
