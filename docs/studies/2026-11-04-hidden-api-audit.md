# 숨겨진 API 점검: 공식 API로 바꿀 수 있는 것

날짜: 2026-11-04 · 브랜치: `daemon/public-apis` (main `7cc0926`에서)

appium-uiautomator2-server는 instrumentation 프로세스 안에서 공식 API만 쓴다. main 데몬(`app_process`, shell uid)은 설치가 없고 실시간 화면이 가벼운 대신 숨겨진 API에 기대는 곳이 있다. 프로세스 모델은 유지하고, 공식 대안이 있는 호출만 바꾼다. 대안이 없는 호출은 실패할 때의 대체 경로와 버전별 확인으로 다룬다.

## 분류

### A. `app_process`라서 피할 수 없는 것 (유지)

ActivityThread 없이 Context와 UiAutomation을 직접 만드는 부분이다. instrumentation이라면 프레임워크가 해 주는 일이다.

| 호출 | 위치 | 용도 |
|---|---|---|
| `ActivityThread.systemMain` · `getSystemContext` · `makeApplication` · `mInitialApplication` | Binders, Main | Context, Application |
| `UiAutomationConnection` · `IUiAutomationConnection.connect` | Main | UiAutomation 연결 |
| `VMRuntime.setHiddenApiExemptions` | Main | 숨겨진 API 접근 |
| `ServiceManager.getService` · `I*$Stub.asInterface` | Binders | 시스템 서비스 바인더 |
| `DdmHandleAppName`, `BaseBundle.setShouldDefuse`, `Looper.setSlowLogThresholdMs` | Main | 디버거 이름, 진단 |

### B. 공식 대안이 있는 것 (바꿈)

| 호출 | 위치 | 공식 대안 | API |
|---|---|---|---|
| `IWindowManager.freezeRotation` | Wm | `UiAutomation.setRotation` (instrumentation 브랜치 `8218d28`에서 검증) | 18 |
| `IPackageManager`/`IPermissionManager.grant`·`revokeRuntimePermission` | Pm | `UiAutomation.grantRuntimePermission` · `revokeRuntimePermission` | 28 |
| `IPowerManager.isInteractive` | State | `PowerManager.isInteractive()` | 20 |
| `DisplayManagerGlobal.getRealDisplay` | Screenshot | `DisplayManager.getDisplay` | 17 |

### C. 공식 대안이 없거나 부족한 것 (유지, 대체 경로 확인)

| 호출 | 위치 | 이유 | 지금의 대체 |
|---|---|---|---|
| `SurfaceControl.createDisplay` 외 | MirrorDisplay | 10~13 미러: 공식 길은 MediaProjection(앱·알림 필요) | 실패하면 `takeScreenshot` |
| `IActivityTaskManager.startActivityAsUser` | Am | 공식 `Context.startActivity`는 백그라운드 실행 제한과 결과 코드(`result=2/3`)가 없음 | 없음 |
| `IActivityManager.forceStopPackage` | Am | `ActivityManager.forceStopPackage`는 SystemApi | 없음 (`am force-stop`과 같은 바인더) |
| 3-인자 `injectInputEvent` | Input | 애니메이션 대기를 끄는 길이 숨겨진 쪽뿐 | 2-인자 공개 메서드 |
| `InputEvent.setDisplayId` | Input | 다른 디스플레이로 입력 | 없음 (오류로 알림) |
| `SystemProperties.get`/`set` | Props, State | 공개 API 없음 | 없음 (수십 년 안정) |
| `IClipboard` | Clipboard | 공개 `ClipboardManager`는 백그라운드 읽기가 막힘 | 없음 |
| `ContentProviderHolder.provider`, `AttributionSource` | Providers, SettingsDirect | ContentResolver 없이 프로바이더 직접 호출 | 없음 |
| `SessionParams.installFlags` | Installer | 설치 플래그 일부는 SystemApi | `setRequireUserAction`은 31+ 공개 |
| `IIntentSender` | Transfers | 앱 없이 설치 결과 받기 | 없음 |
| `AssetManager()` + `addAssetPath` | Manifest | 공개 `getResourcesForApplication(...).assets`에는 프레임워크 리소스도 있어 `AndroidManifest.xml`이 어느 APK 것인지(쿠키) 정해지지 않음 | 없음 |

## 다음

- B를 바꾸고 SDK 29, 33, 36에서 확인한다.
- C는 버전별 스모크 테스트(미러, 실행, 입력, 권한, 설치)로 감시한다.
