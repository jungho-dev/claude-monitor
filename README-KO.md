# rust-agents-proxy

## 개요

1. **소개**
    - Claude Code용 localhost 허브 역할을 하는 상시 실행 단일 Windows 실행 파일
    - `serve`가 여러 `127.0.0.1` 포트를 한 프로세스에서 실행하고, 배경 기록 색인기와 일일 리포트 스케줄러도 함께 돌림
    - 명령 없이 exe 를 실행하면 `serve`가 없을 때 분리된 배경 프로세스로 띄운 뒤 ratatui 실시간 대시보드를 열며, 대시보드를 닫아도 `serve`는 계속 실행
2. **하는 일**
    - 세션 기록(transcript), OTLP http/json 로그, API 게이트웨이 세 출처에서 사용량 메타데이터를 수집
    - SQLite 파일 하나에 저장하고, 로컬 관리 API, 대시보드, 일일 Markdown 리포트로 집계를 제공
    - Bash 훅의 구형 CLI 호출(`cat`, `ls`, `git diff`)을 가드가 거부하기 전에 재작성하고, 최종 명령은 기존 가드가 다시 검사
3. **특징**
    - 내장 SQLite(WAL)를 쓰는 단일 실행 파일이며 Node, Python, 데이터베이스 서버가 필요 없음
    - localhost 포트 4개: 관리 `47330`, OTLP `47331`, 게이트웨이 `47332`(기본 켜짐), 훅 `47333`
    - 메타데이터만 저장: 이름, 횟수, 소요 시간, 토큰 수, 가드 라벨이며 프롬프트 본문, 명령 본문, 파일 내용, 인증 헤더는 저장하지 않음
    - fail-closed 훅 체인: 가드가 응답하지 못하면 호출을 거부
    - 핫 경로(훅 체인, 수집, 색인, 대시보드)에서는 자식 프로세스를 띄우지 않으며, 유일한 생성은 명령 없이 실행했을 때 한 번 일어나는 분리 `serve` 기동
4. **문서**
    - 영어 원문: [README.md](README.md)
    - 소스 트리, 런타임 구성, 데이터 흐름, 보안 경계: [ARCHITECTURE-KO.md](ARCHITECTURE-KO.md)
    - 라이선스: [LICENSE.md](LICENSE.md)

---

## 경고

1. **fail-closed 등록**
    - `pretool-chain`을 `onFailure: "block"`으로 등록하면 `serve`가 꺼진 동안 모든 Bash 호출이 막힘
    - 가드가 `hooks.guard_deadline_ms` 안에 응답하지 않아도 체인이 호출을 거부함
2. **등록 전**
    - `serve`를 먼저 실행하고 `rust-agents-proxy doctor`를 돌려 `fail`과 `warn` 항목을 모두 해결함
    - `doctor`는 `fail` 항목이 있을 때만 종료 코드 1을 반환
3. **serve 상시 실행**
    - `.cmd/install-task.ps1`로 `serve`를 상시 실행: 로그온 시 작업 스케줄러, `conhost --headless`, 실패 시 1분 간격 재시작
    - 비정상 종료 후 작업이 `serve`를 다시 띄울 때까지는 Bash가 막힌 상태로 남음
4. **미확인**
    - `DISABLE_TELEMETRY=1`이 OTLP 내보내기를 막는지는 미확인이며 `doctor`도 그렇게 보고함

---

## 빠른 시작

### 1. 빌드

1. **빌드와 서명**
    - 미리 빌드한 배포본은 없으므로 소스에서 빌드하며, 툴체인은 소스에서 빌드 절에 정리함
    - `.cmd/build.cmd`가 release 빌드, 실행 중인 exe의 무중단 교체, Authenticode 서명을 수행

```bat
.cmd\build.cmd
```

2. **결과물**
    - exe는 `target\release\rust-agents-proxy.exe`에 생기며, `CARGO_TARGET_DIR`을 지정하면 그 아래에 생김
    - `libunwind.dll`을 exe 옆에 함께 둠: gnullvm 타깃이라 `build.rs`가 그 위치로 복사함

### 2. 실행

1. **그냥 실행**
    - exe 를 더블클릭하거나 명령 없이 실행
    - 관리 포트가 응답하지 않으면 `rust-agents-proxy serve`를 분리 프로세스로 띄우고, `/health`가 `200 ok`로 응답할 때까지 최대 8초 기다림. 이 응답은 저장소, 토큰, 런타임 준비가 끝난 뒤에만 나옴. 콘솔 창이 없고, Windows 가 허용하면 부모 Job 에서도 이탈함
    - 이어서 대시보드를 열며, 대시보드나 그 창을 닫아도 `serve`는 계속 실행
    - 두 번째 실행은 이미 떠 있는 `serve`를 그대로 쓰고 대시보드만 엶
    - 분리된 서버의 출력은 데이터 폴더의 `serve.log`에 기록되며 실행마다 새로 만듦

```bat
rust-agents-proxy
```

