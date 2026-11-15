# rust-agents-proxy

## Overview

1. **What it is**
    - A single always-on Windows executable that acts as a localhost hub for Claude Code.
    - `serve` runs several `127.0.0.1` ports in one process, plus a background transcript indexer and a daily report scheduler.
    - Running the exe with no command starts `serve` as a detached background process when it is not already running, then opens the ratatui live dashboard; closing the dashboard leaves `serve` running.
2. **What it does**
    - Collects usage metadata from three sources: session transcripts, OTLP http/json logs, and the API gateway.
    - Stores it in one SQLite file and serves aggregates over a local admin API, the dashboard, and a daily Markdown report.
    - Rewrites legacy CLI calls in Bash hooks (`cat`, `ls`, `git diff`) before the guard denies them, then lets the existing guard re-check the final command.
3. **Highlights**
    - One binary with a bundled SQLite (WAL); no Node, Python, or database server.
    - Four localhost ports: admin `47330`, OTLP `47331`, gateway `47332` (on by default), hooks `47333`.
    - Metadata only: names, counts, durations, token counts, and guard labels; no prompt text, command text, file contents, or auth headers.
    - Fail-closed hook chain: when the guard cannot answer, the call is denied.
    - Hot paths (hook chain, intake, indexing, dashboard) spawn no child process; the only spawn is the one-time detached `serve` launch when the exe runs with no command.
4. **Documents**
    - Korean version: [README-KO.md](README-KO.md)
    - Source tree, runtime layout, data flow, and security boundaries: [ARCHITECTURE.md](ARCHITECTURE.md)
    - License: [LICENSE.md](LICENSE.md)

---

## Warning

1. **Fail-closed registration**
    - Once `pretool-chain` is registered with `onFailure: "block"`, every Bash call is blocked while `serve` is down.
    - A guard that does not answer within `hooks.guard_deadline_ms` also makes the chain deny the call.
2. **Before registering**
    - Start `serve`, then run `rust-agents-proxy doctor` and fix every `fail` and `warn` line first.
    - `doctor` exits with code 1 only when a finding is `fail`.
3. **Keep serve running**
    - Run `serve` permanently with `.cmd/install-task.ps1`: Task Scheduler at logon, `conhost --headless`, restart on failure every minute.
    - A crash still leaves Bash blocked until the task restarts `serve`.
4. **Unverified**
    - `DISABLE_TELEMETRY=1` does not block the OTLP export (measured 2026-11-16: `otlp_batches` rose with it set).

---

## Quick Start

### 1. Build

1. **Build and sign**
    - The repository carries no release workflow, so build from source with the toolchain described in Build From Source.
    - `.cmd/build.cmd` builds the release exe, replaces a running one without downtime, and Authenticode-signs it.

```bat
.cmd\build.cmd
```

2. **Output**
    - The exe lands at `target\release\rust-agents-proxy.exe`, or under `CARGO_TARGET_DIR` when that is set.
    - Keep `libunwind.dll` next to it; `build.rs` copies it there for the gnullvm target.

### 2. Run

1. **Just run it**
    - Double-click the exe, or run it with no command.
    - When the admin port does not answer, it launches `rust-agents-proxy serve` as a detached process (no console window, breaking away from the parent job when Windows allows it) and waits up to 8 seconds until its `/health` answers `200 ok`, which happens only after the store, token, and runtime are ready.
    - It then opens the dashboard; closing the dashboard or its window leaves `serve` running.
    - A second launch reuses the running `serve` and only opens the dashboard.
    - The detached server writes its output to `serve.log` in the data directory, recreated on each launch.

```bat
rust-agents-proxy
```

2. **Server only**
    - `serve` runs every enabled port and background job in the foreground, for Task Scheduler or debugging.
    - It writes a new admin token to the data directory on every start.
    - Start order: the gateway upstream is validated first, then the ports are bound in the order admin, OTLP, hooks, gateway; `admin.token` is written only after the required ports are bound.
    - Required ports are admin and hooks (when `hooks.enabled`); when one is taken, `serve` exits with an error.
    - OTLP and gateway are optional: when one cannot bind, `serve` prints a warning and runs without it.
    - With `hooks.enabled = false` the hooks port is not taken.

