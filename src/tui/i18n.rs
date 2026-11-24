//! tui/i18n.rs
//! 화면 문구 카탈로그: 영어·한국어 두 벌, 기본 영어, 값이 들어가는 문구는 형식 함수.

use crate::tui::layout::SplitAxis;
use crate::tui::poller::{AdminAction, AdminOutcome};
use crate::tui::state::ToastLevel;

// 1. 표시 언어 ----------------------------------------------------------------
// 파일 저장 값은 en, ko
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Lang {
    #[default]
    En,
    Ko,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ko => "ko",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Self::En),
            "ko" => Some(Self::Ko),
            _ => None,
        }
    }
}

// 2. 문구 카탈로그 ------------------------------------------------------------
// 항목마다 영어·한국어가 한 줄에 있어 한쪽만 빠질 수 없음, ALL 은 완결성 테스트용
macro_rules! define_messages {
    ($($name:ident => ($en:expr, $ko:expr),)+) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Msg {
            $($name,)+
        }

        impl Msg {
            pub const ALL: &'static [Msg] = &[$(Msg::$name,)+];
        }

        pub fn text(lang: Lang, msg: Msg) -> &'static str {
            match msg {
                $(Msg::$name => match lang {
                    Lang::En => $en,
                    Lang::Ko => $ko,
                },)+
            }
        }
    };
}