2. **서버만**
    - `serve`는 켜진 모든 포트와 배경 작업을 포그라운드로 실행하며, 작업 스케줄러나 디버깅용
    - 시작할 때마다 새 관리 토큰을 데이터 폴더에 기록
    - 기동 순서: 게이트웨이 상류를 먼저 검증하고, 관리, OTLP, 훅, 게이트웨이 순으로 포트를 바인딩하며, `admin.token`은 필수 포트 바인딩이 끝난 뒤에만 기록
    - 필수 포트는 관리와 훅(`hooks.enabled`일 때)이며, 하나라도 사용 중이면 `serve`는 오류로 종료
    - OTLP와 게이트웨이는 선택: 바인딩에 실패하면 경고를 출력하고 그 서버만 빼고 실행
    - `hooks.enabled = false`이면 훅 포트를 점유하지 않음

```bat
rust-agents-proxy serve
```

3. **대시보드만**
    - `tui`는 서버를 띄우지 않고 대시보드만 여며, 저장된 데이터는 볼 수 있지만 상태, 재색인, 리포트 생성은 `serve`가 필요함

```bat
rust-agents-proxy tui
```

### 3. Claude Code 등록

1. **진단**
    - `doctor`는 항목마다 `[level] topic: message`를 출력하고, 해결책이 있으면 `->` 줄을 붙임
    - 아래 스니펫은 기본 포트일 때 `doctor`가 출력하는 해결책과 같음

```bat
rust-agents-proxy doctor
```

2. **PreToolUse 가드 항목 교체**
    - Claude 홈(`%USERPROFILE%\.claude`, 또는 `CLAUDE_CONFIG_DIR`)의 `settings.json`에서 `hooks.PreToolUse`의 `^Bash$` 항목 중 `/hook/pretool-guard`를 가리키는 것을 아래 항목으로 교체
    - `onFailure`는 `block`, `timeout`은 `10`을 유지
    - 옛 가드 항목을 함께 두지 않음: 훅이 병렬로 실행되어 가드 거부가 재작성을 이기며, `doctor`가 그 조합을 경고함

```json
{"matcher":"^Bash$","hooks":[{"type":"http","url":"http://127.0.0.1:47333/hook/pretool-chain","timeout":10,"onFailure":"block"}]}
```

3. **훅 URL 허용**
    - `settings.json`에 `allowedHttpHookUrls` 목록이 있으면 아래 항목을 추가: 없으면 훅이 실행되지 않으며 `doctor`가 `fail`로 보고. 빈 목록은 모든 HTTP 훅을 막아 `fail`, 목록 자체가 없으면 제한 없음(`ok`)

```json
"allowedHttpHookUrls": ["http://127.0.0.1:47333/hook/*"]
```

4. **텔레메트리 환경 변수**
    - `env` 블록에 아래 키를 추가하면 Claude Code가 OTLP 로그를 수신기로 보냄

```json
"env": {
  "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
  "OTEL_LOGS_EXPORTER": "otlp",
  "OTEL_EXPORTER_OTLP_ENDPOINT": "http://127.0.0.1:47331",
  "OTEL_EXPORTER_OTLP_PROTOCOL": "http/json"
}
```

5. **게이트웨이 경유**
    - 게이트웨이는 기본 켜짐이므로 `serve`가 이미 포트 `47332`를 잡고 `gateway.upstream`으로 중계하며, `rap.toml`을 바꿀 필요 없음
    - 작성자의 환경은 Claude Code를 게이트웨이로 경유시킴: `env` 블록에 아래 키를 추가하며, 사용자 지정 기본 URL을 거치면 백그라운드 작업이 메인 모델을 쓰므로 `ANTHROPIC_DEFAULT_HAIKU_MODEL`로 작은 모델을 지정함
    - `ANTHROPIC_BASE_URL`이 멈춘 게이트웨이나 `[gateway] enabled = false`로 끈 게이트웨이를 가리키면 API 호출이 연결에 실패하며, 끈 경우 `doctor`가 `fail`로 보고

```json
"env": {
  "ANTHROPIC_BASE_URL": "http://127.0.0.1:47332",
  "ANTHROPIC_DEFAULT_HAIKU_MODEL": "<haiku 모델 ID>"
}
```

### 4. 상시 실행

1. **작업 스케줄러**
    - `.cmd/install-task.ps1`이 작업 `rust-agents-proxy`를 등록하며, 현재 사용자 계정으로 로그온 시 `conhost.exe --headless "<exe>" serve`를 실행
    - 실패 시 재시작: 최대 999회, 1분 간격이며 실행 시간 제한이 없고 두 번째 인스턴스는 무시함
    - `serve`는 `admin.token`을 쓰기 전에 관리 포트를 바인딩함. 다른 `serve` 등이 관리나 훅 포트를 쓰고 있으면 오류로 종료하고 실행 중인 인스턴스의 토큰은 건드리지 않으므로, 동시 기동해도 토큰을 덮어쓰지 않음
    - 감독: 관리나 훅 서버가 끝나거나 panic하면 프로세스 전체가 0 이 아닌 코드로 종료함. 반만 동작하는 `serve`가 남지 않고 작업 스케줄러가 다시 띄움
    - OTLP, 색인기, 리포트 스케줄러, 게이트웨이가 끝나면 `serve`는 경고를 출력하고 대시보드에 warn 알림을 보낸 뒤 계속 실행
    - 시작 시 첫 색인을 기다린 뒤 어제 리포트를 다시 만듦. 색인이 따라잡기 전에 만들어진 리포트는 이때 교체됨
    - 작업은 다음 로그온부터 시작하며, 지금 시작하려면 `Start-ScheduledTask -TaskName rust-agents-proxy`를 실행