```bat
rust-agents-proxy serve
```

3. **Dashboard only**
    - `tui` opens the dashboard without starting a server; stored data stays browsable, while status, reindex, and report generation need a running `serve`.

```bat
rust-agents-proxy tui
```

### 3. Register With Claude Code

1. **Diagnose**
    - `doctor` prints each finding as `[level] topic: message`, with a `->` fix line when there is one.
    - The snippets below are the fixes `doctor` prints for the default ports.

```bat
rust-agents-proxy doctor
```

2. **Replace the PreToolUse guard entry**
    - In `settings.json` under the Claude home (`%USERPROFILE%\.claude`, or `CLAUDE_CONFIG_DIR`), replace the `^Bash$` entry of `hooks.PreToolUse` that points at `/hook/pretool-guard` with this entry.
    - Keep `onFailure` at `block` and `timeout` at `10`.
    - Do not leave the old guard entry beside it: hooks run in parallel, so a guard denial beats the rewrite, and `doctor` warns about the pair.

```json
{"matcher":"^Bash$","hooks":[{"type":"http","url":"http://127.0.0.1:47333/hook/pretool-chain","timeout":10,"onFailure":"block"}]}
```

3. **Allow the hook URL**
    - When `settings.json` has an `allowedHttpHookUrls` list, add this entry; without it the hook does not run, and `doctor` reports `fail`. An empty list blocks every HTTP hook (`fail`); no list at all means no restriction (`ok`).

```json
"allowedHttpHookUrls": ["http://127.0.0.1:47333/hook/*"]
```

4. **Telemetry environment**
    - Add these keys to the `env` block so Claude Code sends OTLP logs to the receiver.

```json
"env": {
  "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
  "OTEL_LOGS_EXPORTER": "otlp",
  "OTEL_EXPORTER_OTLP_ENDPOINT": "http://127.0.0.1:47331",
  "OTEL_EXPORTER_OTLP_PROTOCOL": "http/json"
}
```

5. **Gateway routing**
    - The gateway is on by default, so `serve` already takes port `47332` and relays to `gateway.upstream`; no `rap.toml` change is needed.
    - The author's setup routes Claude Code through it: add these keys to the `env` block; through a custom base URL, background tasks use the main model, so `ANTHROPIC_DEFAULT_HAIKU_MODEL` pins the small model.
    - With `ANTHROPIC_BASE_URL` pointing at a stopped gateway, or at one turned off with `[gateway] enabled = false`, API calls fail to connect, and `doctor` reports `fail` for the turned-off case.

```json
"env": {
  "ANTHROPIC_BASE_URL": "http://127.0.0.1:47332",
  "ANTHROPIC_DEFAULT_HAIKU_MODEL": "<haiku model id>"
}
```

### 4. Keep It Running

1. **Task Scheduler**
    - `.cmd/install-task.ps1` registers the task `rust-agents-proxy`, which runs `conhost.exe --headless "<exe>" serve` at logon as the current user.
    - Restart on failure: up to 999 times, 1 minute apart; no execution time limit; a second instance is ignored.
    - `serve` binds admin before it writes `admin.token`; when admin or hooks is taken (for example by another `serve`) it exits with an error and leaves the running instance's token alone, so a concurrent start never overwrites it.
    - Supervision: when the admin or hooks server ends or panics, the whole process exits with a non-zero code, so the scheduler restarts it instead of leaving a half-working `serve`.
    - When OTLP, the indexer, the report scheduler, or the gateway ends, `serve` prints a warning, sends a warn notice to the dashboard, and keeps running.
    - At start, `serve` waits for the first index run and then regenerates yesterday's report, so a report written before indexing caught up gets replaced.
    - The task starts at the next logon; start it now with `Start-ScheduledTask -TaskName rust-agents-proxy`.