define_messages! {
    // 탭, 구간
    TabOverview => ("Overview", "개요"),
    TabTools => ("Tools", "도구"),
    TabHooks => ("Hooks", "훅"),
    TabModels => ("Models", "모델"),
    TabAgents => ("Agents", "에이전트"),
    TabSettings => ("Settings", "설정"),
    TabReport => ("Report", "리포트"),
    WindowToday => ("Today", "오늘"),
    WindowHour => ("1h", "1시간"),
    WindowDay => ("24h", "24시간"),
    WindowWeek => ("7d", "7일"),

    // 앱 바
    AppBarWindow => ("Window", "구간"),
    AppBarWaiting => ("Waiting", "대기"),
    BadgePinned => ("Pinned", "최상단"),

    // 하단 버튼
    ChipReindex => ("Reindex", "재색인"),
    ChipRefresh => ("Refresh", "새로고침"),
    ChipPinOn => ("Pin ON", "최상단 ON"),
    ChipPinOff => ("Pin OFF", "최상단 OFF"),
    ChipResetLayout => ("Reset layout", "배치 초기화"),
    ChipLanguage => ("EN", "KO"),
    ChipHelp => ("Help", "도움말"),
    ChipQuit => ("Quit", "종료"),

    // 하단 안내 (첫 낱말이 키·동작 글자)
    HintRelease => ("Release to place", "놓기 위치 확정"),
    HintCancel => ("Esc Cancel", "Esc 취소"),
    HintDragCardSize => ("Drag card size", "드래그 카드 크기"),
    HintResetLayout => ("l Reset layout", "l 기본 배치"),
    HintDragColumnWidth => ("Drag column width", "드래그 열 폭"),
    HintDoubleClickAuto => ("Double-click auto width", "더블클릭 자동 폭"),
    HintDragScroll => ("Drag scroll", "드래그 스크롤"),
    HintTrackPage => ("Track-click page", "트랙클릭 한 화면"),
    HintSelectRow => ("↑↓ Select row", "↑↓ 행 선택"),
    HintPage => ("PgUp/PgDn Page", "PgUp/PgDn 페이지"),
    HintFirstLast => ("Home/End First/Last", "Home/End 처음·끝"),
    HintScroll => ("↑↓ Scroll", "↑↓ 스크롤"),
    HintSortColumn => ("s Sort column", "s 정렬 열"),
    HintSortOrder => ("S Sort order", "S 정렬 방향"),
    HintReportDate => ("←→ Date", "←→ 날짜"),
    HintGenerate => ("g Generate", "g 생성"),
    HintNextTab => ("Tab Next tab", "Tab 다음 탭"),
    HintAllKeys => ("? All keys", "? 전체 단축키"),

    // 도움말 겹침
    HelpTitle => ("Help", "도움말"),
    HelpClose => ("Any key or click: close", "아무 키·클릭: 닫기"),
    HelpTruncated => ("… (enlarge window to show all)", "… (창을 키우면 전체 표시)"),
    HelpSectionKeyboard => ("Keyboard", "키보드"),
    HelpSectionMouse => ("Mouse", "마우스"),
    HelpSwitchTab => ("Switch tab (number = tab bar position)", "탭 이동 (번호 = 탭 바 표시 순서)"),
    HelpMoveTab => ("Move current tab left / right", "현재 탭 순서 왼쪽·오른쪽 이동"),
    HelpMoveRows => ("Move rows / scroll focused card", "선택 카드의 행 이동·스크롤"),
    HelpWindow => ("Cycle window (today, 1h, 24h, 7d)", "집계 구간 전환 (오늘, 1시간, 24시간, 7일)"),
    HelpSort => ("Next sort column / flip order, reset", "다음 정렬 열 / 방향 전환 → 기본 순환"),
    HelpReportDate => ("Change report date", "리포트 날짜 이동"),
    HelpReportGenerate => ("Generate report (needs serve)", "리포트 생성 (serve 실행 필요)"),
    HelpRefresh => ("Reload settings/report, else refresh", "설정·리포트 다시 불러오기, 그 외 즉시 갱신"),
    HelpReindex => ("Request reindex (needs serve)", "기록 재색인 요청 (serve 실행 필요)"),
    HelpPin => ("Pin window on top on/off", "창 최상단 고정 켜기·끄기"),
    HelpKeyShiftArrows => ("Shift+Arrows", "Shift+방향키"),
    HelpMoveCard => ("Move focused card", "포커스 카드 위치 이동"),
    HelpResetLayout => ("Reset card size, order, widths (tab)", "현재 탭 카드 크기·순서·열 폭 기본 배치"),
    HelpLanguage => ("Switch language (EN / KO)", "표시 언어 전환 (EN / KO)"),
    HelpToggleHelp => ("Open / close help", "도움말 열기·닫기"),
    HelpQuit => ("Quit", "종료"),
    HelpKeyClickTab => ("Click tab/button", "탭·버튼 클릭"),
    HelpClickTab => ("Switch tab, run footer button", "탭 전환, 하단 버튼 실행"),
    HelpKeyClickCard => ("Click card/row", "카드·행 클릭"),
    HelpClickCard => ("Focus card (thick border), select row", "카드 선택(굵은 테두리), 표 행 선택"),
    HelpKeyClickHeader => ("Click table header", "표 머리글 클릭"),
    HelpClickHeader => ("Sort: numbers ↓ first, text ↑, reset", "그 열 정렬: 숫자·시간 ↓ 먼저, 문자 ↑, 세 번째 기본"),
    HelpKeyDragColumn => ("Drag column edge", "열 경계 드래그"),
    HelpDragColumn => ("Resize column, double-click auto width", "머리글 사이를 끌어 폭 조절, 더블클릭 자동 폭"),
    HelpKeyWheel => ("Wheel / edge drag", "휠 / 경계 드래그"),
    HelpWheel => ("Scroll card under pointer / resize cards", "마우스 아래 카드 스크롤 / 카드 크기 조절"),
    HelpKeyLongPress => ("Long-press title+drag", "제목 길게 누름+드래그"),
    HelpLongPress => ("Move card (Esc cancels)", "카드 위치 이동 (Esc 취소)"),
    HelpKeyDragTab => ("Drag tab", "탭 드래그"),
    HelpDragTab => ("Reorder tabs (Esc cancels)", "탭 순서 변경 (Esc 취소)"),
    HelpKeyScrollbar => ("Scrollbar", "스크롤바"),
    HelpScrollbar => ("Drag thumb, click track to page", "썸 드래그 이동, 트랙 클릭 한 화면 이동"),

    // 빈 상태와 안내 카드
    PanelPending => ("Waiting for first poll", "집계 대기 중"),
    PanelPendingHint => ("Shown automatically after the first poll", "첫 집계가 끝나면 자동 표시"),
    NoEvents => ("No events", "이벤트 없음"),
    NoEventsHint => ("Live view starts with a Claude Code session", "Claude Code 세션이 시작되면 실시간 표시"),
    RetryHint => ("Press r to retry", "r 키로 다시 시도"),
    ChangeWindowHint => ("Press w to change window", "w 키로 구간 변경"),
    GateTitle => ("Stats", "집계"),
    GatePending => ("Waiting for first poll", "첫 집계 대기 중"),
    GateNoDb => ("No database", "DB 없음"),
    GateOpenFailed => ("Database open failed", "DB 열기 실패"),
    GateRunServe => ("Run serve or index first", "serve 또는 index 실행 필요"),
    NoHeadings => ("No headings", "제목 없음"),
    NoSavedReport => ("No saved report", "저장된 리포트 없음"),
    ReportGenerateHint => ("Press g or the [Generate] button (needs serve)", "g 키 또는 [리포트 생성] 버튼 (serve 실행 필요)"),
    FailServeOff => ("serve not running", "serve 미실행"),
    FailDbMissing => ("No database: run serve or index first", "DB 없음: serve 또는 index 실행 필요"),

    // 개요 탭
    TitleServerStatus => ("Server status", "서버 상태"),
    ServeRunning => ("serve running", "serve 실행 중"),
    ServeStaleBuild => ("serve on another build · restart it", "serve 빌드 불일치 · 재시작 필요"),
    ServeChecking => ("Checking status", "상태 확인 중"),
    ServeRunHint => ("Run serve as a resident process", "serve 명령으로 상주 실행"),
    StatusUptime => ("Uptime", "가동"),
    StatusVersion => ("Version", "버전"),
    StatusOtlpBatches => ("OTLP batches", "OTLP 배치"),
    StatusIndexRuns => ("Index runs", "색인 횟수"),
    StatusHookCalls => ("Hook calls", "훅 호출"),
    GatewayOn => ("Gateway ON", "게이트웨이 ON"),
    GatewayOff => ("Gateway OFF", "게이트웨이 OFF"),
    TitleSummary => ("Summary", "요약 지표"),
    KpiRequests => ("Requests", "요청"),
    KpiRequestsShort => ("Reqs", "요청"),
    KpiRequestsUnit => ("req", "건"),
    KpiTokens => ("Input/output tokens", "입력/출력 토큰"),
    KpiTokensShort => ("Tokens in/out", "토큰 입/출력"),
    KpiInput => ("In", "입력"),
    KpiOutput => ("Out", "출력"),
    KpiCache => ("Cache hit rate", "캐시 적중률"),
    KpiCacheShort => ("Cache hit", "캐시 적중률"),
    KpiWait => ("Model wait", "모델 대기"),
    KpiWaitShort => ("Wait", "모델 대기"),
    KpiWaitDetail => ("Model reply wait", "모델 응답 대기"),
    KpiWaitDetailShort => ("Reply wait", "응답 대기"),
    KpiTools => ("Tool calls/errors", "도구 호출/오류"),
    KpiToolsShort => ("Tools", "도구"),
    KpiCalls => ("Calls", "호출"),
    KpiErrors => ("Errors", "오류"),
    KpiHooks => ("Hook deny/rewrite", "훅 거부/재작성"),
    KpiHooksShort => ("Hooks", "훅"),
    KpiDeny => ("Deny", "거부"),
    KpiRewrite => ("Rewrite", "재작성"),
    TitleSavings => ("Hook rewrite effect", "훅 재작성 효과"),
    SavingsSaved => ("Saved today", "오늘 절약 시간"),
    SavingsLoss => ("Est. deny loss", "거부 손실 추정"),
    SavingsNet => ("Net effect", "순효과"),
    TitleTrend => ("Per-minute trend", "분당 추이"),
    TitleRequestsPerMin => ("Requests/min", "분당 요청"),
    TitleTokensPerMin => ("Output tokens/min", "분당 출력 토큰"),
    TrendMinutesAgo => ("60 min ago", "60분 전"),
    TrendNow => ("Now", "지금"),
    TitleLiveEvents => ("Live events", "실시간 이벤트"),
    TitleEventDetail => ("Event detail", "이벤트 상세"),

    // 이벤트 피드 열·종류
    FeedTime => ("Time", "시각"),
    FeedKind => ("Kind", "종류"),
    FeedName => ("Name", "이름"),
    FeedAgent => ("Agent", "에이전트"),
    FeedInput => ("In", "입력"),
    FeedOutput => ("Out", "출력"),
    FeedCacheRead => ("CacheRd", "캐시읽기"),
    FeedDuration => ("Dur", "소요"),
    TagApi => ("API ", "API "),
    TagTool => ("Tool", "도구"),
    TagHook => ("Hook", "훅  "),
    TagNotice => ("Note", "알림"),
    FeedError => ("Error", "오류"),

    // 이벤트 상세
    DetailApi => ("API request", "API 요청"),
    DetailTool => ("Tool call", "도구 호출"),
    DetailHook => ("Hook verdict", "훅 판정"),
    DetailNotice => ("Notice", "알림"),
    DetailTime => ("Time", "시각"),
    DetailModel => ("Model", "모델"),
    DetailAgent => ("Agent", "에이전트"),
    DetailInputTokens => ("Input", "입력 토큰"),
    DetailOutputTokens => ("Output", "출력 토큰"),
    DetailCacheRead => ("Cache rd", "캐시 읽기"),
    DetailCacheWrite => ("Cache wr", "캐시 생성"),
    DetailFirstByte => ("1st byte", "첫 바이트"),
    DetailFirstToken => ("1st token", "첫 토큰"),
    DetailSession => ("Session", "세션"),
    DetailDuration => ("Duration", "소요"),
    DetailToolName => ("Tool", "도구"),
    DetailServer => ("Server", "서버"),
    DetailResult => ("Result", "결과"),
    DetailResultOk => ("OK", "정상"),
    DetailCallId => ("Call id", "호출 id"),
    DetailRoute => ("Route", "경로"),
    DetailGuard => ("Guard", "가드"),
    DetailVerdict => ("Verdict", "판정"),
    DetailDetail => ("Detail", "상세"),
    DetailSaved => ("Saved", "절약"),
    DetailLevel => ("Level", "수준"),
    DetailContent => ("Content", "내용"),
    DetailCacheHit => ("Cache hit", "캐시 적중"),
    ContextSameAgent => ("Same agent", "같은 에이전트"),
    ContextSameTool => ("Same tool", "같은 도구"),
    ContextSameGuard => ("Same guard", "같은 가드"),

    // 표 열 머리글
    ColP50 => ("p50", "p50"),
    ColP90 => ("p90", "p90"),
    ColTool => ("Tool", "도구"),
    ColCalls => ("Calls", "호출"),
    ColErrors => ("Errors", "오류"),
    ColTotal => ("Total", "합계"),
    ColRoute => ("Route", "경로"),
    ColGuard => ("Guard", "가드"),
    ColVerdict => ("Verdict", "판정"),
    ColDetail => ("Detail", "상세"),
    ColCount => ("Count", "횟수"),
    ColSaved => ("Saved", "절약"),
    ColModel => ("Model", "모델"),
    ColRequests => ("Reqs", "요청"),
    ColSessions => ("Sess", "세션"),
    ColAgents => ("Agents", "에이전트"),
    ColInput => ("In", "입력"),
    ColOutput => ("Out", "출력"),
    ColCacheRead => ("Cache rd", "캐시읽기"),
    ColCacheWrite => ("Cache wr", "캐시생성"),
    ColHit => ("Hit", "적중"),
    ColCostEst => ("Cost est.", "비용추정"),
    ColLast => ("Last", "최근"),
    ColAgent => ("Agent", "에이전트"),
    ColRuns => ("Runs", "실행"),
    ColTools => ("Tools", "도구"),
    ColTopModel => ("Top model", "주 모델"),
    ColTime => ("Time", "시각"),
    ColNewCache => ("New cache", "신규 캐시"),
    ColPrevContext => ("Prev ctx", "직전 문맥"),
    ColGap => ("Gap", "간격"),
    ColLikelyCause => ("Likely cause", "추정 원인"),

    // 도구·훅 탭
    TitleToolLatency => ("Tool latency", "도구 지연"),
    TitleToolDetail => ("Tool detail", "도구 상세"),
    NoToolSelected => ("No tool selected", "선택한 도구 없음"),
    ToolCallsErrors => ("Calls / errors", "호출 / 오류"),
    LatencyP50P90 => ("Latency p50/p90", "지연 p50 / p90"),
    ToolTotalTime => ("Total time", "합계 시간"),
    ToolAvgPerCall => ("Avg per call", "호출당 평균"),
    ToolShare => ("Share of total", "전체 대비"),
    TitleHookVerdicts => ("Hook verdicts", "훅 판정"),
    TitleHookDetail => ("Hook detail", "훅 상세"),
    NoHookSelected => ("No hook selected", "선택한 훅 없음"),
    HookRouteGuard => ("Route / guard", "경로 / 가드"),
    HookTimeSaved => ("Time saved", "절약 시간"),

    // 모델·에이전트 탭
    NoModelSelected => ("No model selected", "선택한 모델 없음"),
    TitleModelDetail => ("Model detail", "모델 상세"),
    TitleModelStatsPending => ("Model stats (est.)", "모델 통계 (추정)"),
    TitleCacheBreaks => ("Cache-break candidates", "캐시 붕괴 후보"),
    ModelCounts => ("Reqs / sess / kinds", "요청 / 세션 / 종류"),
    InOut => ("Input / output", "입력 / 출력"),
    CacheReadWrite => ("Cache read / write", "캐시 읽기 / 생성"),
    WaitP50P90 => ("Wait p50 / p90", "대기 p50 / p90"),
    CostEstimate => ("Cost (est.)", "비용 (추정)"),
    LastRequest => ("Last request", "마지막 요청"),
    NoAgentSelected => ("No agent selected", "선택한 에이전트 없음"),
    TitleAgentDetail => ("Agent detail", "에이전트 상세"),
    TitleAgentStatsPending => ("Agent stats (est.)", "에이전트 통계 (추정)"),
    AgentCounts => ("Runs / reqs / sess", "실행 / 요청 / 세션"),
    AgentToolCalls => ("Tool calls / errors", "도구 호출 / 오류"),
    AgentTopLast => ("Top model / last req", "주 모델 / 마지막 요청"),

    // 설정 탭
    TitleHooksRegistered => ("Hooks registered", "훅 등록"),
    TitleMcpServers => ("MCP servers", "MCP 서버"),
    TitleAgentsSkills => ("Agents, skills, env", "에이전트·스킬·env"),
    TitleDiagnostics => ("Diagnostics", "진단"),
    HintRefresh => ("r Refresh", "r 새로고침"),
    SetEvent => ("Event", "이벤트"),
    SetMatcher => ("matcher", "matcher"),
    SetType => ("Type", "종류"),
    SetTarget => ("Target", "대상"),
    SetAsync => ("async", "async"),
    SetTimeout => ("timeout", "timeout"),
    SetEffort => ("effort", "effort"),
    SetYes => ("Yes", "예"),
    SetStatus => ("State", "상태"),
    SetName => ("Name", "이름"),
    SetTransport => ("Transport", "전송"),
    SetBlocked => ("Deny", "차단"),
    SetAllowed => ("Allow", "허용"),
    SetModel => ("Model", "모델"),
    SetMaxTurns => ("Turns", "최대턴"),
    SetLevel => ("Level", "수준"),
    SetTopic => ("Topic", "주제"),
    SetMessage => ("Message", "내용"),
    DoctorOk => ("[ok]", "[정상]"),
    DoctorWarn => ("[warn]", "[경고]"),
    DoctorFail => ("[fail]", "[실패]"),
    DoctorOther => ("[misc]", "[기타]"),
    SettingsSkills => ("Skills", "스킬"),

    // 리포트 탭
    ReportPrev => ("Prev", "이전"),
    ReportNext => ("Next", "다음"),
    ReportGenerate => ("Generate", "리포트 생성"),
    DayToday => ("today", "오늘"),
    DayYesterday => ("yesterday", "어제"),
    TitleOutline => ("Outline", "목차"),

    // 알림
    ToastLanguage => ("Language: English", "언어: 한국어"),
    ToastPinned => ("Window pinned on top", "창 최상단 고정"),
    ToastUnpinned => ("Window unpinned", "창 최상단 고정 해제"),
    ToastLayoutReset => ("Card size, order, widths reset", "카드 크기·순서·열 폭 기본 배치 복원"),
    ToastCardMoved => ("Card moved (l resets layout)", "카드 위치 변경 (l 기본 배치)"),
    ToastTabMoved => ("Tab order changed", "탭 순서 변경"),
    ToastSplitDrag => ("Dragging card edge: release to keep, l resets layout", "카드 경계 드래그: 놓으면 고정, l 기본 배치"),
    ToastColumnsAuto => ("Column widths reset to auto", "열 폭 자동 복원"),
    ToastColumnDrag => ("Dragging column edge: resize, double-click for auto", "열 경계 드래그: 열 폭 조절, 더블클릭 자동 폭"),
    ToastReindexRequested => ("Reindex requested", "재색인 요청"),
    ToastNeedServe => ("Available after running serve", "serve 실행 후 사용 가능"),
    AdminReindex => ("Reindex", "재색인"),
    AdminGenerateReport => ("Generate report", "리포트 생성"),

    // 프로젝트 탭
    TabProjects => ("Projects", "프로젝트"),
    ColProject => ("Project", "프로젝트"),
    ColBranch => ("Branch", "브랜치"),
    ColCacheWrite5m => ("Cache wr 5m", "캐시생성 5m"),
    ColCacheWrite1h => ("Cache wr 1h", "캐시생성 1h"),
    ColName => ("Name", "이름"),
    ColCost => ("Cost", "비용"),
    TitleProjectStatsPending => ("Project stats (est.)", "프로젝트 통계 (추정)"),
    TitleProjectDetail => ("Project detail", "프로젝트 상세"),
    NoProjectSelected => ("No project selected", "선택한 프로젝트 없음"),
    ProjectCounts => ("Reqs / sess", "요청 / 세션"),
    ProjectCache => ("Cache rd / wr 5m / 1h", "캐시 읽기/생성 5m/1h"),
    TitleAttributionPending => ("Cost attribution (OTel)", "비용 귀속 (OTel)"),
    DimensionSkill => ("Skill", "스킬"),
    DimensionMcpServer => ("MCP server", "MCP 서버"),
    DimensionPlugin => ("Plugin", "플러그인"),
    DimensionAgent => ("Agent", "에이전트"),
    DimensionModel => ("Model", "모델"),
    DimensionQuerySource => ("Query source", "호출 출처"),
    AttributionNoMetrics => ("No OTel metrics in this window", "이 구간에 OTel 지표 없음"),
    AttributionEnableHint => ("Set OTEL_METRICS_EXPORTER=otlp to collect", "OTEL_METRICS_EXPORTER=otlp 설정 시 수집"),
    AttributionNoDimension => ("No data for this dimension", "이 차원의 데이터 없음"),
    AttributionDimensionHint => ("Press d to change dimension", "d 키로 차원 변경"),
    HintDimension => ("d Dimension", "d 차원"),
    HelpDimension => ("Cycle attribution dimension (Projects tab)", "귀속 차원 전환 (프로젝트 탭)"),

    // 스킬 탭
    TabSkills => ("Skills", "스킬"),
    ColSkill => ("Skill", "스킬"),
    ColInstalled => ("Inst", "설치"),
    ColTopCaller => ("Caller", "주 호출"),
    ColDescChars => ("Desc", "설명"),
    TitleSkillsPending => ("Skills", "스킬"),
    TitleSkillDetail => ("Skill detail", "스킬 상세"),
    NoSkillSelected => ("No skill selected", "선택한 스킬 없음"),
    HintSkillSource => ("Skill tool calls", "Skill 도구 호출 기준"),
    SkillsEmpty => ("No skills installed or called", "설치·호출된 스킬 없음"),
    SkillsEmptyHint => ("Add skills under ~/.claude/skills or call one", "~/.claude/skills 에 추가하거나 호출하면 표시"),
    SkillUnnamed => ("(name not recorded)", "(이름 미기록)"),
    SkillCalls => ("Calls / errors / sess", "호출 / 오류 / 세션"),
    SkillShare => ("Share of skill calls", "스킬 호출 점유"),
    SkillFirstLast => ("First / last call", "처음 / 마지막 호출"),
    SkillInstalled => ("Installed at", "설치 위치"),
    SkillNotInstalled => ("not in ~/.claude/skills (call record only)", "~/.claude/skills 에 없음 (호출 기록만)"),
    SkillInventoryPending => ("reading skill files…", "스킬 파일 읽는 중…"),
    SkillDescription => ("Description length", "설명 길이"),
    SkillCostOtel => ("Cost (OTel)", "비용 (OTel)"),
    SkillNoOtel => ("no OTel metric for this skill", "이 스킬의 OTel 지표 없음"),
    SkillNeverCalled => ("not called in this window", "이 구간에 호출 없음"),
    SkillCallError => ("error", "오류"),

    // 모델·에이전트·프로젝트 상세 보강 (항목 이름 22칸 이내)
    DetailPerRequest => ("Cost / out per req", "요청당 비용 / 출력"),
    DetailWindowShare => ("Share of window", "구간 점유"),
    DetailCacheSplit => ("Cache wr 5m / 1h", "캐시생성 5m / 1h"),
    DetailFirstSeen => ("First seen", "처음 기록"),
    DetailFirstLast => ("First / last request", "처음 / 마지막 요청"),
    DetailPerRun => ("Reqs / cost per run", "실행당 요청 / 비용"),
    DetailPerSession => ("Reqs / cost per sess", "세션당 요청 / 비용"),
    DetailConfigured => ("Configured", "설정값"),
    DetailConfigMain => ("main session (no agent file)", "메인 세션 (정의 파일 없음)"),
    DetailConfigMissing => ("no file in ~/.claude/agents", "~/.claude/agents 에 정의 없음"),
    DetailConfigPending => ("reading agent files…", "에이전트 정의 읽는 중…"),
    SectionAgentsByCost => ("Agents by cost", "에이전트별 비용"),
    SectionModelsByCost => ("Models by cost", "모델별 비용"),
    SectionToolsByCalls => ("Tools by calls", "도구별 호출"),
    SectionSessionsByCost => ("Sessions by cost", "세션별 비용"),
    SectionRecentRuns => ("Recent runs", "최근 실행"),
    SectionCallers => ("Callers", "호출 에이전트"),
    SectionRecentCalls => ("Recent calls", "최근 호출"),
    // 피드: OTLP 신규 이벤트 태그(4칸 이하)·상세 제목·항목·낱말
    TagSubagent => ("Sub ", "서브"),
    TagCompaction => ("Comp", "압축"),
    TagApiError => ("Err ", "오류"),
    TagDecision => ("Perm", "권한"),
    TagMode => ("Mode", "모드"),
    FeedFailed => ("failed", "실패"),
    DetailCompaction => ("Compaction", "컴팩션"),
    DetailSubagent => ("Subagent", "서브에이전트"),
    DetailApiError => ("API error", "API 오류"),
    DetailApiRetries => ("API retries", "API 재시도"),
    DetailDecision => ("Tool decision", "도구 권한 결정"),
    DetailPermMode => ("Permission mode", "권한 모드 변경"),
    DetailTrigger => ("Trigger", "트리거"),
    DetailBefore => ("Before", "직전 토큰"),
    DetailAfter => ("After", "이후 토큰"),
    DetailType => ("Type", "유형"),
    DetailMode => ("Mode", "모드"),
    DetailStarted => ("Started", "시작"),
    DetailEnded => ("Ended", "종료"),
    DetailTokens => ("Tokens", "토큰"),
    DetailToolUses => ("Tools", "도구 호출"),
    DetailStatusCode => ("Status", "상태"),
    DetailAttempt => ("Attempt", "시도"),
    DetailRetryTime => ("Retry time", "대기 시간"),
    DetailDecisionValue => ("Decision", "결정"),
    DetailSource => ("Source", "출처"),
    DetailFrom => ("From", "이전"),
    DetailTo => ("To", "이후"),

    // 리포트 탭 주간 보기
    ReportViewDaily => ("Daily", "일간"),
    ReportViewWeekly => ("Weekly", "주간"),
    WeekThis => ("this week", "이번 주"),
    WeekLast => ("last week", "지난주"),
    HintReportView => ("v Daily/Weekly", "v 일간·주간"),
    HintReportWeek => ("←→ Week", "←→ 주"),
    HelpReportView => ("Daily / weekly report (← → then moves by week)", "일간·주간 리포트 전환 (주간에서 ← → 는 주 이동)"),

    // 한도 카드 (개요 탭), 캐시 타이머 카드 (모델 탭)
    TitleLimits => ("Limits", "한도"),
    TitleCacheTimer => ("Cache timer", "캐시 타이머"),
    LimitsLabelFiveHour => ("5h", "5h"),
    LimitsLabelSevenDay => ("7d", "7d"),
    LimitsLabelContext => ("ctx", "ctx"),
    LimitsLabelApiKey => ("API", "API"),
    LimitsPaceAhead => ("ahead", "빠름"),
    LimitsPaceOn => ("on pace", "적정"),
    LimitsPaceBehind => ("behind", "느림"),
    LimitsOfficial => ("official", "공식"),
    LimitsEstimate => ("estimate", "추정"),
    LimitsTokensUnit => ("tok", "tok"),
    LimitsRequestsShort => ("req", "req"),
    LimitsEmpty => ("No official limit data", "공식 한도 값 없음"),
    LimitsEmptyHint => ("Set statusLine to `claude-monitor statusline`", "statusLine 에 `claude-monitor statusline` 설정"),
    LimitsResetDue => ("reset due", "리셋 예정"),
    CacheWarm => ("warm", "유지"),
    CacheCold => ("cold", "만료"),
    CacheUnknown => ("not observed", "관측 없음"),
    CacheHit => ("hit", "적중"),
    CacheNoMiss => ("no miss recorded", "기록된 미스 없음"),
    CacheEmpty => ("No cache data", "캐시 값 없음"),
}