2. **작업 종류**
    - `-Action Install`, `-Action Uninstall`, `-Action Status`를 받고, 비대화형 실행에는 `-ExePath <exe>`와 `-Yes`를 지정
    - `-Action`을 생략하면 터미널에서는 선택 창을 띄우고, 그 밖에서는 `Status`로 동작

```powershell
.cmd\install-task.ps1 -Action Install -ExePath target\release\rust-agents-proxy.exe -Yes
```

### 5. 확인

1. **상태**
    - `status`는 관리 `/api/status` JSON을 출력: 버전, pid, 가동 시간, 데이터 폴더, 포트 상태, 실시간 카운터
2. **텔레메트리 수신**
    - Claude Code 세션을 한 번 쓴 뒤 `status`의 `otlp_batches`가 늘어야 함
3. **doctor 재실행**
    - `doctor`를 다시 돌려 `fail`과 `warn` 항목이 없도록 맞춤

```bat
rust-agents-proxy status
```

---

## 명령

1. **serve**
    - 상주 서버 실행: 관리, OTLP, 훅, 게이트웨이, 기록 색인기, 리포트 스케줄러
2. **tui**
    - 실시간 대시보드만 엶. 명령을 아예 생략하면 분리된 `serve` 실행을 먼저 보장한 뒤 대시보드를 엶
3. **index**
    - 세션 기록을 한 번 색인하고 JSON 요약을 출력: 검사·변경 파일 수, 읽은 byte, 경과 밀리초, 행 수
4. **report**
    - 일일 리포트를 쓰고 경로를 출력하며, `--date YYYY-MM-DD`로 날짜를 고르고 기본값은 어제
5. **hook**
    - `hook <route>`는 command 훅 클라이언트: stdin을 `http://127.0.0.1:<훅 포트>/hook/<route>`로 보내고 응답을 출력
    - route는 `a-z`, `0-9`, `-`로 이루어진 한 구간이거나 `relay/<이름>`
    - 2xx 응답이면 종료 코드 0, 그 밖에는 1이며 stdin 상한은 16 MiB
    - route가 `pretool`로 시작하면 fail-closed: stdin 상한 초과, 전송 실패, 2xx 아님이면 거부 JSON을 출력하고 종료 코드 0이며, `relay/*` route는 종료 코드 1
6. **status**
    - 관리 상태 JSON을 출력
7. **doctor**
    - 데이터 폴더, 데이터베이스, 포트, 중계 대상, 훅 등록, 허용 URL, 텔레메트리, 게이트웨이, projects 폴더를 점검
    - `pretool-chain`이 등록돼 있는데 훅 포트가 `/health`에 응답하지 않으면(또는 `hooks.enabled`가 false이면) 모든 Bash 호출이 막히므로 `fail`로 보고
8. **전역 옵션**
    - `--config <경로>`로 `rap.toml`을 지정하며, 설정 오류는 종료 코드 2, 그 밖의 오류는 1

---

## 대시보드

1. **탭**
    - 키 `1`-`7`의 일곱 탭: 개요, 도구, 훅, 모델, 에이전트, 설정(`6`), 리포트(`7`)
    - 게이트웨이 탭은 없음: 게이트웨이 지연(첫 바이트, 첫 토큰, 전체 시간의 p50/p90)은 `GET /api/gateway`와 일일 리포트로 계속 볼 수 있고, 개요의 서버 상태 카드는 게이트웨이 포트를 상태 점으로 계속 표시
    - 앱 바는 왼쪽에 `rust-agents-proxy` 배지만 두고 설명 문구는 없으며, 오른쪽에 구간과 갱신 신선도를 표시
    - 배경 스레드가 2초마다 `rap.sqlite`를 읽기 전용으로 읽으므로 UI 스레드는 I/O를 하지 않음
    - 무거운 패널(도구, 훅, 모델 통계, 에이전트 통계, 캐시 붕괴)은 10초마다 조회하며, 첫 수집, 구간 변경, `r` 새로고침은 즉시 조회
    - 화면은 변화가 있을 때(새 데이터, 입력, 알림, 강조 대상 변경)만 다시 그리며, 최소 1초에 한 번은 그림
    - 탭 이름이 폭에 맞지 않으면 탭 바가 탭 사이 간격을 없애고, 더 좁으면 번호를 붙인 압축 이름으로 바뀌어 60열까지 일곱 탭이 모두 보이고 클릭됨
    - 이 버전의 화면 문구와 일일 리포트 본문은 한국어뿐임
2. **키**
    - `1`-`7`, `Tab`, `Shift-Tab`: 탭 이동
    - `Up`, `Down`, `PgUp`, `PgDn`, `Home`, `End`: 스크롤
    - `w`: 집계 구간을 1시간, 24시간, 7일 순으로 전환
    - `s`, `S`: 포커스 표 카드의 다음 정렬 열(오름차순)과 현재 열의 정렬 순환(오름차순, 내림차순, 카드 기본 정렬) (포커스 카드에 정렬이 없으면 탭의 주 표)
    - `Left`, `Right`: 리포트 날짜, 리포트 탭에서만
    - `g`: 리포트 생성, 리포트 탭에서만
    - `i`: 재색인
    - `g`와 `i`는 관리 API를 거치므로 `serve`가 실행 중이어야 함
    - `t`: 항상 위 전환, `l`: 현재 탭 카드 배치와 끌어 정한 열 폭 초기화
    - `r`: 새로고침, `?`: 도움말, `q`, `Esc`, `Ctrl+C`: 종료