2. **Actions**
    - `-Action Install`, `-Action Uninstall`, or `-Action Status`, with `-ExePath <exe>` and `-Yes` for a non-interactive run.
    - Without `-Action` it prompts in a terminal and defaults to `Status` otherwise.

```powershell
.cmd\install-task.ps1 -Action Install -ExePath target\release\rust-agents-proxy.exe -Yes
```

### 5. Check

1. **Status**
    - `status` prints the admin `/api/status` JSON: version, pid, uptime, data directory, port state, and live counters.
2. **Telemetry received**
    - After a Claude Code session, `otlp_batches` in `status` should increase.
3. **Doctor again**
    - Rerun `doctor` and aim for no `fail` or `warn` lines.

```bat
rust-agents-proxy status
```

---

## Commands

1. **serve**
    - Runs the resident server: admin, OTLP, hooks, gateway, transcript indexer, and report scheduler.
    - `start` makes sure a detached `serve` is running and exits without opening the dashboard; the logon shortcut in the boot folder uses it.
2. **tui**
    - Opens the live dashboard only. With no command at all, the exe first makes sure a detached `serve` is running and then opens the dashboard.
3. **index**
    - Indexes the session transcripts once and prints a JSON summary: files scanned and changed, bytes read, elapsed milliseconds, and row counts.
4. **report**
    - Writes the daily report and prints its path; `--date YYYY-MM-DD` picks the day, and the default is yesterday.
5. **hook**
    - `hook <route>` is a command-hook client: it sends stdin to `http://127.0.0.1:<hooks port>/hook/<route>` and prints the reply.
    - The route is one segment of `a-z`, `0-9`, and `-`, or `relay/<name>`.
    - It exits 0 on a 2xx reply and 1 otherwise; the stdin limit is 16 MiB.
    - A route that starts with `pretool` fails closed: on an oversized stdin, a send failure, or a non-2xx reply it prints the deny JSON and exits 0; `relay/*` routes exit 1.
6. **status**
    - Prints the admin status JSON.
7. **doctor**
    - Checks the data directory, database, ports, relay target, hook registration, allowed URL, telemetry, gateway, and projects directory.
    - It reports `fail` when `pretool-chain` is registered but the hooks port does not answer `/health` (or `hooks.enabled` is false), because every Bash call would be blocked.
8. **Global option**
    - `--config <path>` selects the `rap.toml`; a configuration error exits with code 2, and other errors exit with 1.

---

## Dashboard

1. **Tabs**
    - Seven tabs with keys `1`-`7`: overview, tools, hooks, models, agents, settings (`6`), and report (`7`).
    - There is no gateway tab; gateway timing (first byte, first token, and total time, p50/p90) is still available through `GET /api/gateway` and in the daily report, and the overview server-status card still shows the gateway port as a status dot.
    - The app bar holds only the `rust-agents-proxy` badge on the left, with no caption, and the window and freshness on the right.
    - The dashboard reads `rap.sqlite` read-only on a background thread every 2 seconds, so the UI thread does no I/O.
    - The heavy panels (tools, hooks, model stats, agent stats, cache breaks) are queried every 10 seconds; the first read, a window change, and `r` refresh them at once.
    - The screen is redrawn only when something changed (new data, input, a notice, or a hover change), and at least once per second.
    - When the tab labels do not fit the width, the tab bar first removes the gaps between tabs and then switches to numbered compact labels, so all seven tabs stay visible and clickable down to 60 columns.
    - Labels and the daily report text are Korean in this version.
