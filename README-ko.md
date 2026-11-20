# claude-monitor

## 개요

1. **소개**
    - Claude Code용 localhost 허브 역할을 하는 상시 실행 단일 Windows 실행 파일
    - 세션 기록(transcript), OTLP 로그, 선택 사항인 API 게이트웨이에서 사용량 메타데이터를 모아 SQLite 파일 하나에 저장하고, 실시간 터미널 대시보드, 로컬 관리 API, 일일 Markdown 리포트로 보여 줌
    - 사용자의 PreToolUse 가드가 보기 전에 구형 CLI 호출을 재작성해 Bash 훅을 빠르게 만드는 기능도 있음
2. **문서**
    - 영어 원문: [README.md](README.md)
    - 소스 트리, 런타임 구성, 데이터 흐름, 병합 규칙, 보안 경계: [ARCHITECTURE-ko.md](ARCHITECTURE-ko.md)
    - 라이선스(Apache-2.0): [LICENSE.md](LICENSE.md)
3. **배지**
    - [![Crates.io](https://img.shields.io/crates/v/claude-monitor.svg)](https://crates.io/crates/claude-monitor) [![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE.md)
4. **하는 일**
    - 세션 기록(transcript), OTLP 로그·메트릭(http/json 또는 http/protobuf), API 게이트웨이, Claude Code의 statusLine 명령 네 출처에서 사용량 메타데이터를 수집
    - SQLite 파일 하나에 저장하고, 관리 API, 대시보드, 일일·주간 리포트로 집계를 제공
    - 내장 모델별 요금표(캐시 쓰기는 5분·1시간 단가를 따로 적용)로 비용을 추정하며, `config.toml`의 `[pricing.models]`로 덮어쓸 수 있음
    - 상태 줄에서 받은 공식 5시간·7일 플랜 한도, 컨텍스트 사용량, 프롬프트 캐시 타이머를 보여 주고, 스크립트용 한도 점검(`status --once`)을 제공
    - Bash 훅의 구형 CLI 호출(`cat`, `ls`, `git diff`)을 가드가 거부하기 전에 재작성하고, 최종 명령은 가드가 다시 검사
    - 강제 최신 대기 대응: Claude Code가 작업 중인 동안 Windows에 시스템 절전 유예를 요청하고(화면 끄기·잠금 정책은 그대로), 대기 구간을 기록하며, 복귀 뒤 대기 구간에 걸친 실패는 환경 원인이라고 에이전트에게 알림
5. **특징**
    - 내장 SQLite(WAL)를 쓰는 단일 실행 파일이며 Node, Python, 데이터베이스 서버가 필요 없음
    - localhost 포트 4개: 관리 `47330`, OTLP `47331`, 게이트웨이 `47332`(기본 켜짐), 훅 `47333`
    - 메타데이터만 저장: 이름, 횟수, 소요 시간, 토큰 수, 가드 라벨이며 프롬프트 본문, 명령 본문, 파일 내용, 인증 헤더는 저장하지 않음
    - fail-closed 훅 체인: 가드가 응답하지 못하면 호출을 거부
    - 핫 경로(훅 체인, 수집, 색인, 대시보드)에서는 자식 프로세스를 띄우지 않으며, 유일한 생성은 명령 없이 실행했을 때 한 번 일어나는 분리 `serve` 기동

---

## 요구 사항

1. **플랫폼**
    - Windows: 대기 감시, 분리 기동, 콘솔 제어가 Win32 API를 씀
2. **Claude Code**
    - 아래 텔레메트리, 게이트웨이, 훅 단계는 Claude 홈(`%USERPROFILE%\.claude`, 또는 `CLAUDE_CONFIG_DIR`)의 `settings.json`을 편집함
    - 훅 항목은 HTTP 훅 형식(`"type": "http"`)을 씀
3. **Rust 툴체인**
    - 빌드나 `cargo install`에만 필요: Rust 1.89 이상(edition 2024)과, 내장 SQLite와 `ring`을 위한 C 컴파일러(MSVC 툴체인에서는 Visual Studio Build Tools)
4. **가드 서버**
    - 훅 체인에만 필요: 루프백 HTTP 포트(기본 `http://127.0.0.1:47321`)에서 `POST /hook/pretool-guard`와 `GET /health`에 응답하는 기존 PreToolUse 가드
    - 이 프로젝트는 가드를 제공하지 않으며, 가드가 없으면 훅 체인을 건너뛰고 수집과 게이트웨이 기능만 사용

---

## 경고

1. **fail-closed 등록**
    - `pretool-chain`을 `onFailure: "block"`으로 등록하면 `serve`가 꺼진 동안 모든 Bash 호출이 막힘
    - 가드가 `hooks.guard_deadline_ms` 안에 응답하지 않아도 체인이 호출을 거부함
2. **등록 전**
    - `serve`를 먼저 실행하고 `claude-monitor doctor`를 돌려 `fail` 항목을 모두 해결함
    - `doctor`는 `fail` 항목이 있을 때만 종료 코드 1을 반환
3. **serve 상시 실행**
    - 작업 스케줄러로 `serve`를 상시 실행함(상시 실행 절 참고): 비정상 종료 후 작업이 `serve`를 다시 띄울 때까지는 Bash가 막힌 상태로 남음

---

## 빠른 시작

### 1. 설치

1. **crates.io에서**
    - release 바이너리를 `%USERPROFILE%\.cargo\bin`에 설치

```bat
cargo install claude-monitor
```

2. **소스에서**
    - 저장소를 클론한 뒤 기본 cargo로 빌드하며, exe는 `target\release\claude-monitor.exe`에 생기고 `CARGO_TARGET_DIR`을 지정하면 그 아래에 생김

```bat
cargo build --release
```

3. **툴체인 참고**
    - 기본 MSVC나 gnullvm Windows 툴체인은 기본 cargo로 빌드되며, 툴체인 전용 링커 설정은 작성자의 로컬 환경에만 있고 배포하지 않음
    - `windres`는 선택 사항: Windows 타깃에서 `build.rs`가 빌드 출력 폴더에 리소스 스크립트(앱 아이콘 `assets/icon/claude-monitor.ico`, 영어 `VERSIONINFO` 블록, `asInvoker` manifest)를 만들고 `<arch>-w64-mingw32-windres`(링커 옆을 먼저, 그다음 `PATH`), `llvm-windres --target=<arch>-w64-mingw32`, `windres` 중 처음 동작하는 도구로 컴파일
    - 만들어진 오브젝트의 COFF 머신 타입이 타깃과 다르면(예: `aarch64-pc-windows-msvc`에 x64 오브젝트) 경고와 함께 버려 링커에 넘기지 않고, 동작하는 도구가 하나도 없으면 경고를 출력하고 아이콘, 버전 정보, manifest 없이 빌드
    - gnullvm 툴체인에서는 exe가 `libunwind.dll`을 로드함: `build.rs`는 빌드 결과물 옆에만 복사하므로, exe를 옮기거나 `cargo install`한 경우 `<rustc --print sysroot>\bin`에서 exe 옆으로 복사함
4. **설치 확인**
    - `--version`과 `--help`는 아무것도 기동하지 않고 출력

```bat
claude-monitor --version
```

### 2. 첫 실행

1. **그냥 실행**
    - 터미널에서 명령 없이 exe를 실행
    - 관리 포트가 응답하지 않으면 `claude-monitor serve`를 분리 프로세스로 띄우고(콘솔 창 없음, Windows가 허용하면 부모 Job에서 이탈), `/health`가 `200 ok`로 응답할 때까지 최대 8초 기다림. 이 응답은 저장소, 토큰, 런타임 준비가 끝난 뒤에만 나옴
    - 이어서 대시보드를 열며, 대시보드를 닫아도 `serve`는 계속 실행되고 두 번째 실행은 이미 떠 있는 `serve`를 그대로 씀
    - 분리된 서버의 출력은 데이터 폴더의 `serve.log`에 기록되며 실행마다 새로 만듦
    - 첫 실행은 데이터 폴더 `~/.config/claude-monitor/`(Windows에서는 `%USERPROFILE%\.config\claude-monitor\`)와 데이터베이스를 만들며, Claude Code가 데이터를 만들 때까지 대시보드는 비어 있음

```bat
claude-monitor
```

2. **서버만**
    - `serve`는 켜진 모든 포트와 배경 작업을 포그라운드로 실행하며, 작업 스케줄러나 디버깅용이고 시작할 때마다 새 관리 토큰을 기록
    - 기동 순서: 게이트웨이 상류를 먼저 검증하고, 관리, OTLP, 훅, 게이트웨이 순으로 포트를 바인딩하며, `admin.token`은 필수 포트 바인딩이 끝난 뒤에만 기록
    - 필수 포트는 관리와 훅(`hooks.enabled`일 때)이며, 하나라도 사용 중이면 `serve`는 오류로 종료
    - OTLP와 게이트웨이는 선택: 바인딩에 실패하면 경고를 출력하고 그 서버만 빼고 실행
    - `hooks.enabled = false`이면 훅 포트를 점유하지 않음

```bat
claude-monitor serve
```

3. **대시보드만**
    - `tui`는 서버를 띄우지 않고 대시보드만 여며, 저장된 데이터는 볼 수 있지만 상태, 재색인, 리포트 생성은 `serve`가 필요함

```bat
claude-monitor tui
```

### 3. Claude Code 연결

1. **편집 위치**
    - Claude 홈의 `settings.json`을 편집하며, 아래 스니펫은 모두 기본 포트일 때 `doctor`가 해결책으로 출력하는 내용과 같음
    - 편집한 뒤에는 새 Claude Code 세션을 시작해 새 값을 읽게 함
2. **텔레메트리 환경 변수**
    - `env` 블록에 아래 키를 추가하면 Claude Code가 OTLP 로그와 메트릭을 수신기로 보냄
    - 수신기는 `http/json`과 `http/protobuf`를 받으며, 메트릭 키가 대시보드의 스킬, MCP 서버, 플러그인, 에이전트, 모델별 비용 귀속을 채움

```json
"env": {
  "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
  "OTEL_LOGS_EXPORTER": "otlp",
  "OTEL_METRICS_EXPORTER": "otlp",
  "OTEL_EXPORTER_OTLP_ENDPOINT": "http://127.0.0.1:47331",
  "OTEL_EXPORTER_OTLP_PROTOCOL": "http/json"
}
```

3. **게이트웨이 경유(선택)**
    - 게이트웨이는 기본 켜짐이므로 `serve`가 이미 포트 `47332`를 잡고 `gateway.upstream`으로 중계하며, `config.toml`을 바꿀 필요 없음
    - Claude Code 자신의 API 트래픽을 계측하려면 `env` 블록에 아래 키를 추가: 사용자 지정 기본 URL을 거치면 백그라운드 작업이 메인 모델을 쓰므로 `ANTHROPIC_DEFAULT_HAIKU_MODEL`로 작은 모델을 지정함
    - `ANTHROPIC_BASE_URL`이 멈춘 게이트웨이나 `[gateway] enabled = false`로 끈 게이트웨이를 가리키면 API 호출이 연결에 실패하며, 끈 경우 `doctor`가 `fail`로 보고

```json
"env": {
  "ANTHROPIC_BASE_URL": "http://127.0.0.1:47332",
  "ANTHROPIC_DEFAULT_HAIKU_MODEL": "<haiku 모델 ID>"
}
```

4. **훅 체인(선택, 가드 필요)**
    - `hooks.PreToolUse`에서 `^Bash$` 항목 중 `/hook/pretool-guard`를 가리키는 것을 아래 항목으로 교체하며, 먼저 경고 절을 읽음
    - `onFailure`는 `block`, `timeout`은 `10`을 유지
    - 옛 가드 항목을 함께 두지 않음: 훅이 병렬로 실행되어 가드 거부가 재작성을 이기며, `doctor`가 그 조합을 경고함

```json
{"matcher":"^Bash$","hooks":[{"type":"http","url":"http://127.0.0.1:47333/hook/pretool-chain","timeout":10,"onFailure":"block"}]}
```

5. **훅 URL 허용**
    - `settings.json`에 `allowedHttpHookUrls` 목록이 있으면 아래 항목을 추가: 없으면 훅이 실행되지 않으며 `doctor`가 `fail`로 보고
    - 빈 목록은 모든 HTTP 훅을 막아 `fail`, 목록 자체가 없으면 제한 없음(`ok`)

```json
"allowedHttpHookUrls": ["http://127.0.0.1:47333/hook/*"]
```

6. **대기 안내 훅(선택)**
    - `hooks.PostToolUseFailure`와 `hooks.UserPromptSubmit` 배열에 기존 항목과 나란히 아래 항목을 하나씩 추가함
    - `onFailure`는 `continue`로 둠: 이 경로는 문맥만 더하므로 `serve`가 멈춰도 프롬프트나 도구 결과를 막으면 안 됨
    - 항목이 없어도 대기 구간은 기록되고 `GET /api/power`로 보이지만, 에이전트에게 안내는 가지 않음

```json
{"hooks":[{"type":"http","url":"http://127.0.0.1:47333/hook/standby-hint","timeout":5,"onFailure":"continue"}]}
```

7. **상태 줄(선택)**
    - `claude-monitor statusline`을 `statusLine` 명령으로 등록하면 대시보드와 `status --once`가 플랜 한도, 컨텍스트 사용량, 프롬프트 캐시 상태를 받음
    - stdin으로 Claude Code의 statusLine JSON을 읽어 한도·컨텍스트·캐시 수치(와 세션 id, 모델, 세션 비용)만 약 250 ms 예산 안에 `/hook/statusline`으로 보내고, 있는 값만 담은 `5h 42% | 7d 18% | ctx 63%` 같은 짧은 한 줄을 출력
    - `serve`가 꺼져 있거나 전송에 실패해도 이 한 줄은 늦어지거나 깨지지 않음
    - 공식 5시간·7일 값은 Claude Code가 Pro, Max, Ultra 구독에만, 세션의 첫 API 응답 뒤에만 제공하며, 그 전에는 개요 카드가 로컬 토큰 추정을 보여 줌

```json
"statusLine": {"type": "command", "command": "claude-monitor statusline"}
```

8. **기존 상태 줄 감싸기(선택)**
    - `--wrap`은 `--` 뒤의 기존 statusLine 명령을 같은 stdin으로 실행하고 출력을 그대로 통과시키며, 종료 코드는 감싼 명령의 것
    - `--append`는 감싼 출력 뒤에 `claude-monitor` 한 줄을 새 줄로 덧붙임
    - 감싼 명령을 실행하지 못하면 오류는 stderr로 나가고 대신 `claude-monitor` 한 줄을 출력

```json
"statusLine": {"type": "command", "command": "claude-monitor statusline --wrap --append -- <기존 statusline 명령> [인자...]"}
```

### 4. 확인

1. **doctor**
    - `doctor`는 항목마다 `[level] topic: message`를 출력하고 해결책이 있으면 `->` 줄을 붙이며, 데이터 폴더, 데이터베이스, 포트, 중계 대상, 훅 등록, 허용 URL, 텔레메트리, 게이트웨이, projects 폴더를 점검
    - `fail`과 `warn` 항목이 없도록 맞추며, 가드나 훅 체인이 없으면 `relay-target`과 `hook-registration` 경고는 정상
2. **상태**
    - `status`는 관리 `/api/status` JSON을 출력: 버전, pid, 가동 시간, 데이터 폴더, 포트 상태, 실시간 카운터
    - Claude Code 세션을 한 번 쓴 뒤 `status`의 `otlp_batches`가 늘어야 함

```bat
claude-monitor doctor
claude-monitor status
```

3. **한도 점검**
    - `status --once`는 데이터베이스의 최신 상태 줄 스냅샷을 바로 판정하며(읽기 전용, `serve` 불필요, 데이터베이스를 만들지 않음), `level`, `five_hour_pct`, `five_hour_resets_at`, `seven_day_pct`, `seven_day_resets_at`, `snapshot_ts`, `source`를 담은 JSON 한 줄을 출력
    - 종료 코드가 수준을 나타냄: `0` 정상, `10` 경고(5시간·7일 중 높은 값이 `--warn` 퍼센트 이상, 기본 80), `11` 한도(100 이상), `20` 알 수 없음(스냅샷 없음, 한도 표가 생기기 전에 만들어진 데이터베이스, `--max-age`분(기본 360)보다 오래된 스냅샷, 아직 초기화되지 않은 창 없음), `30` 오류(설정, 데이터베이스, 상태 파일 실패)
    - 초기화 시각이 지난 창은 빈 값으로 보며, `--warn`은 0 초과 100 이하, `--max-age`는 1 이상이어야 함
    - `--state-file <경로>`를 주면 위젯이나 스크립트용으로 같은 JSON 한 줄을 원자적으로 쓰며, 주지 않으면 파일을 쓰지 않음

```bat
claude-monitor status --once --warn 90 --state-file C:/Users/<user>/limits.json
```

### 5. 상시 실행

1. **작업 스케줄러**
    - 현재 사용자 계정으로 로그온 시 `conhost.exe --headless "<exe>" serve`를 실행하는 작업 `claude-monitor`를 등록
    - 실패 시 재시작: 최대 999회, 1분 간격이며 실행 시간 제한이 없고 두 번째 인스턴스는 무시함
    - 작업은 다음 로그온부터 시작하며, 지금 시작하려면 `Start-ScheduledTask -TaskName claude-monitor`를 실행
    - `$exe`는 PATH에서 exe를 찾으므로, PATH에 없는 소스 빌드는 exe 전체 경로를 지정

```powershell
$exe = (Get-Command claude-monitor).Source
$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = New-ScheduledTaskAction -Execute "conhost.exe" -Argument "--headless `"$exe`" serve" -WorkingDirectory (Split-Path -Parent $exe)
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew -StartWhenAvailable
Register-ScheduledTask -TaskName claude-monitor -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force
```

2. **감독**
    - 관리나 훅 서버가 끝나거나 panic하면 프로세스 전체가 0 이 아닌 코드로 종료함. 반만 동작하는 `serve`가 남지 않고 작업 스케줄러가 다시 띄움
    - OTLP, 색인기, 리포트 스케줄러, 게이트웨이가 끝나면 `serve`는 경고를 출력하고 대시보드에 warn 알림을 보낸 뒤 계속 실행
    - 관리나 훅 포트가 이미 사용 중인 두 번째 `serve`는 오류로 종료하고 실행 중인 인스턴스의 토큰은 건드리지 않음
3. **대시보드 없이 시작**
    - `start`는 분리된 `serve`가 떠 있도록 보장한 뒤 대시보드를 열지 않고 종료하며, 직접 만든 로그온 실행기에 씀

```bat
claude-monitor start
```

### 6. 중지와 제거

1. **중지**
    - 먼저 `settings.json`에서 `pretool-chain` 항목을 제거함: 그렇지 않으면 `serve`가 꺼진 동안 모든 Bash 호출이 막힘
    - 작업으로 실행 중인 `serve`는 `Stop-ScheduledTask -TaskName claude-monitor`로, 분리 실행한 `serve`는 `Stop-Process -Name claude-monitor`로 멈추며 후자는 열려 있는 대시보드도 닫음. 포그라운드 `serve`는 `Ctrl+C`로 끝냄
2. **제거**
    - 작업 등록을 해제하고, 추가했던 `env`, `hooks`, `allowedHttpHookUrls` 항목을 제거한 뒤 바이너리를 삭제함(`cargo uninstall`은 `cargo install`한 경우에만 해당)
    - 데이터 폴더 `~/.config/claude-monitor/`(설정 파일, 데이터베이스, 토큰, 로그, 리포트, 대시보드 상태)는 직접 지울 때까지 남음

```powershell
Unregister-ScheduledTask -TaskName claude-monitor -Confirm:$false
cargo uninstall claude-monitor
```

### 7. 문제 해결

1. **모든 Bash 호출이 막힘**
    - `serve`가 꺼진 동안 훅 체인이 `onFailure: "block"`으로 등록된 상태: `claude-monitor start`를 실행하거나 등록을 제거
2. **`doctor`가 `relay-target`을 경고**
    - `hooks.relay_target`에서 `GET /health`에 응답하는 가드가 없음: 가드를 띄우거나, `hooks.relay_target`을 그 가드로 지정하거나, 훅 체인을 등록하지 않음
3. **포트가 사용 중**
    - 관리나 훅이 사용 중이면 `serve`가 오류로 종료하고, OTLP나 게이트웨이는 경고와 함께 건너뜀: `config.toml`의 `[ports]`를 바꾸고 재시작
4. **`otlp_batches`가 0에서 늘지 않음**
    - `doctor`를 실행해 `telemetry` 항목을 해결한 뒤 새 Claude Code 세션을 시작: 수신기는 `http/json`과 `http/protobuf`만 받음(다른 content type은 `415`)
5. **`ANTHROPIC_BASE_URL`을 지정한 뒤 API 호출이 실패**
    - 게이트웨이가 실행 중이 아니거나 꺼져 있음: `serve`를 시작하거나 키를 제거
6. **`serve`가 설정 오류로 종료**
    - 설정 오류는 종료 코드 2: 알 수 없는 키, 포트, 설정 절의 제한 값을 확인
7. **`POST`가 401을 반환**
    - 관리 토큰은 `serve`를 시작할 때마다 다시 만들어지므로 현재 `admin.token`을 읽음

---

## 명령

1. **serve**
    - 상주 서버 실행: 관리, OTLP, 훅, 게이트웨이, 기록 색인기, 리포트 스케줄러
2. **start**
    - 분리된 `serve`가 떠 있도록 보장한 뒤 대시보드 없이 종료
3. **tui**
    - 실시간 대시보드만 엶. 명령을 아예 생략하면 분리된 `serve` 실행을 먼저 보장한 뒤 대시보드를 엶
4. **index**
    - 세션 기록을 한 번 색인하고 JSON 요약을 출력: 검사·변경 파일 수, 읽은 byte, 경과 밀리초, 행 수
5. **report**
    - 일일 리포트를 쓰고 경로를 출력하며, `--date YYYY-MM-DD`로 날짜를 고르고 기본값은 어제
6. **hook**
    - `hook <route>`는 command 훅 클라이언트: stdin을 `http://127.0.0.1:<훅 포트>/hook/<route>`로 보내고 응답을 출력
    - route는 `a-z`, `0-9`, `-`로 이루어진 한 구간이거나 `relay/<이름>`
    - 2xx 응답이면 종료 코드 0, 그 밖에는 1이며 stdin 상한은 16 MiB
    - route가 `pretool`로 시작하면 fail-closed: stdin 상한 초과, 전송 실패, 2xx 아님이면 거부 JSON을 출력하고 종료 코드 0이며, `relay/*` route는 종료 코드 1
7. **statusline**
    - `statusline [--wrap [--append] -- <명령> [인자...]]`는 Claude Code의 `statusLine` 명령: stdin의 statusLine JSON을 읽어 메타데이터만 `/hook/statusline`으로 보내고 한 줄 요약을 출력(Claude Code 연결 참고)
8. **status**
    - 관리 상태 JSON을 출력
    - `status --once [--warn <pct>] [--max-age <분>] [--state-file <경로>]`는 `serve` 없이 데이터베이스의 최신 한도 스냅샷을 판정해 종료 코드로 알림(확인 참고)
9. **doctor**
    - 확인 절에 설명한 항목을 출력하며, `pretool-chain`이 등록돼 있는데 훅 포트가 `/health`에 응답하지 않으면(또는 `hooks.enabled`가 false이면) 모든 Bash 호출이 막히므로 `fail`로 보고
10. **전역 옵션**
    - `--config <경로>`로 설정 파일을 지정하며(기본 `~/.config/claude-monitor/config.toml`), 설정 오류는 종료 코드 2, 그 밖의 오류는 1
    - `--version`과 `--help`는 clap 표준 옵션

---

## 대시보드

1. **탭**
    - 키 `1`-`8`의 여덟 탭: 개요, 도구, 훅, 모델, 에이전트, 설정(`6`), 리포트(`7`), 프로젝트(`8`)
    - 게이트웨이 탭은 없음: 게이트웨이 지연(첫 바이트, 첫 토큰, 전체 시간의 p50/p90)은 `GET /api/gateway`와 일일 리포트로 볼 수 있고, 개요의 서버 상태 카드는 게이트웨이 포트를 상태 점으로 표시
    - 배경 스레드가 2초마다 `cm.sqlite`를 읽기 전용으로 읽고, 무거운 패널(도구, 훅, 모델 통계, 에이전트 통계, 캐시 붕괴)은 10초마다 조회하며, 첫 수집, 구간 변경, `r` 새로고침은 즉시 조회
    - 화면은 변화가 있을 때만 다시 그리며, 최소 1초에 한 번은 그림
    - 탭 이름이 폭에 맞지 않으면 탭 바가 탭 사이 간격을 없애고, 더 좁으면 번호를 붙인 압축 이름으로 바뀌어 60열까지 여덟 탭이 모두 보이고 클릭됨
    - 화면 문구는 기본 영어이며 `L`로 한국어로 바꿀 수 있음(표시 언어 참고), 리포트 본문은 `config.toml`의 `[report] language`를 따름
    - 시작할 때 대시보드는 기존 데이터베이스를 쓰기 연결로 한 번 열어 옛 스키마를 올린 뒤 읽기 전용 수집을 시작하며, 이 작업은 첫 수집 전에 수집 스레드에서 실행되고 실패하면 오류 알림을 표시하며 데이터베이스를 새로 만들지는 않음
2. **키**
    - `1`-`8`, `Tab`, `Shift-Tab`: 탭 이동, 번호는 탭 바의 표시 순서(탭 순서 참고)
    - `<`, `>`: 현재 탭을 왼쪽·오른쪽으로 한 칸 이동(탭 순서 참고)
    - `Up`, `Down`, `PgUp`, `PgDn`, `Home`, `End`: 스크롤
    - `w`: 집계 구간을 오늘(로컬 자정부터, 기본값), 1시간, 24시간, 7일 순으로 전환
    - `s`, `S`: 포커스 표 카드의 다음 정렬 열(숫자·시간 열은 내림차순, 문자 열은 오름차순에서 시작)과 현재 열의 정렬 순환(처음 방향, 반대 방향, 카드 기본 정렬)
    - `Left`, `Right`: 리포트 날짜(주간 보기에서는 리포트 주), 리포트 탭에서만
    - `v`: 리포트 탭의 일간·주간 보기 전환
    - `g`: 리포트 생성(주간 보기에서는 보고 있는 주), 리포트 탭에서만
    - `d`: 귀속 차원(스킬, MCP 서버, 플러그인, 에이전트, 모델) 순환, 프로젝트 탭에서만
    - `i`: 재색인
    - `g`와 `i`는 관리 API를 거치므로 `serve`가 실행 중이어야 함
    - `t`: 항상 위 전환
    - `Shift+Up`, `Shift+Down`, `Shift+Left`, `Shift+Right`: 포커스 카드를 한 칸 이동(카드 순서 참고)
    - `l`: 현재 탭의 카드 크기, 카드 순서, 끌어 정한 열 폭 초기화
    - `L`(Shift+l): 표시 언어를 영어와 한국어로 전환
    - `r`: 새로고침, `?`: 도움말, `q`, `Esc`, `Ctrl+C`: 종료
3. **마우스**
    - 모든 탭이 테두리 카드들로 이루어짐: 탭 클릭으로 전환, 카드 클릭으로 포커스, 표 행 클릭으로 선택(상세는 옆 카드)
    - 표 머리글을 클릭하면 그 열로 정렬: 숫자·시간 머리글은 내림차순(큰 값·최근 먼저), 문자 머리글은 오름차순에서 시작하고, 같은 머리글을 다시 누르면 방향이 바뀌며 세 번째 클릭에 표의 기본 정렬로 복원하며, 이벤트 피드와 설정 카드 넷을 포함한 모든 표 카드에 같게 적용
    - 휠은 포인터 아래 카드를 스크롤하며, 스크롤 가능한 카드는 끌 수 있는 썸이 있는 스크롤바를 표시하고 트랙을 클릭하면 한 화면 이동하며, 본문이 한 줄뿐인 카드도 거터에 한 칸 썸과 ` a-b / total ` 위치 표시를 보이지만 끌 곳은 없음
    - 카드 사이 경계를 끌어 카드 크기를, 머리글 두 칸 사이 경계를 끌어 열 폭을 조절하며(이벤트 피드와 설정 카드를 포함한 모든 표 카드), 열 경계 더블클릭이나 `l`로 자동 배치로 복원하고 배치는 상태 파일에 저장됨(상태 파일 참고)
    - 열 경계는 평소 흐린 표시를 보이고 위에 올리면 전체 손잡이가 나타나며, 경계 칸 자체나 양쪽 한 칸까지 잡을 수 있고, 끌지 않은 채 경계 옆 머리글 칸을 그냥 클릭하면 버튼을 뗄 때 그 열로 정렬됨
    - 카드 제목을 400 ms 길게 누른 채 끌면 카드 위치를 옮김(카드 순서 참고)
    - 탭을 누른 채 탭 바를 따라 끌면 탭 순서를 바꿈(탭 순서 참고)
    - 창 밖에서 놓아 버튼 뗌이 사라져도 모든 끌기가 뗌과 같게 끝남: 버튼 없는 첫 움직임이나 다음 누름이 움직인 카드 경계, 열 폭, 스크롤바 끌기를 확정하고(배치도 저장) 탭 전환도 버리지 않고 확정
    - 하단 줄에 재색인, 구간, 새로고침, 항상 위, 배치 초기화, 언어, 도움말, 종료 버튼이 있으며, 리포트 탭에는 이전·다음 날짜와 생성 버튼이 더 있음
4. **화면 구성**
    - 이벤트 피드는 고정 머리글이 있는 표 카드(시각, 종류, 이름, 소요, 에이전트, 입력, 출력, 캐시읽기)이며 다른 표와 같은 열 규칙을 따름: 글자 열은 `…`보다 자연 폭을 먼저 갖고, 글자 열이 숫자 열보다 먼저 줄고, 오른쪽 열은 마지막에 숨으며, 이름과 에이전트 열은 그 전에 읽을 수 있는 최소 폭을 지킴. 훅 행은 이름 칸에 경로·가드·상세를, 에이전트 칸에 판정을 보여 줌
    - 설정 탭은 훅, MCP 서버, 에이전트, 진단을 같은 머리글 정렬과 열 드래그가 있는 표 카드로 보여 주며, 허용 목록, 적재 경고, 스킬·env 요약은 표 행 아래 같은 스크롤에 보조 줄로 이어지고 진단의 해결 힌트는 내용 칸에 붙음
    - 높이가 한 줄뿐인 카드는 머리글 대신 첫 본문 줄을 보이며 정렬은 `s`, `S`로 계속 쓸 수 있음
    - 최상단 고정 상태는 앱 바의 `최상단` 배지와 `[t] 최상단 ON` 칩에 낱말과 함께 전용 보라색 채움을 쓰며, 경고색은 지연·경고 상태 전용
    - 개요에는 추이 줄 오른쪽 끝에 한도 카드가 있음(낮은 화면에서는 숨김): 숫자가 상태 줄에서 왔으면 `공식`, 로컬 토큰 합계뿐이면 `추정`이라고 표시하고, 이어서 초기화 카운트다운이 붙은 5시간·7일 막대, 페이스 표시(균등 소모 속도보다 빠름, 비슷함, 느림), 소진 예상 시각, 컨텍스트 퍼센트, 컴팩션 횟수, 게이트웨이가 `anthropic-ratelimit-*` 헤더를 본 경우 API 키의 남은 요청·토큰을 보여 줌
    - 30분보다 오래된 상태 줄 스냅샷은 흐리게 표시하고 소진 예상을 하지 않음
    - 좁은 화면(추이 카드가 12열 미만이 되는 경우)에서는 개요의 추이 카드 두 장이 숨겨지고 한도 카드가 넓어짐
    - 한도와 API 키 수준에는 색과 함께 표식이 붙음: 사용률 80 % 이상(경고)은 ` !`, 95 % 이상(위험)은 ` !!`이며, 캐시 타이머 카드의 캐시 적중 줄은 80 % 미만에 ` !`, 50 % 미만에 ` !!`를 붙임
    - 모델 탭은 모델별 통계(요청, 세션, 에이전트 종류, 토큰, 캐시 적중률, 대기 p50/p90, 추정 비용, 마지막 시각)와 상세 카드, 캐시 붕괴 후보, 최신 상태 줄 스냅샷으로 만든 캐시 타이머 카드를 보여 주며, 캐시 타이머 카드는 카드가 너무 낮으면 놓인 자리와 상관없이 숨겨짐
    - 프로젝트 탭은 프로젝트·브랜치 표(요청, 세션, 토큰, 5분·1시간 캐시 쓰기, 추정 비용, 마지막 시각)와 상세 카드, 스킬·MCP 서버·플러그인·에이전트·모델별 비용 귀속 표를 보여 주며, `d`로 차원을 순환하고 선택은 상태 파일에 저장되며(상태 파일 참고), 이 표는 OTLP 메트릭이 필요함(Claude Code 연결 참고)
    - 에이전트 탭은 에이전트 종류별 통계(실행, 요청, 세션, 토큰, 추정 비용, 대기 p50/p90, 도구 호출·오류, 주 모델, 마지막 시각)와 상세 카드를 보여 줌
    - 리포트 보기는 박스 표를 그대로 보여 주며, 예전 파이프 표 리포트도 표시할 때 박스 표로 바꿈
    - 리포트 탭은 오늘 리포트(실시간으로 만듦)로 열리며, `v`로 일간·주간 보기를 전환하고 주간 보기는 현재 주(역시 실시간으로 만듦)로 열리며 `Left`, `Right`로 한 주씩 이동함(현재 주 너머로는 가지 않음)
    - 대시보드의 오늘은 자정을 넘기면 시계를 따라감: 오늘이나 이번 주를 보던 리포트 보기는 새 날·새 주로 옮겨 그곳에서 실시간으로 만들고, 지난 날·지난 주를 보던 경우는 그대로 둠
    - 이벤트 피드는 저장된 OTLP 이벤트(컴팩션, 서브에이전트 완료, API 오류, 권한 결정, 권한 모드 변경)도 각자의 태그로 보여 줌
    - 서버 상태 카드는 포트마다 점으로 표시: 열려 있으면 채운 녹색 점, 꺼 두었거나 알 수 없으면 빈 점, 열려야 하는데 닫혀 있으면 굵은 적색 빈 점이며, 카드가 좁으면 항목이 오른쪽부터 통째로 빠짐
    - 카드 제목이 카드 폭보다 길면 가운데 구간부터 덜어내 첫 구간과 정렬 표시를 남기고, 테두리 보조 문구는 제목보다 먼저 빠짐
5. **콘솔**
    - 클래식 콘솔 창에서는 대시보드가 실행되는 동안 `D2Coding ligature` 글꼴을 적용하고 종료할 때 원래 글꼴로 복원
6. **카드 순서**
    - 칸 하나가 카드 한 장인 모든 카드 그룹에 적용: 개요의 KPI·추이·피드 줄, 그리고 도구, 훅, 모델, 에이전트, 설정, 리포트 탭
    - 카드 제목을 400 ms 길게 누르면 이동 모드로 들어가며, 들어 올린 카드는 강조색 점선 테두리가 되고 제목에 이동 표시가 붙음
    - 끌면 새 순서를 미리 보여 주고 놓으면 확정: 칸 크기는 그대로 두고 카드만 칸 사이를 옮겨 감
    - `Esc`, 탭 전환, 터미널 크기 변경은 이동을 취소하고 이전 순서로 복원
    - 400 ms 전에 놓으면 일반 클릭이고, 400 ms 전에 끌면 이동도 크기 조절도 시작하지 않음
    - `Shift+방향키`는 끌지 않고 포커스 카드를 한 칸 옮김: 세로로 쌓인 그룹은 `Up`, `Down`, 가로로 나란한 그룹은 `Left`, `Right`만 받고 나머지 방향키는 무시
    - `l`은 현재 탭의 카드 크기, 카드 순서, 끌어 정한 열 폭을 초기화
7. **탭 순서**
    - 탭을 누른 채 탭 바를 따라 끌면 누르는 순간 그 탭으로 전환하고, 끄는 동안 새 순서를 바로 보여 주며, 놓으면 확정하고 저장
    - 끌지 않은 클릭은 탭만 전환하고 순서는 바꾸지 않음
    - 끄는 중 `Esc`는 누르기 전 순서로 복원하고, 창 밖에서 놓아 버튼 뗔이 사라지면 다음 움직임이나 누름이 보이는 순서를 확정
    - `<`, `>`는 끌지 않고 현재 탭을 한 칸 옮기며 양 끝에서는 멈춤
    - 탭 번호는 보이는 순서를 따르므로 `1`-`8`, `Tab`, `Shift-Tab`은 항상 탭 바 순서를 따르며, `l`은 탭 순서를 초기화하지 않음
8. **표시 언어**
    - 기본은 영어이며 `L`(Shift+l)로 실행 중에 영어와 한국어를 전환하고 선택을 저장
    - 하단 칩 `[L] EN` 또는 `[L] KO`가 현재 언어를 보여 주며 클릭할 수 있고, 다른 왼쪽 칩 뒤와 도움말·종료 앞에 놓이며 왼쪽 칩 중 가장 마지막까지 남아 최소 크기(60x16)에서도 보이고 다른 왼쪽 칩은 오른쪽부터 사라짐
    - 탭 이름, 문구, 도움말, 알림은 언어를 따르며, 경로, 모델·도구 이름, 로그 문구 같은 데이터 값은 번역하지 않고 리포트 본문은 대신 `[report] language`를 따름
9. **상태 파일**
    - `config.toml` 옆의 `~/.config/claude-monitor/tui.toml`(데이터 폴더를 옮겨도 이 위치 유지)에 `version = 1`, `language`, `attribution_dimension`(`d`로 고른 프로젝트 탭 차원이며 기본값인 스킬이 아닐 때만 기록), `tab_order`(`overview`, `tools` 같은 고정 탭 이름이며 기본 순서와 다를 때만 기록), 카드 순서, 분할 비율, 표 열 폭을 `settings_rows`, `event_feed`, `tool_table`, `settings_hooks` 같은 고정 이름 키로 저장
    - 변경이 확정될 때(카드 이동, 탭 이동, 경계·열 드래그의 끝, `Shift+방향키`, `<`, `>`, `l`, `L`, 프로젝트 탭의 `d`) 임시 파일에 쓴 뒤 이름을 바꾸는 방식으로 원자적으로 기록
    - 쓰기는 입력 스레드가 채널로 보내는 전용 저장 스레드가 맡음: 연달아 확정하면 가장 최근 상태 하나로 합쳐지고, 터미널 가드가 어느 종료 경로에서든 마지막 확정을 쓴 뒤 스레드를 합류시키되 최대 5초만 기다리고 그래도 끝나지 않으면 stderr에 경고를 남기고 종료하며, 쓰기에 실패하면 오류 알림이 뜸
    - `tui.toml`이 심볼릭 링크(예: 동기화 폴더로 연결)이면 링크 사슬을 끝까지(최대 16단계) 풀어 실제 파일 옆에 임시 파일을 쓰고 실제 파일 위로 이름을 바꾸므로 중간 링크는 그대로 유지되고, 사슬이 16단계를 넘으면 저장이 오류로 끝남
    - 시작할 때는 오래된 임시 파일(10분 넘게 수정되지 않았고 실행 중인 프로세스 것이 아닌 것)만 지우므로 다른 실행 인스턴스가 지금 쓰는 임시 파일은 남음
    - 파일이 없으면 기본값(영어와 기본 배치)
    - 읽을 수 없는 파일, 지원하지 않는 버전, 맞지 않는 항목(모르는 이름이나 길이·범위 불일치)은 알림 한 번과 함께 건너뛰고 나머지 항목은 그대로 적용하며, 읽을 수 없거나 다른 버전인 파일은 먼저 `tui.toml.bak`으로 복사
    - 네 번째 카드가 생기기 전에 저장한 `overview_trend_cols`·`models_rows` 배치는 알림 없이 불러옴: 순서는 유지하고 새 카드를 맨 뒤에 붙이며 새 경계는 끝에 두고, 다음 저장이 4칸 형식으로 씀

---

## 포트와 엔드포인트

### 1. 관리 47330

1. **읽기 엔드포인트**
    - `GET /health`는 `ok`, `GET /api/status`는 상태 JSON을 반환
    - `GET /api/overview`, `tools`, `hooks`, `models`, `projects`, `gateway`, `cache-breaks`, `minutes`는 집계를 반환하며, `?hours=`로 구간을 지정(기본 24, 1-720으로 제한)
    - `GET /api/projects`는 프로젝트·git 브랜치마다 한 행을 반환: 프로젝트는 작업 폴더의 마지막 폴더이고 `<project>/.claude/worktrees/<name>`은 `<project>`로 셈하며 전체 경로는 저장하지 않음
    - `GET /api/limits`는 최신 상태 줄 스냅샷(`latest`)과 구간의 스냅샷(`history`, 최신 100행까지, 오래된 것부터)을 반환하고, `GET /api/ratelimit`은 구간의 게이트웨이 레이트리밋 스냅샷을 반환(최대 100행, 최신 먼저)
    - `GET /api/model-stats`는 모델마다 한 행(기록 출처만)을 반환: 요청, 세션, 에이전트 종류, 토큰 합, 캐시 적중률, 대기 p50/p90, 추정 비용, 마지막 시각이며 `?hours=`는 위와 같음
    - `GET /api/agent-stats`는 에이전트 종류(`#` 앞 라벨, 메인 스레드는 `main`)마다 한 행을 반환: 실행(서로 다른 에이전트 인스턴스, `main`은 세션 수), 요청, 세션, 토큰 합, 캐시 적중률, 대기 p50/p90, 추정 비용, 도구 호출·오류, 주 모델, 마지막 시각이며 `?hours=`는 위와 같음
    - `GET /api/report?date=YYYY-MM-DD` 또는 `GET /api/report?week=YYYY-Www`(ISO 주)는 보관된 리포트를 Markdown으로 반환하고, 올바른 날짜나 주가 없거나 둘 다 주면 `400`, 보관된 것이 없으면 `404`
    - `GET /api/events`는 실시간 이벤트 Server-Sent Events 스트림이며 15초마다 `: ping` 주석을 보냄
    - `GET /api/power`는 `enabled`, `keep_awake`, `holding`(절전 유예 활성), `gaps`를 반환하며, `gaps`는 최근 대기 구간 32개(`serve`를 다시 시작해도 남음)로 `id`, `start_ms`, `end_ms`(Unix 밀리초), `active`(대기 직전 Claude Code가 작업 중이었음)를 가짐
2. **토큰 보호 엔드포인트**
    - `POST /api/reindex`는 기록 색인을 한 번 실행
    - `POST /api/report?date=YYYY-MM-DD` 또는 `POST /api/report?week=YYYY-Www`는 일일 또는 주간 리포트를 생성해 보관하며 기본값은 어제 일일 리포트
    - 모든 `POST`는 헤더 `x-cm-token`에 토큰이 필요하며, 없거나 틀리면 `401`
3. **토큰**
    - 토큰은 OS가 주는 무작위 32바이트이며 hex로 데이터 폴더의 `admin.token`에 기록되고 `serve`를 시작할 때마다 다시 만듦

### 2. OTLP 47331

1. **경로**
    - `POST /v1/logs`, `/v1/metrics`, `/v1/traces`
2. **받는 입력**
    - `http/json`(`application/json`)과 `http/protobuf`(`application/x-protobuf` 또는 `application/protobuf`): 다른 content type은 `415`와 힌트 `use OTEL_EXPORTER_OTLP_PROTOCOL=http/json or http/protobuf`를 받음
    - `gzip` content encoding을 받으며, 본문 상한은 16 MiB이고 압축을 푼 뒤에도 적용
    - 잘못된 protobuf 본문이나 해석할 수 없는 메트릭 본문은 `400`
3. **처리**
    - 로그는 세 이벤트(`api_request`, `tool_result`, `hook_execution_complete`)를 행으로 바꾸고, 여섯 이벤트 `compaction`, `subagent_completed`, `api_error`, `api_retries_exhausted`, `tool_decision`, `permission_mode_changed`는 `otel_events` 표에 저장하며 나머지는 무시
    - 메트릭(`claude_code.*`의 sum·gauge)은 점이 가진 스킬, MCP 서버, 플러그인, 에이전트, 모델과 함께 `metric_points` 행이 되며, 누적 계열은 증분으로 환산하고 히스토그램은 무시함. 요청 하나에서 최대 5000개 점만 취함
    - 식별 키(이름, 시각, 세션, 토큰 종류, 모델, 쿼리 출처, 에이전트, 스킬, 플러그인, MCP 서버·도구, temporality, 값)가 이미 저장된 점은 무시하므로 다시 보낸 배치는 두 번 세지 않으며, 누적 기준값은 적재가 커밋된 뒤에만 옮김
    - 트레이스는 개수만 셈
    - `user.*`와 `organization.*` 속성은 저장하지 않으며 프롬프트·응답 본문도 저장하지 않음
    - 변환은 Claude Code 2.1.328 실제 캡처(`tests/fixtures/otlp-live/`)로 확인했으며, 메트릭 귀속은 `OTEL_METRICS_EXPORTER=otlp`가 필요함

### 3. 게이트웨이 47332

1. **역할**
    - 기본 켜짐(`gateway.enabled = true`): Anthropic API(`gateway.upstream`, HTTPS만)로 가는 투명 프록시
    - `POST /v1/messages`의 첫 바이트, 첫 토큰, 전체 시간, 상태, 토큰 사용량(5분·1시간 캐시 쓰기 구분 포함)을 측정하며 다른 경로는 계측 없이 중계
2. **동작**
    - 인증 정보를 포함한 요청 헤더는 hop-by-hop 헤더와 `host`를 빼고 그대로 상류에 전달하며 어느 것도 저장하지 않음
    - 응답은 버퍼링 없이 흘려보내며, 상류 연결 오류는 `502` JSON 오류가 됨
    - 상류 프록시: `serve`는 시작할 때 `HTTPS_PROXY`(또는 `https_proxy`)와 `NO_PROXY`(또는 `no_proxy`)를 한 번 읽어 HTTP CONNECT 터널로 상류에 접속하며, 프록시 호스트와 포트만 출력하고 자격 증명은 출력하지 않음
    - `http://` 프록시 주소만 받음(스킴 생략은 http, 포트 생략은 80): 주소의 `user:password@`는 basic 인증이 되고, `https://`나 다른 스킴, 형식이 틀린 주소는 거부하므로 프록시가 필요한 요청은 몰래 직접 연결하지 않고 `502`를 받음
    - 루프백 대상과 `NO_PROXY`에 맞는 대상은 직접 연결: 쉼표 목록에서 항목이 호스트와 정확히 같거나 도메인 접미와 맞으면 되고(앞의 `.`나 `*.` 허용), `*`는 모든 호스트이며, 포트는 무시하고 IP 주소는 정확히 같아야 함
    - CONNECT 협상은 15초 뒤 시간 초과로 끝나며, 프록시 응답 헤더는 8 KiB까지만 받음
    - 응답 헤더가 `gateway.upstream_timeout_secs` 안에 오지 않으면 `504` JSON 오류를 반환하며, 스트리밍 본문에는 시한이 없음
    - 긴 스트림이 훅·색인과 경쟁하지 않도록 자체 런타임 스레드에서 실행
    - `gzip`·`deflate` 응답의 사용량은 압축을 푼 사본에서 읽고 클라이언트로 가는 바이트는 건드리지 않으며, `br`, `zstd`, 겹친 인코딩은 중계와 시간 측정만 하고 사용량은 읽지 않음
    - `anthropic-ratelimit-*`와 `retry-after` 응답 헤더, 그리고 헤더가 없어도 `429`를 레이트리밋 스냅샷으로 저장하며 다른 헤더는 읽지 않음

### 4. 훅 47333

1. **경로**
    - `POST /hook/pretool-chain`, `POST /hook/relay/{route}`, `POST /hook/standby-hint`, `POST /hook/statusline`, `GET /health`
2. **대기 안내 경로**
    - 페이로드에서 `hook_event_name`, `session_id`, `duration_ms`만 읽음
    - `duration_ms`가 있는 `PostToolUseFailure`는 대기 구간이 도구 실행과 겹치면 모든 해당 실패에 `additionalContext`로 응답
    - `UserPromptSubmit`은 `power.hint_window_secs` 안에 끝났고 Claude Code가 작업 중일 때(대기 전 `power.awake_grace_secs` 안의 게이트웨이 요청, 훅 호출, 열린 스트림) 시작된 구간에 대해 세션·구간마다 한 번 `additionalContext`로 응답
    - `duration_ms`가 없는 실패는 프롬프트 규칙을 따르되 조건부 안내를 받음: 오류가 시간 초과나 연결 끊김일 때만 대기 때문이라고 봄
    - 그 밖의 이벤트, 읽을 수 없는 본문, `power.enabled = false`, 맞는 구간 없음은 `{}`를 반환하며, 호출은 route `standby-hint`로 기록되고 안내를 담은 응답은 가드 라벨 `standby-hint`로 표시
3. **상태 줄 경로**
    - `claude-monitor statusline`이 뽑은 지표 JSON(한도, 컨텍스트, 캐시, 세션 비용)을 받아, 값이 바뀌었거나 그 세션의 직전 스냅샷 이후 60초 이상 지났을 때만 스냅샷을 저장
    - `{}`로 응답하며, JSON 객체가 아니거나 문자열 필드가 너무 긴 본문은 `400`이고 훅 이벤트로는 기록하지 않음
4. **중계 경로**
    - `{route}`는 `a-z`, `0-9`, `-`만 허용
    - 요청을 `{hooks.relay_target}/hook/{route}`로 그대로 전달하고 상류의 상태와 본문을 그대로 돌려줌
    - 판정, 가드 라벨, 소요 시간을 기록하며 상류에 닿지 못하면 `502`
5. **본문 상한**
    - 요청당 16 MiB

### 5. 공통 규칙

1. **바인딩**
    - 모든 포트는 `127.0.0.1`만 수신
2. **요청 검사**
    - `Host` 헤더는 `127.0.0.1:<포트>` 또는 `localhost:<포트>`여야 하며 그 밖에는 `403`
    - `Origin`이나 `Sec-Fetch-Site` 헤더가 있는 요청은 `403`이며, 브라우저에서 시작한 요청을 막음

---

## 훅 체인

### 1. 흐름

1. **단계**
    - `pretool-chain`은 구형 CLI 명령을 재작성한 뒤 페이로드를 `{hooks.relay_target}/hook/pretool-guard`(기본 `http://127.0.0.1:47321`)의 가드로 전달
    - 가드는 이 프로젝트에 포함되지 않으며 그 주소에서 이미 응답하고 있어야 함
2. **결과**
    - 가드에 닿지 못하거나 늦거나 2xx가 아니거나 JSON 객체가 아니면 사유 `[cm] pretool-guard 응답 없음 (fail-closed)`로 거부
    - 그 밖에는 가드의 판정이 유지되며 cm는 가드가 주지 않은 허용을 만들지 않음: 재작성된 명령에는 `updatedInput`과 변환 항목을 밝힌 `additionalContext` 메모가 붙음
    - 판정 경우의 전체 목록은 [ARCHITECTURE-ko.md](ARCHITECTURE-ko.md)에 있음
3. **기록 항목**
    - 결과(`allow`, `deny`, `rewrite`, `error`), 가드 라벨, 변환한 명령 이름, 소요 시간, 재작성 한 번당 4200 ms로 추정한 절약 시간

### 2. 재작성 규칙

1. **cat**
    - `cat <파일>`은 `bat --plain --paging=never <파일>`이 됨
    - 인수가 없거나, 인수가 `-`로 시작하거나, 리다이렉션이 있으면 그대로 둠
    - `>|`(noclobber 해제 리다이렉션)가 들어 있는 명령은 재작성하지 않음
2. **ls**
    - 모든 플래그가 `-l -a -la -al -lh -lah -alh -1 -A -h` 중 하나이면 `ls`는 `eza`가 되며, `h`는 빼고 `A`는 `a`로 바꾸고 경로가 없으면 `.`을 붙임
3. **eza**
    - 플래그만 있는 경로 없는 `eza`에는 ` .`을 붙임
4. **git diff**
    - `git diff`에는 ` | delta`를 붙임
    - 요약 플래그(`--stat`, `--shortstat`, `--numstat`, `--name-only`, `--name-status`, `--quiet`, `--exit-code`, `--check`, `--summary`, `--no-patch`, `-s`), `--no-index`, 리다이렉션, 기존 파이프, 바로 뒤의 `&&`나 `||`가 있으면 그대로 둠(파이프가 종료 코드를 바꾸므로)
5. **그 밖의 경우**
    - 명령은 `&&`, `||`, `;`, `|`로 나누어 부분마다 따로 변환
    - 변환할 수 없는 `cat`이나 `ls` 부분이 하나라도 있으면 명령 전체를 그대로 둠
    - 개행, heredoc, 백틱, `$(`, `$'`, `${`, `[[`, `case`, 프로세스 치환, 짝이 안 맞는 따옴표, 주석, `&`, 괄호, 따옴표 없는 중괄호, 비 ASCII 공백, 따옴표 없는 비 ASCII 문자, 끝의 역슬래시도 그대로 둠
    - 플래그는 따옴표를 벗긴 값으로 판단하므로 `ls "-s" src`는 지원하지 않는 플래그 `-s`로 셈
    - `grep`, `find`, 그 밖의 명령은 재작성하지 않으며 가드는 항상 최종 명령을 다시 검사
    - 재작성된 명령은 Claude Code를 실행하는 PC의 PATH에 `bat`, `eza`, `delta`가 있어야 함

---

## 데이터와 개인정보

### 1. 데이터 폴더

1. **위치**
    - `~/.config/claude-monitor/` 폴더 하나에 프로그램이 보관하는 모든 파일이 있음
    - `~`는 `USERPROFILE`이며 `USERPROFILE`이 없거나 비어 있으면 `HOME`이고, Windows에서는 `%USERPROFILE%\.config\claude-monitor\`
    - `CM_DATA_DIR` 또는 `config.toml`의 `data_dir`은 `config.toml`을 뺀 모든 파일을 다른 폴더로 옮기며, 설정 파일은 자기 경로에 그대로 있음
2. **내용**
    - `config.toml`: 선택 사항인 설정 파일이며 사용자가 만들고 프로그램은 읽기만 함
    - `cm.sqlite`: WAL 모드 데이터베이스이며 열려 있는 동안 `cm.sqlite-wal`과 `cm.sqlite-shm`이 옆에 생김
    - `admin.token`: 현재 `serve` 실행의 관리 토큰
    - `serve.log`: 명령 없는 실행이나 `start`가 만드는 분리 `serve`의 출력이며 실행마다 새로 만듦
    - `reports\YYYY-MM-DD.md`와 `reports\YYYY-Www.md`: 일일·주간 리포트(ISO 주, 예: `2026-W47`)이며 데이터베이스에도 함께 보관
    - `tui.toml`: 대시보드 상태(표시 언어, 카드 순서, 카드 크기, 열 폭)이며 대시보드가 기록하고 파일이 없으면 기본값
3. **생성**
    - 폴더는 `serve`, `start`, `index`처럼 파일을 쓰는 첫 명령이 필요할 때 만들어지며, `doctor`는 폴더가 없으면 경고만 함

### 2. 저장하는 데이터

1. **메타데이터만**
    - 모델·에이전트 이름, 도구 이름, 세션 id, 프로젝트 이름(작업 폴더의 마지막 폴더이며 전체 경로는 저장하지 않음), git 브랜치 이름, 시각, 소요 시간, 5분·1시간 캐시 쓰기 구분이 있는 토큰 수
    - 리셋 시각이 붙은 한도·컨텍스트 퍼센트, 프롬프트 캐시 상태, 레이트리밋 헤더 값, 대기 구간(최신 1000건), OTLP 47331 절에 적은 OTLP 이벤트·메트릭 필드
    - 훅 판정: route, 가드 라벨, 결과, 감지한 구형 명령 이름
2. **저장하지 않는 것**
    - 프롬프트 본문, 명령 본문, 파일 내용, 도구 입출력, 인증 헤더, OTLP 속성 `user.*`와 `organization.*`
    - 기록 판독기는 파싱하는 동안 도구 결과의 앞 400자까지만 메모리에 두며, 오류 결과에서 가드 라벨과 명령 이름을 읽는 데만 쓰고 그 둘만 기록
3. **설정 점검**
    - `doctor`와 설정 탭은 필요할 때 `settings.json`과 `.claude.json`을 읽으며, env 값은 비교만 하고 출력하거나 저장하지 않음
4. **중복**
    - 같은 요청이 기록, OTLP, 게이트웨이로 모두 들어올 수 있어 집계는 출처마다 목적에 맞게 셈하며, 키와 규칙은 [ARCHITECTURE-ko.md](ARCHITECTURE-ko.md)에 있음

---

## 설정

### 1. 결정 방식

1. **순서**
    - `--config <경로>`, `CM_CONFIG`, `~/.config/claude-monitor/config.toml`, 기본값 순
    - `~`는 `USERPROFILE`이며, `USERPROFILE`이 없거나 비어 있으면 `HOME`이고, Windows에서는 `%USERPROFILE%\.config\claude-monitor\config.toml`
    - 그 경로만 찾으므로 `CM_DATA_DIR` 폴더에서는 설정 파일을 찾지 않으며, 파일은 명령이 시작될 때 읽으므로 수정한 뒤에는 `serve`를 다시 시작
2. **엄격성**
    - 알 수 없는 키는 오류이며, 포트는 0이 아니고 서로 달라야 하고, `report.run_hour`는 0-23, `report.language`는 `"ko"` 또는 `"en"`이어야 함
    - `[pricing.models]` 항목은 `input`과 `output`이 필요하고, 모든 단가는 0 이상의 유한한 수여야 하며, 앞뒤 공백을 자르고 앞의 `claude-`를 뺀 뒤 비어 있는 키는 거부됨
    - `hooks.relay_target`은 cm 자신의 포트가 아닌 포트의 `http` 루프백 주소(`127.0.0.1`, `localhost`, `[::1]`)만 허용하며, 그 밖에는 설정 오류라 시작에 실패
    - `hooks.guard_deadline_ms`는 1에서 4500 사이여야 하며, `defer` 재검사의 두 번 가드 호출도 10초 훅 제한 안에 들어옴
    - `power.gap_threshold_secs`는 15 이상이어야 하며, 5초 감시 주기와 그 흔들림이 대기 구간으로 잡히지 않음
3. **데이터 폴더**
    - `config.toml`의 `data_dir`, `CM_DATA_DIR`, `~/.config/claude-monitor` 순이며, 마지막은 기본 설정 파일과 같은 폴더
    - `data_dir`, `CM_DATA_DIR`, `USERPROFILE`, `HOME`이 모두 없으면 설정 오류로 시작에 실패
4. **Claude 홈**
    - `config.toml`의 `claude_home`, `CLAUDE_CONFIG_DIR`, `%USERPROFILE%\.claude`, `$HOME/.claude`(Linux·macOS) 순
5. **예시 파일**
    - 저장소의 `docs/config.example.toml`을 `~/.config/claude-monitor/config.toml`로 복사해 수정함: 같은 파일이 crates.io 패키지에도 들어 있음

### 2. config.toml

1. **키**
    - 모든 키는 선택이며 아래 블록이 기본값이고, `docs/config.example.toml`에도 같은 키가 있음
    - `gateway.enabled`의 기본값은 `true`이며 `false`이면 포트 `47332`를 건드리지 않음
    - `gateway.upstream_timeout_secs`는 상류 응답 헤더 대기 시간을 제한하며(시간 초과 시 504), 헤더가 온 뒤 스트리밍 본문에는 시한이 없음. 1 이상이어야 함
    - `report.language`(`"ko"` 또는 `"en"`, 기본 `"ko"`)는 일일·주간 리포트 본문의 언어를 정함
    - `[pricing.models."<접두어>"]`는 비용 추정 단가를 덮어쓰며(비용 추정 절 참고), 아래 블록에서는 주석 처리되어 있고 명령이 시작될 때 읽음
    - `power.enabled`는 대기 감시 스레드를 시작하며, `power.keep_awake`는 게이트웨이 스트림이 열려 있거나 `power.awake_grace_secs` 안에 게이트웨이 요청이나 훅 호출이 있는 동안 Windows에 시스템 절전 유예를 요청(`SetThreadExecutionState`의 `ES_SYSTEM_REQUIRED`, `ES_DISPLAY_REQUIRED`는 쓰지 않음)
    - 감시 주기 두 번 사이에 벽시계가 `power.gap_threshold_secs`보다 크게 도약하면 대기 구간으로 기록하며, `power.hint_window_secs`는 구간이 끝난 뒤 프롬프트가 안내를 받는 시간

```toml
# data_dir = "C:/Users/<user>/.config/claude-monitor"
# claude_home = "C:/Users/<user>/.claude"

[ports]
admin = 47330
otlp = 47331
gateway = 47332
hooks = 47333

[gateway]
enabled = true
upstream = "https://api.anthropic.com"
upstream_timeout_secs = 900

[hooks]
enabled = true
relay_target = "http://127.0.0.1:47321"
rewrite_enabled = true
guard_deadline_ms = 3000

[indexer]
enabled = true
# projects_dir = "C:/Users/<user>/.claude/projects"
interval_secs = 30
backfill_days = 7

[report]
enabled = true
# dir = "C:/Users/<user>/.config/claude-monitor/reports"
run_hour = 0
language = "ko"

[power]
enabled = true
keep_awake = true
awake_grace_secs = 600
gap_threshold_secs = 60
hint_window_secs = 1800

# [pricing.models."opus-5-5"]
# input = 4.0
# output = 20.0
# cache_write_5m = 5.0
# cache_write_1h = 8.0
# cache_read = 0.2
```

2. **리포트 예약**
    - 시작할 때 `serve`는 첫 색인 실행을 기다린 뒤(최대 10분, 초과하면 warn 알림) 어제 리포트를 다시 만들며 보관된 것을 교체
    - 이후 매일 현지 시각 `run_hour:05`에 전날 리포트를 씀
    - 주간 리포트는 ISO 주(현지 시각 월~일) 하나를 그 전 주와 비교해 다루며 `YYYY-Www`로 보관되고, 예약된 일일 실행마다와 시작할 때 지난주 리포트가 없거나 그 주가 끝나기 전에 보관된 것(예를 들어 `g`로 만든 부분 본)이면 다시 만들고, 월요일 실행은 항상 덮어씀
    - 일일 리포트는 현지 달력 하루를 다루며, 구성은 요약, 전일 대비, 모델·에이전트 사용량과 추정 비용, 도구 지연 상위, 훅 판정, 캐시 붕괴 상위, 있으면 게이트웨이 시간, 데이터가 있으면 컴팩션·컨텍스트 절, 규칙 기반 제안
    - 리포트 파일의 모든 표는 절 제목 아래 `text` 울타리 안의 선 박스 표(`┌─┬─┐`)이며 한글과 CJK는 2칸으로 셈하고 파이프 표는 없음

### 3. 비용 추정

1. **단가**
    - 추정은 100만 토큰당 USD의 내장 모델별 요금표를 쓰며, 모델 이름 접두어로 맞추고 가장 긴 접두어가 이김(예: `opus-5-5`가 `opus-5`보다 먼저)
    - 모델마다 입력, 출력, 5분 캐시 쓰기, 1시간 캐시 쓰기, 캐시 읽기 단가가 따로 있으며, 캐시 읽기 단가가 모든 모델에서 입력의 같은 배수는 아님
    - 표는 opus, sonnet, haiku, fable 계열을 다루며 `sonnet-4-5`와 `fable-5-1`도 포함
    - 모든 단가는 2026-11-19에 공식 가격 페이지로 확인했음: `opus-4`(Opus 4와 4.1)는 입력 $15, 출력 $75에 표준 캐시 배수를 쓰고, Opus 4.5~4.8은 각자 $5와 $25 항목이 있으며, `sonnet-4`(Sonnet 4, 날짜가 붙은 ID 포함)는 $3와 $15
    - `haiku-5-5`는 프롬프트(입력 + 캐시 읽기 + 캐시 쓰기)가 100k 토큰을 넘는 요청에 더 높은 단가 구간을 쓰며, 구간은 요청마다 판단
    - 어디에도 맞지 않는 모델은 비용 0으로 셈
2. **덮어쓰기**
    - `[pricing.models."<접두어>"]` 표는 이름이 키로 시작하는 모든 모델의 내장 단가를 교체하며(`claude-` 접두사 없이 적고 가장 긴 키가 이김) `input`과 `output`이 필수
    - 생략한 `cache_write_5m`, `cache_write_1h`, `cache_read`는 `input`의 1.25배, 2배, 0.1배가 됨
    - 목록에 있는 모델은 단일 단가를 쓰므로 `haiku-5-5`의 100k 초과 구간은 더 이상 적용되지 않음

---

## 측정

1. **범위**
    - 2026-11-15 작성자 PC에서 잰 값이며 보증이 아님: 다른 PC와 작업 부하에서는 달라짐
2. **기록 소급 색인**
    - 7일 소급 색인이 release 빌드에서 파일 453개, 602 MB를 1.7초에 읽음
3. **훅 체인 부담**
    - `pretool-chain`은 가드만 쓸 때보다 Bash 호출당 약 1.6 ms를 더함: 재사용 연결에서 3.6 ms 대 2.0 ms
4. **거부**
    - 작성자의 7일 기록에서 구형 CLI 가드 거부가 266건이었고, 건마다 모델 왕복이 중앙값 4.2초 더 걸림
    - 재작성 규칙은 `cat`과 `ls`만 다루고 `grep`은 다루지 않으므로 `grep` 거부는 남음
5. **네트워크**
    - API 왕복은 약 190 ms이며 로컬 프록시로 줄일 수 없음
6. **이득의 출처**
    - 속도 이득은 피한 거부와 피한 훅 프로세스 생성에서 나오며 네트워크 기법에서 나오지 않음
    - 대시보드의 절약 시간은 추정값: 재작성 횟수에 4200 ms를 곱한 값
    - 게이트웨이는 첫 바이트와 첫 토큰을 재기 위해 있으며 자체 추가 지연은 측정하지 않음

---

## 빌드와 테스트

1. **release 빌드**
    - `cargo build --release`가 `target\release\claude-monitor.exe`를 만들며, 툴체인 참고는 설치 절에 있음
    - 아이콘은 `assets/icon`에서 오며, 무손실 WebP 원본 두 개는 `claude-monitor.webp`(40 px 이상용 1024 px 타일)와 `claude-monitor-small.webp`(32 px 이하용 256 px 육각형 크롭)임
    - 다중 크기 `claude-monitor.ico`는 `python assets/icon/build_icon.py`로 다시 만들며 ImageMagick이 필요하고, `build.rs`가 빌드할 때 `.ico`를 버전 정보와 manifest와 함께 실행 파일에 삽입하며, crates.io 패키지는 `.ico`는 담고 WebP 원본과 스크립트는 빼둠
2. **검사**
    - 변경할 때마다 저장소 루트에서 실행

```bat
cargo clippy --all-targets -- -D warnings
cargo test
cargo fmt --check
```

3. **테스트 구성**
    - 단위 테스트는 모듈 옆 `#[cfg(test)]`, 또는 `tests.rs`, `test_support.rs` 파일에 있음
    - `tests/fixtures`에는 테스트가 읽는 fixture Claude 홈, 기록, OTLP 페이로드가 있음
4. **범위**
    - 재작성 표, 가짜 가드를 쓴 체인(허용, 거부, 시한, 재작성, 재검사), hook 클라이언트 fail-closed 출력, 임시 포트의 훅 서버
    - 관리 토큰과 집계, OTLP 변환과 크기 상한, 게이트웨이 502·504 경로, SSE 사용량 파서, 색인기
    - 포트 바인딩과 serve 감독 규칙, doctor 점검, 리포트 흐름, 대시보드 렌더링과 마우스 처리, 대기 감시

---

## 한계

1. **게이트웨이**
    - 상류 프록시 지원은 `HTTPS_PROXY`나 `https_proxy`로 지정한 `http://` 프록시뿐이며, `https://` 프록시, `ALL_PROXY`, `NO_PROXY`의 CIDR 항목은 지원하지 않음
    - `br`나 `zstd`(또는 겹친 인코딩)로 압축된 응답은 중계와 시간 측정은 되지만 토큰 사용량은 읽지 않으며, `gzip`과 `deflate`는 읽음
2. **Windows 네트워크**
    - 작성자의 Windows 호스트에서 `Connection: close`를 쓰는 HTTP/1.1 요청이 응답을 가끔 잃어(RST) 내장 블로킹 클라이언트는 그 헤더를 쓰지 않으며, 원인은 미확인
3. **OTLP**
    - 로그·메트릭 변환은 Claude Code 2.1.328 실제 캡처 하나로만 확인했으며 다른 Claude Code 버전은 검증하지 않음
    - 메트릭 귀속(스킬, MCP 서버, 플러그인, 에이전트, 모델)은 `OTEL_METRICS_EXPORTER=otlp`가 필요하며, 없으면 프로젝트 탭의 귀속 표가 비어 있음
    - `DISABLE_TELEMETRY=1`은 OTLP 내보내기를 막지 않음 (2026-11-16 실측: 설정 상태에서 `otlp_batches` 증가)
4. **가드 의존**
    - 체인은 `hooks.relay_target`에 기존 가드 서버가 있어야 하며, 없으면 모든 Bash 호출을 거부
5. **부분 기동**
    - OTLP나 게이트웨이 포트를 바인딩하지 못하면 `serve`는 그 서버 없이 경고만 출력하고 실행하므로 `doctor`와 기동 로그를 확인함
    - 실행 중에 OTLP, 색인기, 리포트 스케줄러, 게이트웨이가 끝나면 알림만 하고, 관리나 훅이 끝나면 `serve`가 멈춤
6. **언어와 플랫폼**
    - 리포트 본문은 한국어 또는 영어(`[report] language`, 기본 한국어)이고 대시보드 문구는 영어 또는 한국어(`L`)이며, 기본 데이터 폴더는 `USERPROFILE`, `HOME`, `CM_DATA_DIR` 중 하나가 필요함
7. **비용 추정**
    - 내장 모델별 요금표(비용 추정 절 참고)를 쓰며 모르는 모델은 0으로 셈하고, 표는 비교용 참고값이지 청구 금액이 아님
    - 5분·1시간 구분 없이 저장된 행(예: 옛 행)은 캐시 쓰기 전체를 5분 단가로 셈
8. **읽기 엔드포인트**
    - 관리 포트의 `GET` 엔드포인트는 토큰이 필요 없으며, 메타데이터만 반환하고 Host·Origin 검사를 거침
9. **대기**
    - 정책으로 화면이 꺼지는 최신 대기(S0) PC에서 Windows가 시스템 필요 요청을 지키는지는 미확인: 요청을 유지하던 중 대기 구간이 기록되면 알림과 `serve.log`에 `절전 유예 중`이 표시되며, 이는 요청이 대기를 막지 못했다는 뜻
    - 대기 구간 기록은 저장되고(최신 1000건) `serve`를 다시 시작하면 마지막 32건을 불러오며, 어떤 세션이 이미 안내를 받았는지는 저장되지 않으므로 재시작 뒤 세션이 안내를 한 번 더 받을 수 있음
    - 대기 전 `power.awake_grace_secs` 동안 게이트웨이 요청이나 훅 호출이 없는 긴 배경 작업은 유휴로 보므로, 그 구간 뒤의 프롬프트는 안내를 받지 못함
    - 벽시계가 `power.gap_threshold_secs`보다 크게 앞으로 도약하면(수동이나 NTP 보정) 대기 구간으로 기록되고, 뒤로 도약하면 다음 활동까지 절전 유예가 끝남
    - 대기 구간은 프로세스 전체가 멈췄을 때만 보이며, 대기 없는 네트워크 끊김은 감지하지 못함
10. **상태 줄**
    - 공식 5시간·7일 값은 `statusline` 명령을 거쳐서만 데이터베이스에 들어오므로, 없으면 한도 카드가 로컬 토큰 추정을 보여 줌
    - Claude Code는 Pro, Max, Ultra 구독에만, 세션의 첫 API 응답 뒤에만 이 값을 제공하며, 이 조건은 이 저장소에서 검증하지 않음
11. **보관 기간**
    - `limit_snapshots`와 `ratelimit_snapshots`는 30일을 보관하며 기록할 때 정리하고, 기준 시각은 기록 시각과 현재 중 이른 쪽에서 거슬러 세므로 미래 시각 한 건이 아무것도 지우지 못함
    - 대기 구간은 1000건으로 제한되고, `metric_points`와 `otel_events`에는 아직 보관 상한이 없어 계속 늘어남

---

## 라이선스

1. **Apache-2.0**
    - [LICENSE.md](LICENSE.md) 참고