// 3. 값이 들어가는 문구 -------------------------------------------------------
// 낱말 순서가 언어마다 달라서 조립은 모두 여기서
pub fn too_small(lang: Lang, min_width: u16, min_height: u16, width: u16, height: u16) -> String {
    match lang {
        Lang::En => {
            format!("Terminal too small (min {min_width}x{min_height}, now {width}x{height})")
        }
        Lang::Ko => {
            format!("터미널 크기 부족 (최소 {min_width}x{min_height}, 현재 {width}x{height})")
        }
    }
}

pub fn stale(lang: Lang, age_secs: i64) -> String {
    match lang {
        Lang::En => format!("Stale {age_secs}s"),
        Lang::Ko => format!("갱신 지연 {age_secs}s"),
    }
}

pub fn card_moving(lang: Lang, title: &str) -> String {
    match lang {
        Lang::En => format!("Moving · {title}"),
        Lang::Ko => format!("이동 · {title}"),
    }
}

pub fn sort_title(lang: Lang, title: &str, label: &str, arrow: &str) -> String {
    match lang {
        Lang::En => format!("{title} · Sort {label}{arrow}"),
        Lang::Ko => format!("{title} · 정렬 {label}{arrow}"),
    }
}

pub fn no_records_in_window(lang: Lang, window: &str) -> String {
    match lang {
        Lang::En => format!("No records in selected window ({window})"),
        Lang::Ko => format!("선택 구간({window})에 기록 없음"),
    }
}