2. **Keys**
    - `1`-`7`, `Tab`, `Shift-Tab`: switch tab
    - `Up`, `Down`, `PgUp`, `PgDn`, `Home`, `End`: scroll
    - `w`: cycle the window through 1 hour, 24 hours, and 7 days
    - `s`, `S`: next sort column (ascending) and the sort cycle of the current column (ascending, descending, card default) of the focused table card (the main table of the tab when the focused card has no sort)
    - `Left`, `Right`: report date, report tab only
    - `g`: generate the report, report tab only
    - `i`: reindex
    - `g` and `i` go through the admin API and need a running `serve`
    - `t`: toggle always-on-top, `l`: reset the card layout and the dragged column widths of the current tab
    - `r`: refresh, `?`: help, `q`, `Esc`, `Ctrl+C`: quit
3. **Mouse**
    - The layout and interaction follow the sibling `database-monitor`: every tab is a set of bordered cards.
    - Click a tab to switch, a card to focus it, a table row to select it (details appear in the side card), and a table header to sort by that column (a different header starts ascending, and clicking the same header cycles ascending, descending, and back to the table's default sort; the sorted header shows `↑` or `↓`, and the default column shows its default arrow again after the cycle).
    - The wheel scrolls the card under the pointer; hovering a card or a card boundary highlights it.
    - A scrollable card shows a scrollbar in a gutter column just inside its right border when its content overflows, so the thumb never shares a color with the border: the `░` track is quiet, the gray `█` thumb brightens on hover and turns white while dragged. Drag the thumb to scroll or click the track (gutter or border column) to move one screen; the `a-b / n` position stays at the bottom.
    - The selected row is a full-width band from the left padding to the gutter: bright blue with white text in the focused card, a soft gray in the others.
    - Drag a boundary between cards to resize them; neighbors keep a minimum size, and ratios hold on window resize. Layouts live for the session only. A boundary shows a gray grip only while the pointer is on it or it is dragged; nothing is drawn while idle.
    - Drag the boundary between two header cells of a table card to resize the left column (minimum 3 columns; numeric columns never go below their content width): the slack column absorbs the change, widths are kept per card for the session, and a double click on a boundary or `l` restores automatic widths. A boundary grabbed by the scrollbar gutter or a card split handle goes to those first, and a drag on a column boundary never sorts. A boundary shows the same gray grip only while hovered or dragged.
    - The bottom bar holds clickable buttons for reindex, window, refresh, top-most, and layout reset on the left and help and quit on the right (the left group shrinks first on a narrow window); the report tab adds previous/next date and generate buttons.
    - The hint line below the buttons follows the context: the pointer on a card boundary, column boundary, or scrollbar, the focused card's kind, and the tab (sort keys on every sortable table, date keys on report).
    - The event feed is a column grid with a pinned header (time, kind, name, agent, input, output, cache read, duration); the shown columns shrink with the card width, and long agent ids are shortened to `name#xxxx`.
    - Every table card sorts by header click or `s`/`S` with a per-card sort state and a sensible default sort that the third click or `S` returns to (feed: time descending, tools: total time descending, hooks: count descending, models and agents: requests descending); numbers and durations sort numerically, text by its displayed string, and the selected row stays on the same item after a re-sort.
    - The models tab shows per-model stats (requests, sessions, agent types, tokens, cache hit ratio, wait p50/p90, estimated cost, last seen) with a detail card and the cache-break candidates; the agents tab shows per-agent-type stats (runs, requests, sessions, tokens, estimated cost, wait p50/p90, tool calls and errors, top model, last seen) with a detail card.
    - Text columns get their natural width (the widest cell or header, Hangul counted as 2) first, so nothing is ellipsized while the card has free width, and the last growing text column (for example hook detail) takes the slack; in narrow cards the widest text columns shrink and ellipsize first, numeric columns keep their full width, and the right-most columns drop before any number is clipped. The event feed grows its name and agent columns the same way.
    - The report view hides the `text` fence lines, shows box tables verbatim in a muted line color with an emphasized header row (clipped, never wrapped), and converts legacy pipe-table reports to box tables when displaying them.
4. **Console**
    - In a classic console window the dashboard applies the `D2Coding ligature` font while it runs and restores the original font on exit, as `database-monitor` does.

---

## Ports And Endpoints

### 1. Admin 47330

1. **Read endpoints**
    - `GET /health` returns `ok`; `GET /api/status` returns the status JSON.
    - `GET /api/overview`, `tools`, `hooks`, `models`, `gateway`, `cache-breaks`, and `minutes` return aggregates; `?hours=` sets the window (default 24, clamped to 1-720).
    - `GET /api/model-stats` returns one row per model (transcript source only): requests, sessions, agent types, token sums, cache hit ratio, wait p50/p90, estimated cost, and last timestamp; `?hours=` works as above.
    - `GET /api/agent-stats` returns one row per agent type (the label before `#`, `main` for the main thread): runs (distinct agent instances; sessions for `main`), requests, sessions, token sums, cache hit ratio, wait p50/p90, estimated cost, tool calls and errors, top model, and last timestamp; `?hours=` works as above.
    - `GET /api/report?date=YYYY-MM-DD` returns the stored report as Markdown, `400` without a valid date, and `404` when none is stored.
    - `GET /api/events` is a Server-Sent Events stream of live events, with a `: ping` comment every 15 seconds.
2. **Token-protected endpoints**
    - `POST /api/reindex` runs the transcript indexer once.
    - `POST /api/report?date=YYYY-MM-DD` generates and stores a report; the default date is yesterday.
    - Every `POST` needs the token in the header `x-rap-token`; a missing or wrong token returns `401`.
3. **Token**
    - The token is 32 random bytes from the OS, written as hex to `%LOCALAPPDATA%\rust-agents-proxy\admin.token`.
    - It is regenerated on every `serve` start.

### 2. OTLP 47331

1. **Routes**
    - `POST /v1/logs`, `/v1/metrics`, and `/v1/traces`.
2. **Accepted input**
    - JSON only: another content type gets `415` with the hint `use OTEL_EXPORTER_OTLP_PROTOCOL=http/json`.
    - `gzip` content encoding is accepted; the body limit is 16 MiB, also after inflating.
3. **Handling**
    - Logs map three events (`api_request`, `tool_result`, `hook_execution_complete`) to rows; other events are ignored.
    - Metrics and traces are only counted.

### 3. Gateway 47332

1. **Role**
    - On by default (`gateway.enabled = true`); a transparent proxy to the Anthropic API (`gateway.upstream`, HTTPS only).
    - It measures first byte, first token, total time, status, and token usage for `POST /v1/messages`; every other path is relayed untracked.
2. **Behavior**
    - Request headers, including credentials, go to the upstream unchanged except hop-by-hop headers and `host`; none are stored.
    - The response streams through without buffering; an upstream connection error becomes a `502` JSON error.
    - When the response headers do not arrive within `gateway.upstream_timeout_secs`, it returns a `504` JSON error; a streamed body has no deadline.
    - It runs on its own runtime thread so a long stream does not compete with hooks and indexing.

### 4. Hooks 47333

1. **Routes**
    - `POST /hook/pretool-chain`, `POST /hook/relay/{route}`, and `GET /health`.
2. **Relay route**
    - `{route}` is `a-z`, `0-9`, and `-` only.
    - The request is forwarded unchanged to `{hooks.relay_target}/hook/{route}`, and the upstream status and body come back as they are.
    - The verdict, guard label, and duration are recorded; an unreachable upstream returns `502`.
3. **Body limit**
    - 16 MiB per request.

### 5. Common Rules

1. **Binding**
    - Every port listens on `127.0.0.1` only.
2. **Request checks**
    - The `Host` header must be `127.0.0.1:<port>` or `localhost:<port>`; anything else gets `403`.
    - A request with an `Origin` or `Sec-Fetch-Site` header gets `403`, which blocks browser-originated requests.

---

## Hook Chain

### 1. Flow

1. **Steps**
    - `pretool-chain` rewrites a legacy CLI command, then forwards the payload to the existing guard at `{hooks.relay_target}/hook/pretool-guard` (default `http://127.0.0.1:47321`).
    - The guard is not part of this project; it must already answer there.
2. **Outcomes**
    - Guard unreachable, late, non-2xx, or not a JSON object: deny with the reason `[rap] pretool-guard 응답 없음 (fail-closed)`.
    - Guard denies: the guard's answer passes through unchanged.
    - Guard allows and nothing was rewritten: the guard's answer passes through unchanged.
    - Guard explicitly allows and a rewrite happened: `permissionDecision: allow` with `updatedInput` carrying the new command and an `additionalContext` note naming each conversion.
    - Guard gives no decision (`{}`) and a rewrite happened: `updatedInput` and the note only, with no `permissionDecision`, so Claude Code's normal permission flow applies to the rewritten command; rap never grants an allow the guard did not give.
    - Guard asks and a rewrite happened: `ask` with `updatedInput`, so the user approves exactly the command the guard evaluated.
    - Guard supplies its own `updatedInput`: the guard's answer wins and the rewrite is dropped.
    - Guard gives a decision other than allow, ask, or deny (for example `defer`) after a rewrite: the rewrite is dropped, the guard is called again with the original command, and its second answer is returned as it is (a deny stays a deny; a second unreachable, late, non-2xx, or non-object answer fails closed).
3. **Recorded fields**
    - The outcome (`allow`, `deny`, `rewrite`, or `error`), the guard label, converted command names, duration, and an estimated saving of 4200 ms per rewrite.

### 2. Rewrite Rules

1. **cat**
    - `cat <files>` becomes `bat --plain --paging=never <files>`.
    - It is left alone when there are no arguments, an argument starts with `-`, or a redirection is present.
    - A command that contains `>|` (the noclobber-override redirection) is never rewritten.
2. **ls**
    - `ls` becomes `eza` when every flag is one of `-l -a -la -al -lh -lah -alh -1 -A -h`; `h` is dropped, `A` becomes `a`, and `.` is added when no path is given.
3. **eza**
    - A pathless `eza` with only flags gets ` .` appended.
4. **git diff**
    - `git diff` gets ` | delta` appended.
    - It is left alone with summary flags (`--stat`, `--shortstat`, `--numstat`, `--name-only`, `--name-status`, `--quiet`, `--exit-code`, `--check`, `--summary`, `--no-patch`, `-s`), `--no-index`, a redirection, an existing pipe, or a directly following `&&` or `||` (a pipe would replace the exit code).
5. **Everything else**
    - Commands are split on `&&`, `||`, `;`, and `|`, and each part is converted on its own.
    - One `cat` or `ls` part that cannot be converted leaves the whole command untouched.
    - Newlines, heredocs, backticks, `$(`, `$'`, `${`, `[[`, `case`, process substitution, unbalanced quotes, comments, `&`, parentheses, unquoted braces, non-ASCII whitespace, unquoted non-ASCII text, and a trailing backslash leave it untouched too.
    - Flags are judged on the unquoted value, so `ls "-s" src` counts as the unsupported flag `-s`.
    - `grep`, `find`, and other commands are never rewritten, and the guard always re-checks the final command.

---

## Data And Privacy

### 1. Data Directory

1. **Location**
    - `%LOCALAPPDATA%\rust-agents-proxy`; `RAP_DATA_DIR` overrides it.
2. **Contents**
    - `rap.sqlite`: the database, in WAL mode.
    - `rap.toml`: optional configuration; the example is `rap.example.toml`.
    - `admin.token`: the admin token of the current `serve` run.
    - `serve.log`: the output of the detached `serve` that a no-command launch starts, recreated on each launch.
    - `reports\YYYY-MM-DD.md`: daily reports; each is also stored in the database.

### 2. Stored Data

1. **Metadata only**
    - Model and agent names, tool names, session ids, project folder names, timestamps, durations, and token counts.
    - Hook verdicts: route, guard label, outcome, and the names of detected legacy commands.
2. **Never stored**
    - Prompt text, command text, file contents, tool inputs and outputs, and auth headers.
    - While parsing, the transcript reader holds up to 400 characters of a tool result in memory; it uses them only to read a guard label and a command name from an error result and writes only those two.
3. **Config inspection**
    - `doctor` and the settings tab read `settings.json` and `.claude.json` on demand; env values are compared, never printed or stored.

### 3. Duplicate Handling

1. **One fact, one owner**
    - The same request can arrive through the transcript, OTLP, and the gateway, so the aggregates count each source for its own purpose.
    - [ARCHITECTURE.md](ARCHITECTURE.md) lists the keys and rules.

---

## Configuration

### 1. Resolution

1. **Order**
    - `--config <path>`, then `RAP_CONFIG`, then `rap.toml` in the data directory, then the defaults.
2. **Strictness**
    - Unknown keys are an error; ports must be non-zero and distinct, and `report.run_hour` must be 0-23.
    - `hooks.relay_target` accepts only an `http` loopback address (`127.0.0.1`, `localhost`, or `[::1]`) on a port that is not one of rap's own ports; anything else is a configuration error, so the start fails.
    - `hooks.guard_deadline_ms` must be between 1 and 4500, so even the two guard calls of the `defer` re-check fit inside the 10-second hook timeout.
3. **Claude home**
    - `claude_home` in `rap.toml`, then `CLAUDE_CONFIG_DIR`, then `%USERPROFILE%\.claude`.

### 2. rap.toml

1. **Keys**
    - Every key is optional; this block shows the defaults, and `rap.example.toml` is the same file.
    - `gateway.enabled` defaults to `true`; `false` leaves port `47332` untouched.
    - `gateway.upstream_timeout_secs` limits the wait for upstream response headers (504 on timeout); a streamed body has no deadline after the headers arrive. It must be at least 1.

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

2. **Report schedule**
    - On start, `serve` waits for the first index run (up to 10 minutes; a warn notice is sent on timeout) and then regenerates yesterday's report, replacing any stored one.
    - Then it writes the previous day's report daily at `run_hour:05` local time.
    - A report covers one local calendar day; its sections are summary, comparison with the previous day, model and agent usage with an estimated cost, top tool latencies, hook verdicts, top cache breaks, gateway timing when present, and rule-based suggestions.
    - Every table in the report file is a light box table (`┌─┬─┐`) inside a `text` fence under its section heading, with Hangul and CJK counted as two columns, headers centered, counts and durations right-aligned, and a rule between every row; there are no pipe tables.

---

## Measurements

1. **Scope**
    - These are measurements from the author's PC on 2026-11-15, not guarantees; other machines and workloads will differ.
2. **Transcript backfill**
    - A 7-day backfill read 453 files and 602 MB in 1.7 s on the release build.
3. **Hook chain overhead**
    - `pretool-chain` added about 1.6 ms per Bash call over the guard alone: 3.6 ms against 2.0 ms on a reused connection.
4. **Denials**
    - The 7-day history showed 266 cli-guard denials, each costing a median 4.2 s extra model round trip.
    - The real-data report for 2026-11-14 listed `cat` 47, `ls` 39, and `grep` 11 denials.
    - The rewrite rules cover `cat` and `ls` but not `grep`, so `grep` denials remain.
5. **Network**
    - The round trip to the API is about 190 ms, which a local proxy cannot reduce.
6. **Where the gain comes from**
    - The speed gain comes from avoided denials and avoided hook process spawns, not from network tricks.
    - The dashboard's saved time is an estimate: rewrites multiplied by 4200 ms.
    - The gateway exists to measure first byte and first token; its own added latency was not measured.

---

## Build From Source

### 1. Requirements

1. **Toolchain**
    - Rust 1.85 or newer (edition 2024), host `stable-x86_64-pc-windows-gnullvm`.
    - `.cargo/config.toml` pins the clang, `ar`, `windres`, and linker paths under `C:/JUNGHO/1.Language/8.Clang`; change them on another machine.
2. **Vendored crates**
    - Builds read the project `vendor/` directory, because the shared global vendor lacks these crates.
    - `vendor/` is gitignored; regenerate it with the command below.

```bat
cargo vendor --config net.offline=false vendor
```

### 2. Release Build

1. **build.cmd**
    - It moves a running exe aside as `rust-agents-proxy.exe.stale-<n>`, so a running `serve` keeps its image and cargo links a fresh binary at the original path.
    - It deletes older stale images, runs `cargo build --release --offline`, then signs the exe.
    - Restart `serve` afterwards to run the new build.
2. **Signing**
    - `.cmd/sign.ps1` signs with the certificate whose thumbprint is in `RUST_AGENTS_PROXY_SIGN_THUMBPRINT` (store `CurrentUser\My`, private key required).
    - Without it, the script uses or creates a local self-signed code-signing certificate, which is not in a trusted root, so it only proves that a signature exists.
    - `RUST_AGENTS_PROXY_SIGN_TIMESTAMP` sets an optional timestamp server; a signing failure only warns and leaves the exe unsigned.
3. **Resources**
    - `build.rs` embeds the icon, version info, and an `asInvoker` manifest with `windres`, and copies `libunwind.dll` beside the exe.
    - The icon comes from `assets/icon`; regenerate `rust-agents-proxy.ico` from the two SVG files with `assets/icon/build_icon.py`, which needs ImageMagick.

---

## Limitations

1. **Gateway**
    - It does not honor `HTTPS_PROXY`.
    - A response with a `Content-Encoding` header is relayed and timed, but its token usage is not read.
2. **Windows networking**
    - On this Windows host, HTTP/1.1 requests with `Connection: close` sometimes lose the response (RST), so the built-in blocking clients avoid that header; the cause is unconfirmed.
3. **OTLP**
    - The mapping is tested against fixture files only; it is untested against a live Claude Code export.
4. **Guard dependency**
    - The chain needs an existing guard server at `hooks.relay_target`; without one every Bash call is denied.
5. **Partial start**
    - When the OTLP or gateway port cannot bind, `serve` runs without that server and only prints a warning, so check `doctor` and the start-up log.
    - Once running, the end of OTLP, the indexer, the report scheduler, or the gateway is only announced; the end of admin or hooks stops `serve`.
6. **Language and platform**
    - The dashboard and report text are Korean only, and the default data directory needs `%LOCALAPPDATA%` or `RAP_DATA_DIR`.
7. **Cost estimate**
    - It uses a built-in per-model rate table; an unknown model counts as zero.
8. **Read endpoints**
    - `GET` endpoints on the admin port need no token; they return metadata only, behind the Host and Origin checks.

---

## Development

### 1. Checks

1. **Commands**
    - Run these from the repository root after every change.

```bat
cargo clippy --offline --all-targets -- -D warnings
cargo test --offline
cargo fmt --check
```

2. **Divider comments**
    - Divider comments must end at column 80.

```bat
bun ~/.claude/skills/languages-comments/scripts/divider-width.ts <file>...
```

### 2. Tests

1. **Layout**
    - Unit tests sit beside their modules under `#[cfg(test)]`, or in `tests.rs` and `test_support.rs` files.
    - `tests/fixtures` holds the fixture Claude homes, transcripts, and OTLP payloads that the tests read.
2. **Coverage**
    - The rewrite table, the chain with a fake guard (allow, deny, deadline, rewrite, recheck), the hook client fail-closed output, and the hook server on temporary ports.
    - The admin token and aggregates, the `reject_non_local` table, the OTLP mapping and its size limits (413), the gateway 502 and 504 paths, the SSE usage parser, and the indexer.
    - The port bind and serve supervision rules, the doctor checks, the report flow with its overwrite on start, and the per-event hook-denial counting.
    - The dashboard rendering, mouse and layout handling, and the scrollbar.

---

## License

1. **Apache-2.0**
    - See [LICENSE.md](LICENSE.md).