3. **마우스**
    - 화면 구성과 조작은 `database-monitor`를 따름: 모든 탭이 테두리 카드들로 이루어짐
    - 탭 클릭으로 전환, 카드 클릭으로 포커스, 표 행 클릭으로 선택(상세는 옆 카드), 표 머리글 클릭으로 그 열 정렬 (다른 머리글은 오름차순에서 시작하고, 같은 머리글을 다시 누르면 오름차순, 내림차순, 표의 기본 정렬 순으로 순환하며, 정렬 머리글에 `↑` 또는 `↓`를 표시하고 순환이 끝나면 기본 열이 기본 화살표를 다시 표시)
    - 휠은 포인터 아래 카드를 스크롤하며, 카드나 카드 경계에 올리면 강조됨
    - 스크롤 가능한 카드는 내용이 넘칠 때 오른쪽 테두리 바로 안쪽 거터 열에 스크롤바를 표시하므로 썸이 테두리와 같은 색이 되지 않음: `░` 트랙은 조용하고, 회색 `█` 썸은 올리면 밝아지고 끄는 동안 흰색이 됨. 썸을 끌어 이동하고, 트랙(거터와 테두리 열)을 클릭하면 한 화면 이동하며, 하단의 `a-b / n` 표시는 그대로 유지
    - 선택 행은 왼쪽 여백부터 거터까지 이어지는 띠: 포커스 카드는 진한 파랑 바탕에 흰 글자, 나머지 카드는 옅은 회색 바탕
    - 카드 사이 경계를 끌어 크기를 조절하며, 이웃 카드는 최소 크기를 유지하고 창 크기가 바뀌어도 비율이 유지됨. 배치는 실행 중에만 기억함. 경계는 포인터를 올렸거나 끄는 동안에만 회색 손잡이가 나타나고 평소에는 아무것도 그리지 않음
    - 표 카드의 머리글 두 칸 사이 경계를 끌어 왼쪽 열 폭을 조절하며(최소 3칸, 숫자 열은 내용 폭 미만으로 줄지 않음), 남는 폭 열이 변화를 흡수하고 폭은 카드별로 실행 중에만 기억하며, 경계 더블클릭이나 `l`로 자동 폭으로 복원함. 스크롤바 거터와 카드 경계 손잡이가 먼저 잡히고, 열 경계 드래그는 정렬을 일으키지 않음. 경계에는 같은 회색 손잡이가 포인터를 올렸거나 끄는 동안에만 나타남
    - 하단 줄 왼쪽에 재색인, 구간, 새로고침, 항상 위, 배치 초기화 버튼이, 오른쪽에 도움말, 종료 버튼이 있으며(좁은 창에서는 왼쪽 그룹이 먼저 줄어듦), 리포트 탭에는 이전·다음 날짜와 생성 버튼이 더 있음
    - 버튼 아래 안내 줄은 상황에 따라 바뀜: 카드 경계·열 경계·스크롤바 위의 포인터, 포커스 카드 종류, 탭(정렬 가능한 표는 정렬 키, 리포트 탭은 날짜 키)
    - 이벤트 피드는 고정 머리글이 있는 열 격자(시각, 종류, 이름, 에이전트, 입력, 출력, 캐시읽기, 소요)이며, 카드 폭에 따라 표시 열이 줄고 긴 에이전트 id 는 `이름#xxxx` 로 줄여 보임
    - 모든 표 카드는 머리글 클릭 또는 `s`/`S`로 정렬하며, 카드별 정렬 상태와 세 번째 클릭이나 `S`가 되돌리는 기본 정렬이 있음 (피드: 시각 내림차순, 도구: 합계 시간 내림차순, 훅: 횟수 내림차순, 모델·에이전트: 요청 내림차순). 숫자·시간은 값으로, 문자는 표시 문자열로 정렬하고, 다시 정렬해도 선택 행은 같은 항목을 유지함
    - 모델 탭은 모델별 통계(요청, 세션, 에이전트 종류, 토큰, 캐시 적중률, 대기 p50/p90, 추정 비용, 마지막 시각)와 상세 카드, 캐시 붕괴 후보를 보여 주고, 에이전트 탭은 에이전트 종류별 통계(실행, 요청, 세션, 토큰, 추정 비용, 대기 p50/p90, 도구 호출·오류, 주 모델, 마지막 시각)와 상세 카드를 보여 줌
    - 글자 열은 자연 폭(가장 넓은 칸이나 머리글, 한글은 2칸)을 먼저 받으므로 카드에 여유 폭이 있는 동안은 말줄임이 없고, 마지막 늘어나는 글자 열(예: 훅 상세)이 남는 폭을 가지며, 좁은 카드에서는 가장 넓은 글자 열이 먼저 줄고 말줄임되고 숫자 열은 전체 폭을 유지하며 숫자가 잘리기 전에 맨 오른쪽 열부터 숨김. 이벤트 피드도 이름·에이전트 열이 같은 방식으로 넓어짐
    - 리포트 보기는 `text` 울타리 줄을 숨기고 박스 표를 그대로(줄바꿈 없이 잘라서) 뮤트 선 색과 강조 머리글 행으로 보여 주며, 예전 파이프 표 리포트도 표시할 때 박스 표로 바꿈