pub fn live_events_title(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => format!("Live events ({count})"),
        Lang::Ko => format!("실시간 이벤트 ({count}건)"),
    }
}

pub fn feed_hint(lang: Lang, api: usize, tool: usize, hook: usize) -> String {
    match lang {
        Lang::En => format!("API {api} · Tools {tool} · Hooks {hook}"),
        Lang::Ko => format!("API {api} · 도구 {tool} · 훅 {hook}"),
    }
}

pub fn trend_title(lang: Lang, title: &str, peak: &str) -> String {
    match lang {
        Lang::En => format!("{title} (peak {peak})"),
        Lang::Ko => format!("{title} (최대 {peak})"),
    }
}

pub fn sessions_detail(lang: Lang, sessions: u64) -> String {
    match lang {
        Lang::En => format!("Sessions {sessions}"),
        Lang::Ko => format!("세션 {sessions}"),
    }
}

pub fn cache_write_detail(lang: Lang, tokens: &str, short: bool) -> String {
    match (lang, short) {
        (Lang::En, false) => format!("Cache write {tokens}"),
        (Lang::En, true) => format!("Write {tokens}"),
        (Lang::Ko, false) => format!("캐시 생성 {tokens}"),
        (Lang::Ko, true) => format!("생성 {tokens}"),
    }
}

