# CLAUDE.md (rust-agents-proxy)

- 이 저장소에서 작업하는 에이전트용 프로젝트 규칙. 구조는 [ARCHITECTURE.md](ARCHITECTURE.md)([한국어](ARCHITECTURE-KO.md)), 사용법은 [README.md](README.md)([한국어](README-KO.md)).

---

## 1. 불변 조건

1. **개인정보**
    - 메타데이터(이름, 횟수, 소요 시간, 토큰 수, 가드 라벨)만 저장하고 기록한다.
    - 프롬프트 본문, 명령 본문, 파일 내용, 도구 입출력, 인증 헤더를 저장·로그·응답에 담지 않는다.
    - `hook_events.detail`에는 가드 라벨과 감지한 명령 이름만 넣고 원문은 넣지 않는다.
2. **fail-closed**
    - `pretool-chain`이 가드의 정상 응답을 받지 못하면 거부한다(연결 불가, 시한 초과, 2xx 아님, JSON 객체 아님).
    - 체인이 스스로 허용 응답을 만드는 경로를 추가하지 않으며, 재작성한 명령은 반드시 가드가 다시 검사한다.
    - `pretool-chain`을 `onFailure: block`으로 등록하면 `serve`가 꺼진 동안 모든 Bash가 막히므로, 등록은 `doctor` 통과와 `serve` 상주를 확인한 뒤에만 한다.
3. **127.0.0.1 전용**
    - 새 리스너는 `net::common::serve_local_http`로 열어 `127.0.0.1` 바인딩과 Host·Origin·Sec-Fetch-Site 검사를 그대로 거친다.
    - 상태를 바꾸는 관리 요청(`POST`)은 `x-rap-token` 검사를 거친다.
4. **핫 경로에 프로세스 생성 금지**
    - 훅 체인, 수집, 색인, 대시보드 경로에서 `Command`로 외부 프로세스를 띄우지 않는다.
    - 내부 HTTP 호출은 `std::net::TcpStream` 블로킹 클라이언트나 hyper 클라이언트를 쓴다.
5. **블로킹 작업 분리**
    - 비동기 문맥에서 SQLite 접근, 색인, 리포트 생성은 `spawn_blocking`으로 실행한다.

---

## 2. 빌드·검증

1. **명령**
    - `.cmd\build.cmd`: release 빌드 + 무중단 exe 교체 + Authenticode 서명(`RUST_AGENTS_PROXY_SIGN_THUMBPRINT`로 인증서 지정)
    - `cargo clippy --offline --all-targets -- -D warnings`
    - `cargo test --offline`
    - `cargo fmt --check`
    - `bun ~/.claude/skills/languages-comments/scripts/divider-width.ts <파일>...`: 구분선 폭 검사
2. **환경**
    - 툴체인 `stable-x86_64-pc-windows-gnullvm`, 의존성은 프로젝트 `vendor/`(gitignore 대상)에서 오프라인으로 읽는다. 전역 vendor에는 이 crate들이 없다.
    - 새 crate를 쓰려면 `cargo vendor --config net.offline=false vendor`로 `vendor/`를 다시 만든다.
    - release 프로필의 `panic = "unwind"`를 `abort`로 바꾸지 않는다: 단일 프로세스 다중 포트에서 작업 단위 panic 격리에 쓴다.
3. **실행 금지**
    - 검증 중 `serve`를 띄우거나 `~/.claude` 설정을 수정하지 않는다. 필요하면 사용자에게 요청한다.
    - 명령 없이 exe 를 실행해도 `serve`가 분리 상주로 기동되므로 검증 중 금지한다. 화면 확인은 `tui` 명령(서버 미기동)만 쓴다.

---

## 3. 코드 규칙

1. **구분선**
    - 선언 구분선은 `// N. 제목 ---` 형태로 표시 폭 80열에 맞춘다.
    - 위 `divider-width.ts`로 검사하고, 어긋나면 같은 스크립트의 `--write`로 맞춘다.
2. **파일 머리말**
    - 모든 `.rs` 파일은 첫 줄에 `src/` 기준 경로 `//! core/config.rs`, 둘째 줄에 용도 한 줄 `//! ...`을 둔다(`build.rs`는 `//! build.rs`).
3. **src 루트**
    - `src/` 루트에는 `main.rs`와 `lib.rs`만 둔다. 나머지는 모듈 디렉터리(`core`, `serve`, `store`, `net`, `hooks`, `transcript`, `report`, `inventory`, `power`, `tui`) 안에 둔다.
    - 새 모듈은 `lib.rs`에 `pub mod`로 선언한다.
4. **테스트**
    - 단위 테스트는 모듈 옆 `#[cfg(test)]`, `tests.rs`, `test_support.rs`에 두고 fixture는 `tests/fixtures`에 둔다.
5. **블로킹 HTTP 클라이언트**
    - 요청에 `Connection: close` 헤더를 넣지 않는다. 이 Windows 호스트에서 응답을 가끔 잃는 현상(RST)이 있으며 원인은 미확인이다.

---

## 4. 문서

1. **영어·한국어 쌍**
    - `README.md`와 `README-KO.md`, `ARCHITECTURE.md`와 `ARCHITECTURE-KO.md`는 구조와 사실을 같게 유지한다. 한쪽을 바꾸면 다른 쪽도 같이 바꾼다.
    - 같은 사실은 한 문서가 소유하고 다른 문서는 링크한다: 구조·병합 규칙·보안 경계는 ARCHITECTURE, 설치·사용법·한계는 README.