4. **콘솔**
    - 클래식 콘솔 창에서는 실행 중 `D2Coding ligature` 글꼴을 적용하고 종료 시 원래 글꼴로 되돌림 (`database-monitor` 와 동일)

---

## 포트와 엔드포인트

### 1. 관리 47330

1. **조회 엔드포인트**
    - `GET /health`는 `ok`, `GET /api/status`는 상태 JSON을 반환
    - `GET /api/overview`, `tools`, `hooks`, `models`, `gateway`, `cache-breaks`, `minutes`는 집계를 반환하며 `?hours=`로 구간을 지정(기본 24, 1-720으로 보정)
    - `GET /api/model-stats`는 모델당 한 행(기록 색인 출처만)을 반환: 요청, 세션, 에이전트 종류 수, 토큰 합계, 캐시 적중률, 대기 p50/p90, 추정 비용, 마지막 시각이며 `?hours=`는 위와 같음
    - `GET /api/agent-stats`는 에이전트 종류(`#` 앞 라벨, 메인 스레드는 `main`)당 한 행을 반환: 실행 수(에이전트 인스턴스 수, `main`은 세션 수), 요청, 세션, 토큰 합계, 캐시 적중률, 대기 p50/p90, 추정 비용, 도구 호출과 오류, 최다 모델, 마지막 시각이며 `?hours=`는 위와 같음
    - `GET /api/report?date=YYYY-MM-DD`는 보관된 리포트를 Markdown으로 반환하고, 날짜가 없거나 잘못되면 `400`, 보관본이 없으면 `404`
    - `GET /api/events`는 실시간 이벤트 Server-Sent Events 스트림이며 15초마다 `: ping` 주석을 보냄
2. **토큰 보호 엔드포인트**
    - `POST /api/reindex`는 기록 색인기를 한 번 실행
    - `POST /api/report?date=YYYY-MM-DD`는 리포트를 생성·보관하며 기본 날짜는 어제
    - 모든 `POST`는 헤더 `x-rap-token`에 토큰이 필요하며 없거나 틀리면 `401`
3. **토큰**
    - OS 난수 32 byte를 hex로 `%LOCALAPPDATA%\rust-agents-proxy\admin.token`에 기록
    - `serve`를 시작할 때마다 새로 만듦

### 2. OTLP 47331

1. **경로**
    - `POST /v1/logs`, `/v1/metrics`, `/v1/traces`
2. **받는 입력**
    - JSON만 받으며, 다른 content type은 `415`와 함께 `use OTEL_EXPORTER_OTLP_PROTOCOL=http/json` 안내를 반환
    - `gzip` content encoding은 허용하며 본문 상한은 16 MiB이고 압축을 푼 뒤에도 같음
3. **처리**
    - 로그는 이벤트 세 가지(`api_request`, `tool_result`, `hook_execution_complete`)를 행으로 변환하고 나머지 이벤트는 무시
    - 지표와 트레이스는 건수만 셈

### 3. 게이트웨이 47332

1. **역할**
    - 기본 켜짐(`gateway.enabled = true`)이며 Anthropic API로 가는 투명 중계(`gateway.upstream`, HTTPS만 허용)
    - `POST /v1/messages`의 첫 바이트, 첫 토큰, 전체 시간, 상태, 토큰 사용량을 계측하며 다른 경로는 계측 없이 중계
2. **동작**
    - 요청 헤더는 인증 정보를 포함해 hop-by-hop 헤더와 `host`만 빼고 상류로 그대로 보내며 어느 것도 저장하지 않음
    - 응답은 버퍼링 없이 흘려보내고, 상류 연결 오류는 `502` JSON 오류가 됨
    - 응답 헤더가 `gateway.upstream_timeout_secs` 안에 오지 않으면 `504` JSON 오류를 반환하며, 스트리밍 본문에는 시한이 없음
    - 긴 스트림이 훅·색인과 경쟁하지 않도록 전용 런타임 스레드에서 실행

### 4. 훅 47333

1. **경로**
    - `POST /hook/pretool-chain`, `POST /hook/relay/{route}`, `GET /health`
2. **relay 경로**
    - `{route}`는 `a-z`, `0-9`, `-`만 허용
    - 요청을 `{hooks.relay_target}/hook/{route}`로 그대로 전달하고, 상류의 상태와 본문을 그대로 돌려줌
    - 판정, 가드 라벨, 소요 시간을 기록하며 상류에 닿지 못하면 `502`를 반환
3. **본문 상한**
    - 요청당 16 MiB

### 5. 공통 규칙

1. **바인딩**
    - 모든 포트는 `127.0.0.1`에서만 수신
2. **요청 검사**
    - `Host` 헤더가 `127.0.0.1:<포트>` 또는 `localhost:<포트>`가 아니면 `403`
    - `Origin` 또는 `Sec-Fetch-Site` 헤더가 있는 요청은 `403`이며 브라우저에서 보낸 요청을 차단함

---

## 훅 체인

### 1. 흐름