pub fn cache_read_detail(lang: Lang, tokens: &str) -> String {
    match lang {
        Lang::En => format!("Read {tokens}"),
        Lang::Ko => format!("읽기 {tokens}"),
    }
}

pub fn error_rate_detail(lang: Lang, rate: f64) -> String {
    match lang {
        Lang::En => format!("Error rate {rate:.1}%"),
        Lang::Ko => format!("오류율 {rate:.1}%"),
    }
}

pub fn rewrite_share_detail(lang: Lang, share: &str, short: bool) -> String {
    match (lang, short) {
        (Lang::En, false) => format!("Rewrite share {share}"),
        (Lang::En, true) => format!("Share {share}"),
        (Lang::Ko, false) => format!("재작성 비율 {share}"),
        (Lang::Ko, true) => format!("비율 {share}"),
    }
}

pub fn tool_table_title(lang: Lang, kinds: usize) -> String {
    match lang {
        Lang::En => format!("Tool latency · {kinds} kinds"),
        Lang::Ko => format!("도구 지연 · {kinds}종"),
    }
}

pub fn tool_calls_value(lang: Lang, calls: &str, errors: &str, rate: f64) -> String {
    match lang {
        Lang::En => format!("{calls} / {errors}  (error rate {rate:.1}%)"),
        Lang::Ko => format!("{calls} / {errors}  (오류율 {rate:.1}%)"),
    }
}

pub fn hook_table_title(lang: Lang, denials: &str, rewrites: &str, saved: &str) -> String {
    match lang {
        Lang::En => format!("Hook verdicts · Deny {denials} · Rewrite {rewrites} · Saved {saved}"),
        Lang::Ko => format!("훅 판정 · 거부 {denials} · 재작성 {rewrites} · 절약 {saved}"),
    }
}

pub fn count_with_share(lang: Lang, count: &str, share: f64) -> String {
    match lang {
        Lang::En => format!("{count}  ({share:.1}% of total)"),
        Lang::Ko => format!("{count}  (전체 대비 {share:.1}%)"),
    }
}

pub fn model_table_title(lang: Lang, cost: f64) -> String {
    match lang {
        Lang::En => format!("Model stats · est. (API rates) · total ${cost:.2}"),
        Lang::Ko => format!("모델 통계 · 추정 (API 단가) · 합계 ${cost:.2}"),
    }
}

pub fn agent_table_title(lang: Lang, kinds: usize, cost: f64) -> String {
    match lang {
        Lang::En => format!("Agent stats · {kinds} kinds · est. (API rates) · total ${cost:.2}"),
        Lang::Ko => format!("에이전트 통계 · {kinds}종 · 추정 (API 단가) · 합계 ${cost:.2}"),
    }
}

pub fn cache_breaks_title(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => format!("Cache-break candidates · {count}"),
        Lang::Ko => format!("캐시 붕괴 후보 · {count}건"),
    }
}

pub fn cache_read_write_value(lang: Lang, read: &str, write: &str, hit: f64) -> String {
    match lang {
        Lang::En => format!("{read} / {write}  (hit {hit:.1}%)"),
        Lang::Ko => format!("{read} / {write}  (적중 {hit:.1}%)"),
    }
}

pub fn cost_value(lang: Lang, cost: f64) -> String {
    match lang {
        Lang::En => format!("${cost:.2}  (at API rates)"),
        Lang::Ko => format!("${cost:.2}  (API 단가 기준)"),
    }
}

pub fn settings_count(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => format!(" · {count}"),
        Lang::Ko => format!(" · {count}건"),
    }
}

pub fn doctor_summary(lang: Lang, failed: usize, warned: usize) -> String {
    match lang {
        Lang::En => format!(" · Fail {failed} · Warn {warned}"),
        Lang::Ko => format!(" · 실패 {failed} · 경고 {warned}"),
    }
}

pub fn allowlist_heading(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => format!("HTTP hook allowlist ({count})"),
        Lang::Ko => format!("HTTP 훅 허용 목록 ({count}건)"),
    }
}

pub fn skills_summary(lang: Lang, count: usize, chars: &str) -> String {
    match lang {
        Lang::En => format!("{count} · description total {chars} chars"),
        Lang::Ko => format!("{count}개 · 설명 합계 {chars}자"),
    }
}

pub fn env_keys_heading(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => format!("env keys ({count})"),
        Lang::Ko => format!("env 키 ({count}개)"),
    }
}

pub fn daily_report_title(lang: Lang, date: &str) -> String {
    match lang {
        Lang::En => format!("Daily report · {date}"),
        Lang::Ko => format!("일일 리포트 · {date}"),
    }
}

pub fn event_error(lang: Lang, kind: &str) -> String {
    let word = text(lang, Msg::FeedError);
    if kind.is_empty() {
        word.to_string()
    } else {
        format!("{word} {kind}")
    }
}

pub fn context_summary_api(
    lang: Lang,
    api: usize,
    tool: usize,
    error: usize,
    average: &str,
) -> String {
    match lang {
        Lang::En => format!("API {api} · Tools {tool} · Errors {error} · Avg dur {average}"),
        Lang::Ko => format!("API {api} · 도구 {tool} · 오류 {error} · 평균 소요 {average}"),
    }
}

pub fn context_summary_tool(lang: Lang, tool: usize, error: usize, average: &str) -> String {
    match lang {
        Lang::En => format!("Calls {tool} · Errors {error} · Avg dur {average}"),
        Lang::Ko => format!("호출 {tool} · 오류 {error} · 평균 소요 {average}"),
    }
}

pub fn context_summary_hook(lang: Lang, hook: usize, denial: usize, rewrite: usize) -> String {
    match lang {
        Lang::En => format!("Hooks {hook} · Deny {denial} · Rewrite {rewrite}"),
        Lang::Ko => format!("훅 {hook} · 거부 {denial} · 재작성 {rewrite}"),
    }
}

// 도구 호출 건수 (서브에이전트 이름 칸)
pub fn tools_count_text(lang: Lang, count: i64) -> String {
    match lang {
        Lang::En => format!("{count} tools"),
        Lang::Ko => format!("도구 {count}"),
    }
}

// API 오류 시도 번호 (이름 칸)
pub fn attempt_text(lang: Lang, attempt: i64) -> String {
    match lang {
        Lang::En => format!("attempt {attempt}"),
        Lang::Ko => format!("{attempt}회차"),
    }
}

// 재시도 소진 (이름 칸, tries 는 표시된 횟수 또는 -)
pub fn retries_exhausted_text(lang: Lang, tries: &str) -> String {
    match lang {
        Lang::En => format!("retries exhausted · {tries} tries"),
        Lang::Ko => format!("재시도 소진 · {tries}회"),
    }
}