1. **단계**
    - `pretool-chain`은 구형 CLI 명령을 재작성한 뒤 페이로드를 기존 가드 `{hooks.relay_target}/hook/pretool-guard`(기본 `http://127.0.0.1:47321`)로 전달
    - 가드는 이 프로젝트에 포함되지 않으며 그 주소에서 이미 응답하고 있어야 함
2. **결과**
    - 가드 연결 불가, 시한 초과, 2xx 아님, JSON 객체 아님: 사유 `[rap] pretool-guard 응답 없음 (fail-closed)`로 거부
    - 가드가 거부: 가드 응답을 그대로 통과
    - 가드가 허용하고 재작성이 없음: 가드 응답을 그대로 통과
    - 가드가 명시적으로 허용하고 재작성이 있음: `permissionDecision: allow`에 새 명령을 담은 `updatedInput`과 변환 내역을 알리는 `additionalContext` 메모를 붙여 반환
    - 가드가 판정 없음(`{}`)이고 재작성이 있음: `permissionDecision` 없이 `updatedInput`과 메모만 반환. 재작성된 명령에 Claude Code 의 일반 권한 흐름이 그대로 적용되며, rap 은 가드가 주지 않은 허용을 만들지 않음
    - 가드가 확인 요청(`ask`)이고 재작성이 있음: `ask`와 `updatedInput`을 함께 반환. 사용자는 가드가 평가한 바로 그 명령을 승인함
    - 가드가 자체 `updatedInput`을 돌려줌: 가드 응답이 우선하며 재작성은 버림
    - 재작성 뒤 가드가 허용, 확인 요청, 거부 외의 판정(예: `defer`)을 내림: 재작성을 버리고 원본 명령으로 가드를 다시 호출해 그 두 번째 응답을 그대로 반환(거부는 거부로 유지되며, 두 번째 응답이 연결 불가, 시한 초과, 2xx 아님, JSON 객체 아님이면 fail-closed)
3. **기록 항목**
    - 판정(`allow`, `deny`, `rewrite`, `error`), 가드 라벨, 변환된 명령 이름, 소요 시간, 재작성 1건당 4200 ms의 절감 추정

### 2. 재작성 규칙

1. **cat**
    - `cat <파일>`은 `bat --plain --paging=never <파일>`로 바뀜
    - 인자가 없거나, `-`로 시작하는 인자가 있거나, 리다이렉션이 있으면 그대로 둠
    - `>|`(noclobber 무시 리다이렉션)가 들어 있는 명령은 재작성하지 않음
2. **ls**
    - 모든 플래그가 `-l -a -la -al -lh -lah -alh -1 -A -h` 중 하나일 때 `eza`로 바뀌며, `h`는 빼고 `A`는 `a`로 바꾸고 경로가 없으면 `.`을 붙임
3. **eza**
    - 플래그만 있는 경로 없는 `eza`에는 ` .`을 덧붙임
4. **git diff**
    - `git diff`에 ` | delta`를 덧붙임
    - 요약 플래그(`--stat`, `--shortstat`, `--numstat`, `--name-only`, `--name-status`, `--quiet`, `--exit-code`, `--check`, `--summary`, `--no-patch`, `-s`), `--no-index`, 리다이렉션, 기존 파이프, 바로 뒤의 `&&`나 `||`가 있으면 그대로 둠 (파이프가 종료 코드를 바꾸기 때문)
5. **그 외**
    - 명령을 `&&`, `||`, `;`, `|`로 나누어 구간마다 따로 변환
    - 변환할 수 없는 `cat`이나 `ls` 구간이 하나라도 있으면 명령 전체를 건드리지 않음
    - 줄바꿈, heredoc, 백틱, `$(`, `$'`, `${`, `[[`, `case`, 프로세스 치환, 닫히지 않은 따옴표, 주석, `&`, 괄호, 따옴표 밖 중괄호, 비ASCII 공백, 따옴표 밖 비ASCII 문자, 끝의 역슬래시가 있어도 건드리지 않음
    - 플래그 여부는 따옴표를 벗긴 값으로 판정하므로 `ls "-s" src`는 지원하지 않는 플래그 `-s`로 봄
    - `grep`, `find` 등 다른 명령은 재작성하지 않으며 최종 명령은 항상 가드가 다시 검사

---

## 데이터와 개인정보

### 1. 데이터 폴더

1. **위치**
    - `%LOCALAPPDATA%\rust-agents-proxy`, `RAP_DATA_DIR`로 변경
2. **내용**
    - `rap.sqlite`: WAL 모드 데이터베이스
    - `rap.toml`: 선택 설정이며 예시는 `rap.example.toml`
    - `admin.token`: 현재 `serve` 실행의 관리 토큰
    - `serve.log`: 명령 없이 실행했을 때 띄우는 분리 `serve`의 출력이며 실행마다 새로 만듦
    - `reports\YYYY-MM-DD.md`: 일일 리포트이며 데이터베이스에도 함께 보관

### 2. 저장하는 데이터

1. **메타데이터만**
    - 모델·에이전트 이름, 도구 이름, 세션 id, 프로젝트 폴더 이름, 시각, 소요 시간, 토큰 수
    - 훅 판정: route, 가드 라벨, 결과, 감지한 구형 명령의 이름