pub fn weekly_report_title(lang: Lang, week_key: &str) -> String {
    match lang {
        Lang::En => format!("Weekly report · {week_key}"),
        Lang::Ko => format!("주간 리포트 · {week_key}"),
    }
}

// 훅 판정 문자열 -> 라벨 (모르는 값은 -)
pub fn outcome_label(lang: Lang, outcome: &str) -> &'static str {
    match (lang, outcome) {
        (Lang::En, "allow") => "Allow",
        (Lang::En, "deny") => "Deny",
        (Lang::En, "rewrite") => "Rewrite",
        (Lang::En, "error") => "Error",
        (Lang::Ko, "allow") => "허용",
        (Lang::Ko, "deny") => "거부",
        (Lang::Ko, "rewrite") => "재작성",
        (Lang::Ko, "error") => "오류",
        _ => "-",
    }
}

// 4. 알림 문구 ----------------------------------------------------------------
pub fn report_requested(lang: Lang, date: &str) -> String {
    match lang {
        Lang::En => format!("Report generation requested {date}"),
        Lang::Ko => format!("리포트 생성 요청 {date}"),
    }
}

pub fn card_resized(lang: Lang, axis: SplitAxis, before: u16, after: u16) -> String {
    match (lang, axis) {
        (Lang::En, SplitAxis::Columns) => {
            format!("Card size {before} : {after} cols (l resets layout)")
        }
        (Lang::En, SplitAxis::Rows) => {
            format!("Card size {before} : {after} rows (l resets layout)")
        }
        (Lang::Ko, SplitAxis::Columns) => format!("카드 크기 {before}열 : {after}열 (l 기본 배치)"),
        (Lang::Ko, SplitAxis::Rows) => format!("카드 크기 {before}행 : {after}행 (l 기본 배치)"),
    }
}

pub fn column_width(lang: Lang, width: u16) -> String {
    match lang {
        Lang::En => format!("Column width {width} cells (double-click for auto)"),
        Lang::Ko => format!("열 폭 {width}칸 (더블클릭 자동 폭)"),
    }
}

// 관리 요청 결과 -> (알림 문구, 수준)
pub fn admin_toast(
    lang: Lang,
    action: AdminAction,
    outcome: &AdminOutcome,
) -> (String, ToastLevel) {
    let label = match action {
        AdminAction::Reindex => text(lang, Msg::AdminReindex),
        AdminAction::GenerateReport(_) => text(lang, Msg::AdminGenerateReport),
        AdminAction::GenerateWeeklyReport(_) => text(lang, Msg::AdminGenerateReport),
    };
    match (lang, outcome) {
        (_, AdminOutcome::NoToken) => (
            text(lang, Msg::ToastNeedServe).to_string(),
            ToastLevel::Error,
        ),
        (Lang::En, AdminOutcome::Done(reply)) => {
            (format!("{label} done {reply}"), ToastLevel::Success)
        }
        (Lang::Ko, AdminOutcome::Done(reply)) => {
            (format!("{label} 완료 {reply}"), ToastLevel::Success)
        }
        (Lang::En, AdminOutcome::Failed(reason)) => (
            format!("{label} failed: {}", failure_text(lang, reason)),
            ToastLevel::Error,
        ),
        (Lang::Ko, AdminOutcome::Failed(reason)) => (
            format!("{label} 실패: {}", failure_text(lang, reason)),
            ToastLevel::Error,
        ),
        (Lang::En, AdminOutcome::Waiting) => (format!("{label} pending"), ToastLevel::Info),
        (Lang::Ko, AdminOutcome::Waiting) => (format!("{label} 대기"), ToastLevel::Info),
    }
}

// 5. 패널 실패 ----------------------------------------------------------------
// 오류 본문은 데이터라 번역하지 않음
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    ServeOff,
    DbOpenFailed,
    DbMissing,
    Internal(String),
    Error(String),
}

pub fn failure_text(lang: Lang, failure: &Failure) -> String {
    match (lang, failure) {
        (_, Failure::ServeOff) => text(lang, Msg::FailServeOff).to_string(),
        (_, Failure::DbOpenFailed) => text(lang, Msg::GateOpenFailed).to_string(),
        (_, Failure::DbMissing) => text(lang, Msg::FailDbMissing).to_string(),
        (Lang::En, Failure::Internal(message)) => format!("Internal error: {message}"),
        (Lang::Ko, Failure::Internal(message)) => format!("내부 오류: {message}"),
        (_, Failure::Error(message)) => message.clone(),
    }
}

// 6. 상태 파일 알림 -----------------------------------------------------------
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateIssue {
    UnsupportedVersion(i64),
    Detail(String),
}

pub fn state_unreadable(lang: Lang, file: &str, issue: &StateIssue) -> String {
    match (lang, issue) {
        (Lang::En, StateIssue::UnsupportedVersion(version)) => {
            format!("{file} read failed, using defaults (unsupported version {version})")
        }
        (Lang::Ko, StateIssue::UnsupportedVersion(version)) => {
            format!("{file} 읽기 실패, 기본값 사용 (지원하지 않는 version {version})")
        }
        (Lang::En, StateIssue::Detail(reason)) => {
            format!("{file} read failed, using defaults ({reason})")
        }
        (Lang::Ko, StateIssue::Detail(reason)) => {
            format!("{file} 읽기 실패, 기본값 사용 ({reason})")
        }
    }
}

pub fn state_entries_skipped(lang: Lang, file: &str, skipped: usize) -> String {
    match lang {
        Lang::En => format!("{file} skipped {skipped} entries, using defaults"),
        Lang::Ko => format!("{file} 일부 항목 무시 {skipped}건, 기본값 사용"),
    }
}

pub fn state_save_failed(lang: Lang, error: &str) -> String {
    match lang {
        Lang::En => format!("Saving UI state failed: {error}"),
        Lang::Ko => format!("화면 상태 저장 실패: {error}"),
    }
}

pub fn schema_upgrade_failed(lang: Lang, error: &str) -> String {
    match lang {
        Lang::En => format!("DB schema upgrade failed: {error}"),
        Lang::Ko => format!("DB 스키마 갱신 실패: {error}"),
    }
}

pub fn serve_stale_build(lang: Lang, pid: u32) -> String {
    match lang {
        Lang::En => format!(
            "serve (pid {pid}) runs another build: restart serve to collect with this build"
        ),
        Lang::Ko => {
            format!("serve(pid {pid})가 다른 빌드로 실행 중: serve 를 재시작해야 이 빌드로 수집함")
        }
    }
}

pub fn project_table_title(lang: Lang, count: usize, cost: f64) -> String {
    match lang {
        Lang::En => format!("Projects · {count} · est. (API rates) · total ${cost:.2}"),
        Lang::Ko => format!("프로젝트 · {count}개 · 추정 (API 단가) · 합계 ${cost:.2}"),
    }
}

pub fn attribution_title(lang: Lang, dimension: &str, count: usize) -> String {
    match lang {
        Lang::En => format!("Attribution · by {dimension} · {count} names · OTel"),
        Lang::Ko => format!("귀속 · {dimension} 기준 · {count}개 · OTel"),
    }
}

// 스킬 탭·상세 카드 보강 문구 (이름·모델·시각은 데이터라 번역하지 않음)
pub fn skill_table_title(lang: Lang, installed: Option<usize>, used: usize, calls: u64) -> String {
    match (lang, installed) {
        (Lang::En, Some(installed)) => {
            format!("Skills · {installed} installed · {used} used · {calls} calls")
        }
        (Lang::En, None) => format!("Skills · {used} used · {calls} calls"),
        (Lang::Ko, Some(installed)) => {
            format!("스킬 · 설치 {installed}개 · 사용 {used}개 · 호출 {calls}회")
        }
        (Lang::Ko, None) => format!("스킬 · 사용 {used}개 · 호출 {calls}회"),
    }
}

pub fn skill_calls_value(
    lang: Lang,
    calls: &str,
    errors: &str,
    sessions: &str,
    rate: f64,
) -> String {
    match lang {
        Lang::En => format!("{calls} / {errors} / {sessions}  (error {rate:.1}%)"),
        Lang::Ko => format!("{calls} / {errors} / {sessions}  (오류율 {rate:.1}%)"),
    }
}

pub fn chars_value(lang: Lang, chars: &str) -> String {
    match lang {
        Lang::En => format!("{chars} chars"),
        Lang::Ko => format!("{chars}자"),
    }
}

pub fn otel_cost_value(lang: Lang, cost: f64, input: &str, output: &str) -> String {
    match lang {
        Lang::En => format!("${cost:.2}  (in {input} / out {output})"),
        Lang::Ko => format!("${cost:.2}  (입력 {input} / 출력 {output})"),
    }
}

pub fn window_share_value(lang: Lang, requests: f64, cost: f64) -> String {
    match lang {
        Lang::En => format!("req {requests:.1}% · cost {cost:.1}%"),
        Lang::Ko => format!("요청 {requests:.1}% · 비용 {cost:.1}%"),
    }
}

pub fn agent_config_value(lang: Lang, model: &str, effort: &str, turns: &str) -> String {
    match lang {
        Lang::En => format!("model {model} · effort {effort} · turns {turns}"),
        Lang::Ko => format!("모델 {model} · effort {effort} · 최대턴 {turns}"),
    }
}

pub fn share_cost_value(lang: Lang, count: &str, cost: f64, share: f64) -> String {
    match lang {
        Lang::En => format!("{count} req · ${cost:.2} · {share:.0}%"),
        Lang::Ko => format!("요청 {count} · ${cost:.2} · {share:.0}%"),
    }
}

pub fn share_calls_value(lang: Lang, count: &str, errors: &str, share: f64) -> String {
    match lang {
        Lang::En => format!("{count} calls · {errors} err · {share:.0}%"),
        Lang::Ko => format!("호출 {count} · 오류 {errors} · {share:.0}%"),
    }
}

pub fn run_value(lang: Lang, model: &str, requests: &str, cost: f64, span: &str) -> String {
    match lang {
        Lang::En => format!("{model} · {requests} req · ${cost:.2} · {span}"),
        Lang::Ko => format!("{model} · 요청 {requests} · ${cost:.2} · {span}"),
    }
}

// 7. 도움말 줄 ----------------------------------------------------------------
// (키, 뜻) 쌍, 뜻이 빈 줄은 구역 제목, 키 글자는 번역하지 않음
pub fn help_rows(lang: Lang) -> Vec<(&'static str, &'static str)> {
    let words = |msg: Msg| text(lang, msg);
    vec![
        (words(Msg::HelpSectionKeyboard), ""),
        ("1-9, Tab, Shift-Tab", words(Msg::HelpSwitchTab)),
        ("<  >", words(Msg::HelpMoveTab)),
        ("↑ ↓ PgUp PgDn Home End", words(Msg::HelpMoveRows)),
        ("w", words(Msg::HelpWindow)),
        ("s / S", words(Msg::HelpSort)),
        ("←  →", words(Msg::HelpReportDate)),
        ("g", words(Msg::HelpReportGenerate)),
        ("v", words(Msg::HelpReportView)),
        ("r", words(Msg::HelpRefresh)),
        ("i", words(Msg::HelpReindex)),
        ("t", words(Msg::HelpPin)),
        (words(Msg::HelpKeyShiftArrows), words(Msg::HelpMoveCard)),
        ("l", words(Msg::HelpResetLayout)),
        ("L", words(Msg::HelpLanguage)),
        ("d", words(Msg::HelpDimension)),
        ("?", words(Msg::HelpToggleHelp)),
        ("q, Esc, Ctrl+C", words(Msg::HelpQuit)),
        (words(Msg::HelpSectionMouse), ""),
        (words(Msg::HelpKeyClickTab), words(Msg::HelpClickTab)),
        (words(Msg::HelpKeyClickCard), words(Msg::HelpClickCard)),
        (words(Msg::HelpKeyClickHeader), words(Msg::HelpClickHeader)),
        (words(Msg::HelpKeyDragColumn), words(Msg::HelpDragColumn)),
        (words(Msg::HelpKeyWheel), words(Msg::HelpWheel)),
        (words(Msg::HelpKeyLongPress), words(Msg::HelpLongPress)),
        (words(Msg::HelpKeyDragTab), words(Msg::HelpDragTab)),
        (words(Msg::HelpKeyScrollbar), words(Msg::HelpScrollbar)),
    ]
}

// 한도·캐시 카드 문구 (카운트다운·시각·숫자는 데이터라 번역하지 않음)
pub fn limits_hint(lang: Lang, source: &str, age: &str, stale: bool) -> String {
    match (lang, stale) {
        (Lang::En, false) => format!("{source} · {age} ago"),
        (Lang::En, true) => format!("{source} · stale {age}"),
        (Lang::Ko, false) => format!("{source} · {age} 전"),
        (Lang::Ko, true) => format!("{source} · 지연 {age}"),
    }
}

pub fn limits_reset_in(lang: Lang, countdown: &str) -> String {
    match lang {
        Lang::En => format!("in {countdown}"),
        Lang::Ko => format!("{countdown} 후"),
    }
}

pub fn limits_compactions(lang: Lang, count: u64) -> String {
    match lang {
        Lang::En => format!("compact {count}"),
        Lang::Ko => format!("압축 {count}회"),
    }
}

pub fn cache_left(lang: Lang, countdown: &str) -> String {
    match lang {
        Lang::En => format!("{countdown} left"),
        Lang::Ko => format!("{countdown} 남음"),
    }
}

pub fn cache_expired_ago(lang: Lang, countdown: &str) -> String {
    match lang {
        Lang::En => format!("expired {countdown} ago"),
        Lang::Ko => format!("{countdown} 전 만료"),
    }
}

pub fn cache_misses_value(lang: Lang, misses: i64, requests: i64) -> String {
    match lang {
        Lang::En => format!("misses {misses}/{requests}"),
        Lang::Ko => format!("미스 {misses}/{requests}"),
    }
}

pub fn cache_last_miss(lang: Lang, clock: &str, ago: &str) -> String {
    match lang {
        Lang::En => format!("last miss {clock} ({ago} ago)"),
        Lang::Ko => format!("최근 미스 {clock} ({ago} 전)"),
    }
}