2. **저장하지 않는 것**
    - 프롬프트 본문, 명령 본문, 파일 내용, 도구 입력·출력, 인증 헤더
    - 기록 읽기 중에는 도구 결과를 최대 400자까지 메모리에 두지만 오류 결과에서 가드 라벨과 명령 이름을 읽는 데만 쓰고, 그 둘만 기록
3. **설정 점검**
    - `doctor`와 설정 탭은 필요할 때 `settings.json`과 `.claude.json`을 읽으며, env 값은 비교에만 쓰고 출력하거나 저장하지 않음

### 3. 중복 처리

1. **사실 하나에 소유 문서 하나**
    - 같은 요청이 기록, OTLP, 게이트웨이로 모두 들어올 수 있으므로 집계는 출처마다 자기 용도로만 셈
    - 키와 규칙은 [ARCHITECTURE-KO.md](ARCHITECTURE-KO.md)에 정리

---

## 설정

### 1. 적용 순서

1. **순서**
    - `--config <경로>`, `RAP_CONFIG`, 데이터 폴더의 `rap.toml`, 기본값 순
2. **엄격성**
    - 알 수 없는 키는 오류이고, 포트는 0이 아니며 서로 달라야 하고, `report.run_hour`는 0-23이어야 함
    - `hooks.relay_target`은 `http` 루프백 주소(`127.0.0.1`, `localhost`, `[::1]`)만 허용하고 rap 자신의 포트는 쓸 수 없으며, 그 밖은 설정 오류라 기동이 실패함
    - `hooks.guard_deadline_ms`는 1~4500 범위여야 함. `defer` 재검사의 가드 2회 호출도 훅 timeout 10초 안에 들어가게 하기 위함
3. **Claude 홈**
    - `rap.toml`의 `claude_home`, `CLAUDE_CONFIG_DIR`, `%USERPROFILE%\.claude` 순

### 2. rap.toml

1. **키**
    - 모든 키는 생략할 수 있으며, 아래 블록이 기본값이고 `rap.example.toml`과 같은 내용
    - `gateway.enabled`의 기본값은 `true`이며, `false`면 포트 `47332`를 건드리지 않음
    - `gateway.upstream_timeout_secs`는 상류 응답 헤더를 기다리는 시한이며 초과 시 504 를 반환함. 헤더가 온 뒤 스트리밍 본문에는 시한이 없고, 값은 1 이상이어야 함

```toml
# data_dir = "C:/Users/<user>/AppData/Local/rust-agents-proxy"
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
# dir = "C:/Users/<user>/AppData/Local/rust-agents-proxy/reports"
run_hour = 0
```

2. **리포트 예약**
    - `serve`는 시작할 때 첫 색인 실행을 기다린 뒤(최대 10분, 초과하면 warn 알림) 어제 리포트를 다시 만들며, 보관된 리포트가 있어도 교체함
    - 이후 매일 로컬 시각 `run_hour:05`에 전일 리포트를 씀
    - 리포트는 로컬 달력 기준 하루를 다루며, 요약, 전일 대비, 비용 추정을 포함한 모델·에이전트별 사용량, 도구 지연 상위, 훅 판정, 캐시 붕괴 상위, 게이트웨이 데이터가 있을 때의 게이트웨이 지연, 규칙 기반 제안으로 구성
    - 리포트 파일의 모든 표는 섹션 제목 아래 `text` 펜스 안의 가벼운 선 박스 표(`┌─┬─┐`)이며, 한글·CJK는 2칸, 머리글은 가운데, 건수·시간 길이는 오른쪽 정렬이고 행 사이마다 구분선이 있음. 파이프 표는 쓰지 않음

---

## 측정

1. **범위**
    - 2026-11-15 작성자 PC에서 잰 값이며 보장이 아님: 다른 PC와 작업 부하에서는 달라짐
2. **기록 backfill**
    - 7일 backfill이 release 빌드에서 파일 453개, 602 MB를 1.7 s에 읽음
3. **훅 체인 오버헤드**
    - `pretool-chain`은 가드 단독 대비 Bash 호출당 약 1.6 ms를 더함: 재사용 연결에서 3.6 ms 대 2.0 ms
4. **거부**
    - 7일 이력에 cli-guard 거부 266건이 있었고, 건마다 중앙값 4.2 s의 모델 재왕복이 추가됨
    - 2026-11-14 실데이터 리포트의 거부는 `cat` 47건, `ls` 39건, `grep` 11건
    - 재작성 규칙은 `cat`과 `ls`만 다루고 `grep`은 다루지 않으므로 `grep` 거부는 남음
5. **네트워크**
    - API까지 왕복은 약 190 ms이며 로컬 프록시가 줄일 수 없음
6. **이득이 나오는 곳**
    - 속도 이득은 피한 거부와 피한 훅 프로세스 생성에서 나오며 네트워크 기법에서 나오지 않음
    - 대시보드의 절약 시간은 추정값: 재작성 횟수에 4200 ms를 곱한 값
    - 게이트웨이는 첫 바이트와 첫 토큰을 재기 위한 것이며 자체가 더하는 지연은 측정하지 않았음

---

## 소스에서 빌드

### 1. 요구 사항

1. **툴체인**
    - Rust 1.85 이상(edition 2024), 호스트 `stable-x86_64-pc-windows-gnullvm`
    - `.cargo/config.toml`이 clang, `ar`, `windres`, 링커 경로를 `C:/JUNGHO/1.Language/8.Clang` 아래로 고정하므로 다른 PC에서는 바꿈