pub fn cache_miss_cause(lang: Lang, cause: &str) -> String {
    match lang {
        Lang::En => format!("cause {cause}"),
        Lang::Ko => format!("원인 {cause}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn every_message_has_a_non_empty_english_and_korean_text() {
        for msg in Msg::ALL {
            assert!(!text(Lang::En, *msg).trim().is_empty(), "{msg:?} en");
            assert!(!text(Lang::Ko, *msg).trim().is_empty(), "{msg:?} ko");
        }
        // 영어 문구에는 한글이 없고 한국어만 한글을 가짐
        for msg in Msg::ALL {
            let english = text(Lang::En, *msg);
            assert!(
                !english
                    .chars()
                    .any(|letter| ('\u{AC00}'..='\u{D7A3}').contains(&letter)),
                "{msg:?}: {english}"
            );
        }
    }

    #[test]
    fn the_default_language_is_english_and_codes_round_trip() {
        assert_eq!(Lang::default(), Lang::En);
        for lang in [Lang::En, Lang::Ko] {
            assert_eq!(Lang::from_code(lang.code()), Some(lang));
        }
        assert_eq!(Lang::from_code("fr"), None);
        assert_eq!(Lang::from_code("EN"), None);
        assert_eq!(Lang::from_code(""), None);
    }

    #[test]
    fn formatted_messages_are_non_empty_and_differ_between_languages() {
        let render = |lang: Lang| -> Vec<String> {
            vec![
                too_small(lang, 60, 16, 50, 10),
                stale(lang, 9),
                card_moving(lang, "T"),
                sort_title(lang, "T", "L", "↑"),
                no_records_in_window(lang, "1h"),
                live_events_title(lang, 3),
                feed_hint(lang, 1, 2, 3),
                trend_title(lang, "T", "9"),
                sessions_detail(lang, 3),
                cache_write_detail(lang, "1K", false),
                cache_write_detail(lang, "1K", true),
                cache_read_detail(lang, "1K"),
                error_rate_detail(lang, 1.5),
                rewrite_share_detail(lang, "9%", false),
                rewrite_share_detail(lang, "9%", true),
                tool_table_title(lang, 4),
                tool_calls_value(lang, "1", "2", 3.0),
                hook_table_title(lang, "1", "2", "3"),
                count_with_share(lang, "4", 5.0),
                model_table_title(lang, 1.0),
                agent_table_title(lang, 2, 1.0),
                cache_breaks_title(lang, 2),
                cache_read_write_value(lang, "1", "2", 3.0),
                cost_value(lang, 1.0),
                settings_count(lang, 2),
                doctor_summary(lang, 1, 2),
                allowlist_heading(lang, 2),
                skills_summary(lang, 2, "3"),
                env_keys_heading(lang, 2),
                daily_report_title(lang, "2026-11-15"),
                event_error(lang, ""),
                event_error(lang, "timeout"),
                context_summary_api(lang, 1, 2, 3, "4ms"),
                context_summary_tool(lang, 1, 2, "4ms"),
                context_summary_hook(lang, 1, 2, 3),
                report_requested(lang, "2026-11-15"),
                card_resized(lang, SplitAxis::Columns, 3, 4),
                card_resized(lang, SplitAxis::Rows, 3, 4),
                column_width(lang, 9),
                state_unreadable(lang, "tui.toml", &StateIssue::UnsupportedVersion(2)),
                state_unreadable(lang, "tui.toml", &StateIssue::Detail("x".to_string())),
                state_entries_skipped(lang, "tui.toml", 2),
                state_save_failed(lang, "disk"),
                failure_text(lang, &Failure::ServeOff),
                failure_text(lang, &Failure::DbOpenFailed),
                failure_text(lang, &Failure::DbMissing),
                failure_text(lang, &Failure::Internal("boom".to_string())),
                project_table_title(lang, 2, 1.0),
                attribution_title(lang, text(lang, Msg::DimensionSkill), 3),
            ]
        };
        let english = render(Lang::En);
        let korean = render(Lang::Ko);
        assert_eq!(english.len(), korean.len());
        for (en, ko) in english.iter().zip(&korean) {
            assert!(!en.trim().is_empty() && !ko.trim().is_empty());
        }
        // 값을 빼면 문구가 같은 항목은 없음 (영어는 한글 없음)
        for (index, (en, ko)) in english.iter().zip(&korean).enumerate() {
            let has_hangul = |text: &str| {
                text.chars()
                    .any(|letter| ('\u{AC00}'..='\u{D7A3}').contains(&letter))
            };
            assert!(!has_hangul(en), "{index}: {en}");
            assert!(has_hangul(ko), "{index}: {ko}");
        }
    }

    #[test]
    fn admin_toasts_and_outcome_labels_cover_both_languages() {
        let report =
            AdminAction::GenerateReport(chrono::NaiveDate::from_ymd_opt(2026, 11, 15).unwrap());
        let (text_en, level) = admin_toast(
            Lang::En,
            AdminAction::Reindex,
            &AdminOutcome::Done("ok".to_string()),
        );
        assert_eq!(
            (text_en.as_str(), level),
            ("Reindex done ok", ToastLevel::Success)
        );
        let (text_ko, _) = admin_toast(
            Lang::Ko,
            report,
            &AdminOutcome::Failed(Failure::Error("거부".to_string())),
        );
        assert_eq!(text_ko, "리포트 생성 실패: 거부");
        let (needs, level) = admin_toast(Lang::En, AdminAction::Reindex, &AdminOutcome::NoToken);
        assert_eq!(
            (needs.as_str(), level),
            ("Available after running serve", ToastLevel::Error)
        );
        let (waiting, level) = admin_toast(Lang::Ko, AdminAction::Reindex, &AdminOutcome::Waiting);
        assert_eq!((waiting.as_str(), level), ("재색인 대기", ToastLevel::Info));

        assert_eq!(outcome_label(Lang::En, "deny"), "Deny");
        assert_eq!(outcome_label(Lang::Ko, "deny"), "거부");
        assert_eq!(outcome_label(Lang::Ko, "allow"), "허용");
        assert_eq!(outcome_label(Lang::En, "rewrite"), "Rewrite");
        assert_eq!(outcome_label(Lang::Ko, "error"), "오류");
        assert_eq!(outcome_label(Lang::En, ""), "-");
        assert_eq!(outcome_label(Lang::Ko, "unknown"), "-");
    }

    #[test]
    fn help_rows_keep_key_names_and_have_a_meaning_for_every_key() {
        for lang in [Lang::En, Lang::Ko] {
            let rows = help_rows(lang);
            assert_eq!(
                rows.iter()
                    .filter(|(_, meaning)| meaning.is_empty())
                    .count(),
                2
            );
            assert!(rows.iter().any(|(keys, _)| *keys == "L"), "{lang:?}");
            assert!(rows.iter().any(|(keys, _)| *keys == "l"), "{lang:?}");
            assert!(
                rows.iter().any(|(keys, _)| *keys == "q, Esc, Ctrl+C"),
                "{lang:?}"
            );
            assert!(
                rows.iter().any(|(keys, _)| *keys == "1-9, Tab, Shift-Tab"),
                "{lang:?}"
            );
        }
    }

    #[test]
    fn detail_labels_leave_a_gap_inside_their_label_columns() {
        // 이벤트 상세 항목 이름 칸 10, 모델·에이전트 상세 23, 도구·훅 상세 16
        let event_labels = [
            Msg::DetailTime,
            Msg::DetailModel,
            Msg::DetailAgent,
            Msg::DetailInputTokens,
            Msg::DetailOutputTokens,
            Msg::DetailCacheRead,
            Msg::DetailCacheWrite,
            Msg::DetailFirstByte,
            Msg::DetailFirstToken,
            Msg::DetailSession,
            Msg::DetailDuration,
            Msg::DetailToolName,
            Msg::DetailServer,
            Msg::DetailResult,
            Msg::DetailCallId,
            Msg::DetailRoute,
            Msg::DetailGuard,
            Msg::DetailVerdict,
            Msg::DetailDetail,
            Msg::DetailSaved,
            Msg::DetailLevel,
            Msg::DetailContent,
            Msg::DetailCacheHit,
        ];
        let wide_labels = [
            Msg::ModelCounts,
            Msg::InOut,
            Msg::CacheReadWrite,
            Msg::WaitP50P90,
            Msg::CostEstimate,
            Msg::LastRequest,
            Msg::AgentCounts,
            Msg::AgentToolCalls,
            Msg::AgentTopLast,
            Msg::DetailPerRequest,
            Msg::DetailWindowShare,
            Msg::DetailCacheSplit,
            Msg::DetailFirstSeen,
            Msg::DetailFirstLast,
            Msg::DetailPerRun,
            Msg::DetailPerSession,
            Msg::DetailConfigured,
            Msg::SkillCalls,
            Msg::SkillShare,
            Msg::SkillFirstLast,
            Msg::SkillInstalled,
            Msg::SkillDescription,
            Msg::SkillCostOtel,
        ];
        let mid_labels = [
            Msg::ToolCallsErrors,
            Msg::LatencyP50P90,
            Msg::ToolTotalTime,
            Msg::ToolAvgPerCall,
            Msg::ToolShare,
            Msg::HookRouteGuard,
            Msg::HookTimeSaved,
        ];
        for lang in [Lang::En, Lang::Ko] {
            for msg in event_labels {
                assert!(
                    UnicodeWidthStr::width(text(lang, msg)) < 10,
                    "{lang:?} {msg:?}"
                );
            }
            for msg in wide_labels {
                assert!(
                    UnicodeWidthStr::width(text(lang, msg)) < 23,
                    "{lang:?} {msg:?}"
                );
            }
            for msg in mid_labels {
                assert!(
                    UnicodeWidthStr::width(text(lang, msg)) < 16,
                    "{lang:?} {msg:?}"
                );
            }
        }
    }
}