2. **vendor crate**
    - 빌드는 프로젝트 `vendor/` 폴더를 읽음: 공용 전역 vendor에 이 crate들이 없기 때문
    - `vendor/`는 gitignore 대상이며 아래 명령으로 다시 만듦

```bat
cargo vendor --config net.offline=false vendor
```

### 2. 릴리스 빌드

1. **build.cmd**
    - 실행 중인 exe를 `rust-agents-proxy.exe.stale-<n>`으로 옆에 옮겨 실행 중인 `serve`가 이미지를 유지하게 하고, cargo가 원래 경로에 새 바이너리를 링크
    - 이전 stale 이미지를 지우고, `cargo build --release --offline`을 실행한 뒤 exe에 서명
    - 새 빌드를 쓰려면 이후 `serve`를 재시작
2. **서명**
    - `.cmd/sign.ps1`은 `RUST_AGENTS_PROXY_SIGN_THUMBPRINT`의 지문에 해당하는 인증서(`CurrentUser\My` 저장소, 개인 키 필요)로 서명
    - 지정하지 않으면 로컬 자체서명 코드서명 인증서를 찾아 쓰거나 새로 만들며, 신뢰 루트에 등록되지 않으므로 서명이 존재한다는 것만 보장
    - `RUST_AGENTS_PROXY_SIGN_TIMESTAMP`로 타임스탬프 서버를 선택 지정하며, 서명이 실패하면 경고만 하고 exe는 서명 없이 남음
3. **리소스**
    - `build.rs`가 `windres`로 아이콘, 버전 정보, `asInvoker` manifest를 삽입하고 `libunwind.dll`을 exe 옆에 복사
    - 아이콘은 `assets/icon`에 있으며, 두 SVG 원본에서 `rust-agents-proxy.ico`를 다시 만들려면 ImageMagick이 필요한 `assets/icon/build_icon.py`를 실행

---

## 한계

1. **게이트웨이**
    - `HTTPS_PROXY`를 따르지 않음
    - `Content-Encoding` 헤더가 있는 응답은 중계와 시간 계측은 하지만 토큰 사용량은 읽지 않음
2. **Windows 네트워크**
    - 이 Windows 호스트에서는 `Connection: close`를 쓴 HTTP/1.1 요청이 가끔 응답을 잃으므로(RST) 내장 블로킹 클라이언트는 그 헤더를 쓰지 않으며, 원인은 미확인
3. **OTLP**
    - 매핑은 fixture 파일로만 시험했으며 실제 Claude Code 내보내기로는 시험하지 않았음
4. **가드 의존**
    - 체인은 `hooks.relay_target`에 기존 가드 서버가 있어야 하며, 없으면 모든 Bash 호출이 거부됨
5. **부분 기동**
    - OTLP나 게이트웨이 포트를 바인딩하지 못하면 `serve`는 그 서버 없이 실행하고 경고만 출력하므로 `doctor`와 시작 로그를 확인
    - 실행 중 OTLP, 색인기, 리포트 스케줄러, 게이트웨이가 끝나도 알림만 보내며, 관리나 훅이 끝나면 `serve`가 멈춤
6. **언어와 플랫폼**
    - 대시보드와 리포트 문구는 한국어뿐이고, 기본 데이터 폴더는 `%LOCALAPPDATA%` 또는 `RAP_DATA_DIR`가 필요
7. **비용 추정**
    - 내장 모델별 단가표를 쓰며 알 수 없는 모델은 0으로 셈
8. **조회 엔드포인트**
    - 관리 포트의 `GET` 엔드포인트는 토큰이 필요 없으며, Host·Origin 검사 뒤에서 메타데이터만 반환

---

## 개발

### 1. 검사

1. **명령**
    - 변경할 때마다 저장소 루트에서 실행

```bat
cargo clippy --offline --all-targets -- -D warnings
cargo test --offline
cargo fmt --check
```

2. **구분선 주석**
    - 구분선 주석은 80열에서 끝나야 함

```bat
bun ~/.claude/skills/languages-comments/scripts/divider-width.ts <파일>...
```

### 2. 테스트

1. **구성**
    - 단위 테스트는 각 모듈 옆 `#[cfg(test)]` 또는 `tests.rs`, `test_support.rs` 파일에 있음
    - `tests/fixtures`는 테스트가 읽는 fixture Claude 홈, 기록, OTLP 페이로드를 담음
2. **범위**
    - 재작성 표, 가짜 가드를 쓴 체인(허용, 거부, 시한, 재작성, 재검사), 훅 클라이언트의 fail-closed 출력, 임시 포트의 훅 서버
    - 관리 토큰과 집계, `reject_non_local` 표, OTLP 매핑과 크기 상한(413), 게이트웨이 502·504 경로, SSE 사용량 파서, 색인기
    - 포트 바인딩과 serve 감독 규칙, doctor 점검, 시작 시 덮어쓰는 리포트 흐름, 이벤트 단위 훅 거부 집계
    - 대시보드 렌더링, 마우스·배치 처리, 스크롤바

---

## 라이선스

1. **Apache-2.0**
    - [LICENSE.md](LICENSE.md) 참조
