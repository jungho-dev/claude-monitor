# claude-monitor

## Overview

1. **What it is**
    - A single always-on Windows executable that acts as a localhost hub for Claude Code.
    - It collects usage metadata from session transcripts, OTLP logs, and an optional API gateway into one SQLite file, and shows it in a live terminal dashboard, a local admin API, and a daily Markdown report.
    - It can also speed up Bash hooks by rewriting legacy CLI calls before your own PreToolUse guard sees them.
2. **Documents**
    - Korean version: [README-ko.md](README-ko.md)
    - Source tree, runtime layout, data flow, merge rules, and security boundaries: [ARCHITECTURE.md](ARCHITECTURE.md)
    - License (Apache-2.0): [LICENSE.md](LICENSE.md)
3. **Badges**
    - [![Crates.io](https://img.shields.io/crates/v/claude-monitor.svg)](https://crates.io/crates/claude-monitor) [![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE.md)
4. **What it does**
    - Collects usage metadata from three sources: session transcripts, OTLP http/json logs, and the API gateway.
    - Stores it in one SQLite file and serves aggregates over the admin API, the dashboard, and a daily report.
    - Rewrites legacy CLI calls in Bash hooks (`cat`, `ls`, `git diff`) before your guard denies them, then lets the guard re-check the final command.
    - Copes with forced Modern Standby: while Claude Code is active it asks Windows to postpone system sleep (the screen-off and lock policy stays untouched), records each standby gap, and tells the agent after resume that a failure overlapping a gap is environmental.
5. **Highlights**
    - One binary with a bundled SQLite (WAL); no Node, Python, or database server.
    - Four localhost ports: admin `47330`, OTLP `47331`, gateway `47332` (on by default), hooks `47333`.
    - Metadata only: names, counts, durations, token counts, and guard labels; no prompt text, command text, file contents, or auth headers.
    - Fail-closed hook chain: when the guard cannot answer, the call is denied.
    - Hot paths (hook chain, intake, indexing, dashboard) spawn no child process; the only spawn is the one-time detached `serve` launch when the exe runs with no command.

---

## Requirements

1. **Platform**
    - Windows; the standby monitor, the detached launch, and the console controls use Win32 APIs.
2. **Claude Code**
    - The telemetry, gateway, and hook steps below edit `settings.json` under the Claude home (`%USERPROFILE%\.claude`, or `CLAUDE_CONFIG_DIR`).
    - Hook entries use the HTTP hook type (`"type": "http"`).
3. **Rust toolchain**
    - Needed only to build or `cargo install`: Rust 1.89 or newer (edition 2024), with a C compiler for the bundled SQLite and `ring` (Visual Studio Build Tools on the MSVC toolchain).
4. **Guard server**
    - Needed only for the hook chain: an existing PreToolUse guard that answers `POST /hook/pretool-guard` and `GET /health` on a loopback HTTP port (default `http://127.0.0.1:47321`).
    - This project does not ship a guard; without one, skip the hook chain and use the intake and gateway features only.

---

## Warning

1. **Fail-closed registration**
    - Once `pretool-chain` is registered with `onFailure: "block"`, every Bash call is blocked while `serve` is down.
    - A guard that does not answer within `hooks.guard_deadline_ms` also makes the chain deny the call.
2. **Before registering**
    - Start `serve`, then run `claude-monitor doctor` and fix every `fail` line first.
    - `doctor` exits with code 1 only when a finding is `fail`.
3. **Keep serve running**
    - Run `serve` permanently with Task Scheduler (see Keep It Running); a crash still leaves Bash blocked until the task restarts `serve`.

---

## Quick Start

### 1. Install

1. **From crates.io**
    - Installs the release binary into `%USERPROFILE%\.cargo\bin`.

```bat
cargo install claude-monitor
```

2. **From source**
    - Clone the repository, then build with plain cargo; the exe lands at `target\release\claude-monitor.exe`, or under `CARGO_TARGET_DIR` when that is set.

```bat
cargo build --release
```

3. **Toolchain notes**
    - The default MSVC or gnullvm Windows toolchains build with plain cargo; a toolchain-specific linker setup is local to the author and not shipped.
    - `windres` is optional: on a Windows target `build.rs` writes the resource script (the app icon `assets/icon/claude-monitor.ico`, an English `VERSIONINFO` block, and an `asInvoker` manifest) into the build output folder and compiles it with the first working tool of `<arch>-w64-mingw32-windres` (looked up next to the linker first, then on `PATH`), `llvm-windres --target=<arch>-w64-mingw32`, and `windres`.
    - A produced object whose COFF machine type does not match the target (for example an x64 object for `aarch64-pc-windows-msvc`) is dropped with a warning so the linker never sees it; when no tool works, `build.rs` prints a warning and builds without the icon, the version info, and the manifest.
    - On a gnullvm toolchain the exe loads `libunwind.dll`: `build.rs` copies it beside the build output only, so copy it from `<rustc --print sysroot>\bin` next to an exe that you move or `cargo install`.
4. **Check the install**
    - `--version` and `--help` print without starting anything.

```bat
claude-monitor --version
```

### 2. First Run

1. **Just run it**
    - Run the exe with no command in a terminal.
    - When the admin port does not answer, it launches `claude-monitor serve` as a detached process (no console window, breaking away from the parent job when Windows allows it) and waits up to 8 seconds until its `/health` answers `200 ok`, which happens only after the store, token, and runtime are ready.
    - It then opens the dashboard; closing the dashboard leaves `serve` running, and a second launch reuses the running `serve`.
    - The detached server writes its output to `serve.log` in the data directory, recreated on each launch.
    - The first launch creates the data directory `~/.config/claude-monitor/` (on Windows `%USERPROFILE%\.config\claude-monitor\`) and the database; the dashboard stays empty until Claude Code produces data.

```bat
claude-monitor
```

2. **Server only**
    - `serve` runs every enabled port and background job in the foreground, for Task Scheduler or debugging, and writes a new admin token on every start.
    - Start order: the gateway upstream is validated first, then the ports are bound in the order admin, OTLP, hooks, gateway; `admin.token` is written only after the required ports are bound.
    - Required ports are admin and hooks (when `hooks.enabled`); when one is taken, `serve` exits with an error.
    - OTLP and gateway are optional: when one cannot bind, `serve` prints a warning and runs without it.
    - With `hooks.enabled = false` the hooks port is not taken.

```bat
claude-monitor serve
```

3. **Dashboard only**
    - `tui` opens the dashboard without starting a server; stored data stays browsable, while status, reindex, and report generation need a running `serve`.

```bat
claude-monitor tui
```

### 3. Connect Claude Code

1. **Where to edit**
    - Edit `settings.json` under the Claude home; every snippet below is exactly what `doctor` prints as the fix for the default ports.
    - Start a new Claude Code session after editing so it reads the new values.
2. **Telemetry environment**
    - Add these keys to the `env` block so Claude Code sends OTLP logs to the receiver.

```json
"env": {
  "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
  "OTEL_LOGS_EXPORTER": "otlp",
  "OTEL_EXPORTER_OTLP_ENDPOINT": "http://127.0.0.1:47331",
  "OTEL_EXPORTER_OTLP_PROTOCOL": "http/json"
}
```

3. **Gateway routing (optional)**
    - The gateway is on by default, so `serve` already takes port `47332` and relays to `gateway.upstream`; no `config.toml` change is needed.
    - To measure Claude Code's own API traffic, add these keys to the `env` block; through a custom base URL, background tasks use the main model, so `ANTHROPIC_DEFAULT_HAIKU_MODEL` pins the small model.
    - With `ANTHROPIC_BASE_URL` pointing at a stopped gateway, or at one turned off with `[gateway] enabled = false`, API calls fail to connect, and `doctor` reports `fail` for the turned-off case.

```json
"env": {
  "ANTHROPIC_BASE_URL": "http://127.0.0.1:47332",
  "ANTHROPIC_DEFAULT_HAIKU_MODEL": "<haiku model id>"
}
```

4. **Hook chain (optional, needs a guard)**
    - In `hooks.PreToolUse`, replace the `^Bash$` entry that points at `/hook/pretool-guard` with this entry; read the Warning first.
    - Keep `onFailure` at `block` and `timeout` at `10`.
    - Do not leave the old guard entry beside it: hooks run in parallel, so a guard denial beats the rewrite, and `doctor` warns about the pair.

```json
{"matcher":"^Bash$","hooks":[{"type":"http","url":"http://127.0.0.1:47333/hook/pretool-chain","timeout":10,"onFailure":"block"}]}
```

5. **Allow the hook URL**
    - When `settings.json` has an `allowedHttpHookUrls` list, add this entry; without it the hook does not run, and `doctor` reports `fail`.
    - An empty list blocks every HTTP hook (`fail`); no list at all means no restriction (`ok`).

```json
"allowedHttpHookUrls": ["http://127.0.0.1:47333/hook/*"]
```

6. **Standby hint hooks (optional)**
    - Add one entry to each of the `hooks.PostToolUseFailure` and `hooks.UserPromptSubmit` arrays, beside the entries already there.
    - Keep `onFailure` at `continue`: the route only adds context, so a stopped `serve` must not block a prompt or a tool result.
    - Without these entries the standby gaps are still recorded and shown in `GET /api/power`, but the agent receives no hint.

```json
{"hooks":[{"type":"http","url":"http://127.0.0.1:47333/hook/standby-hint","timeout":5,"onFailure":"continue"}]}
```

### 4. Check

1. **Doctor**
    - `doctor` prints each finding as `[level] topic: message`, with a `->` fix line when there is one, and checks the data directory, database, ports, relay target, hook registration, allowed URL, telemetry, gateway, and projects directory.
    - Aim for no `fail` or `warn` lines; without a guard or the hook chain, the `relay-target` and `hook-registration` warnings are expected.
2. **Status**
    - `status` prints the admin `/api/status` JSON: version, pid, uptime, data directory, port state, and live counters.
    - After a Claude Code session, `otlp_batches` in `status` should increase.

```bat
claude-monitor doctor
claude-monitor status
```

### 5. Keep It Running

1. **Task Scheduler**
    - This registers a task `claude-monitor` that runs `conhost.exe --headless "<exe>" serve` at logon as the current user.
    - Restart on failure: up to 999 times, 1 minute apart; no execution time limit; a second instance is ignored.
    - The task starts at the next logon; start it now with `Start-ScheduledTask -TaskName claude-monitor`.
    - `$exe` resolves the exe from the PATH; set it to the full exe path for a source build that is not on the PATH.

```powershell
$exe = (Get-Command claude-monitor).Source
$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = New-ScheduledTaskAction -Execute "conhost.exe" -Argument "--headless `"$exe`" serve" -WorkingDirectory (Split-Path -Parent $exe)
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew -StartWhenAvailable
Register-ScheduledTask -TaskName claude-monitor -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force
```

2. **Supervision**
    - When the admin or hooks server ends or panics, the whole process exits with a non-zero code, so the scheduler restarts it instead of leaving a half-working `serve`.
    - When OTLP, the indexer, the report scheduler, or the gateway ends, `serve` prints a warning, sends a warn notice to the dashboard, and keeps running.
    - A second `serve` that finds admin or hooks taken exits with an error and leaves the running instance's token alone.
3. **Start without the dashboard**
    - `start` makes sure a detached `serve` is running and exits without opening the dashboard; use it for a logon launcher of your own.

```bat
claude-monitor start
```

### 6. Stop And Uninstall

1. **Stop**
    - Remove the `pretool-chain` entry from `settings.json` first, or every Bash call is blocked while `serve` is down.
    - Stop a task-run `serve` with `Stop-ScheduledTask -TaskName claude-monitor`; stop a detached one with `Stop-Process -Name claude-monitor`, which also closes an open dashboard; `Ctrl+C` ends a foreground `serve`.
2. **Uninstall**
    - Unregister the task, remove the `env`, `hooks`, and `allowedHttpHookUrls` entries that you added, then remove the binary (`cargo uninstall` applies only to a `cargo install`).
    - The data directory `~/.config/claude-monitor/` (config file, database, token, log, reports, and dashboard state) stays until you delete it.

```powershell
Unregister-ScheduledTask -TaskName claude-monitor -Confirm:$false
cargo uninstall claude-monitor
```

### 7. Troubleshooting

1. **Every Bash call is blocked**
    - The hook chain is registered with `onFailure: "block"` while `serve` is down: run `claude-monitor start`, or remove the registration.
2. **`doctor` warns about `relay-target`**
    - No guard answers `GET /health` at `hooks.relay_target`: start the guard, point `hooks.relay_target` at it, or leave the hook chain unregistered.
3. **A port is in use**
    - `serve` exits with an error when admin or hooks is taken, and skips OTLP or gateway with a warning; change `[ports]` in `config.toml` and restart.
4. **`otlp_batches` stays at 0**
    - Run `doctor` and fix the `telemetry` finding, then start a new Claude Code session; the receiver accepts JSON only (`415` otherwise).
5. **API calls fail after setting `ANTHROPIC_BASE_URL`**
    - The gateway is not running or is turned off: start `serve`, or remove the key.
6. **`serve` exits with a configuration error**
    - A configuration error exits with code 2: check unknown keys, ports, and the limits in Configuration.
7. **`POST` returns 401**
    - The admin token is regenerated on every `serve` start; read the current `admin.token`.

---

## Commands

1. **serve**
    - Runs the resident server: admin, OTLP, hooks, gateway, transcript indexer, and report scheduler.
2. **start**
    - Makes sure a detached `serve` is running and exits without opening the dashboard.
3. **tui**
    - Opens the live dashboard only. With no command at all, the exe first makes sure a detached `serve` is running and then opens the dashboard.
4. **index**
    - Indexes the session transcripts once and prints a JSON summary: files scanned and changed, bytes read, elapsed milliseconds, and row counts.
5. **report**
    - Writes the daily report and prints its path; `--date YYYY-MM-DD` picks the day, and the default is yesterday.
6. **hook**
    - `hook <route>` is a command-hook client: it sends stdin to `http://127.0.0.1:<hooks port>/hook/<route>` and prints the reply.
    - The route is one segment of `a-z`, `0-9`, and `-`, or `relay/<name>`.
    - It exits 0 on a 2xx reply and 1 otherwise; the stdin limit is 16 MiB.
    - A route that starts with `pretool` fails closed: on an oversized stdin, a send failure, or a non-2xx reply it prints the deny JSON and exits 0; `relay/*` routes exit 1.
7. **status**
    - Prints the admin status JSON.
8. **doctor**
    - Prints the findings described in Check; it reports `fail` when `pretool-chain` is registered but the hooks port does not answer `/health` (or `hooks.enabled` is false), because every Bash call would be blocked.
9. **Global options**
    - `--config <path>` selects the config file (default `~/.config/claude-monitor/config.toml`); a configuration error exits with code 2, and other errors exit with 1.
    - `--version` and `--help` are the standard clap options.

---

## Dashboard

1. **Tabs**
    - Seven tabs with keys `1`-`7`: overview, tools, hooks, models, agents, settings (`6`), and report (`7`).
    - There is no gateway tab; gateway timing (first byte, first token, and total time, p50/p90) is available through `GET /api/gateway` and in the daily report, and the overview server-status card shows the gateway port as a status dot.
    - The dashboard reads `cm.sqlite` read-only on a background thread every 2 seconds, and the heavy panels (tools, hooks, model stats, agent stats, cache breaks) every 10 seconds; the first read, a window change, and `r` refresh them at once.
    - The screen is redrawn only when something changed, and at least once per second.
    - When the tab labels do not fit, the tab bar first removes the gaps between tabs and then switches to numbered compact labels, so all seven tabs stay visible and clickable down to 60 columns.
    - Screen text is English by default and switches to Korean with `L` (see Display language); the daily report text is always Korean.
2. **Keys**
    - `1`-`7`, `Tab`, `Shift-Tab`: switch tab
    - `Up`, `Down`, `PgUp`, `PgDn`, `Home`, `End`: scroll
    - `w`: cycle the window through 1 hour, 24 hours, and 7 days
    - `s`, `S`: next sort column (numeric and time columns start descending, text columns ascending) and the sort cycle of the current column (first direction, opposite direction, card default) of the focused table card
    - `Left`, `Right`: report date, report tab only
    - `g`: generate the report, report tab only
    - `i`: reindex
    - `g` and `i` go through the admin API and need a running `serve`
    - `t`: toggle always-on-top
    - `Shift+Up`, `Shift+Down`, `Shift+Left`, `Shift+Right`: move the focused card one slot (see Card order)
    - `l`: reset the card sizes, card order, and dragged column widths of the current tab
    - `L` (Shift+l): switch the display language between English and Korean
    - `r`: refresh, `?`: help, `q`, `Esc`, `Ctrl+C`: quit
3. **Mouse**
    - Every tab is a set of bordered cards: click a tab to switch, a card to focus it, and a table row to select it (details appear in the side card).
    - Click a table header to sort by that column; a numeric or time header starts descending (largest or latest first) and a text header ascending, and clicking the same header again flips the direction, then a third click restores the table's default sort. This holds for every table card, the event feed and the four settings cards included.
    - The wheel scrolls the card under the pointer; a scrollable card shows a scrollbar whose thumb can be dragged, and a click on the track moves one screen. A card whose body is one row high still shows a one-cell thumb in the gutter and the ` a-b / total ` counter, but has nothing to drag.
    - Drag a boundary between cards to resize them, and drag a boundary between two header cells to resize a column (every table card, the event feed and the settings cards included); a double click on a column boundary or `l` restores the automatic layout, and the layout is saved in the state file (see State file).
    - Press and hold a card title for 400 ms, then drag, to move the card (see Card order).
    - A button release lost outside the window ends every drag the way a release does: the first move without a button or the next press commits a moved card boundary, column width, or scrollbar drag (and saves the layout), and a tab switch commits it instead of dropping it.
    - The bottom bar holds clickable buttons for reindex, window, refresh, top-most, layout reset, language, help, and quit; the report tab adds previous and next date and generate buttons.
4. **Views**
    - The event feed is a table card with a pinned header (time, kind, name, duration, agent, input, output, cache read) that follows the same column rules as the other tables: text columns get their natural width before any `…`, text columns shrink before the numeric ones, and the right-most columns hide last, with the name and agent columns keeping a readable minimum width first. A hook row shows route, guard, and detail in the name cell and its verdict in the agent cell.
    - The settings tab shows hooks, MCP servers, agents, and diagnostics as table cards with the same header sort and draggable columns; the allow list, load warnings, and the skill and env summary continue below the table rows as auxiliary lines in the same scroll, and a diagnostic's fix hint stays in its message cell.
    - A card only one row high shows its first body row instead of the header, and the sort stays reachable with `s` and `S`.
    - The top-most state has its own purple fill on the app-bar `Pinned` badge and the `[t] Pin ON` chip, together with the words; the warn color is reserved for stale and warning states.
    - The models tab shows per-model stats (requests, sessions, agent types, tokens, cache hit ratio, wait p50/p90, estimated cost, last seen) with a detail card and the cache-break candidates.
    - The agents tab shows per-agent-type stats (runs, requests, sessions, tokens, estimated cost, wait p50/p90, tool calls and errors, top model, last seen) with a detail card.
    - The report view shows box tables verbatim and converts legacy pipe-table reports to box tables when displaying them.
    - The server-status card marks each port with a dot: a filled green dot when it listens, a hollow dot when it is turned off or unknown, and a hollow bold red dot when it should listen but does not; whole items drop from the right when the card is narrow.
    - A card title wider than its card drops its middle parts first and keeps the first part and the sort mark, and the border hint drops before the title does.
5. **Console**
    - In a classic console window the dashboard applies the `D2Coding ligature` font while it runs and restores the original font on exit.
6. **Card order**
    - It applies to every card group whose panes are single cards: the KPI, trend, and feed rows of the overview, and the tools, hooks, models, agents, settings, and report tabs.
    - Press and hold a card title for 400 ms to enter move mode; the lifted card gets a dashed accent border and its title shows a move mark.
    - Drag to preview the new order and release to commit it; the slot sizes stay where they are and only the cards move between slots.
    - `Esc`, a tab switch, or a terminal resize cancels the move and restores the previous order.
    - Releasing before 400 ms is an ordinary click, and dragging before 400 ms starts neither a move nor a resize.
    - `Shift+arrow` moves the focused card one slot without a drag: `Up` and `Down` in a stacked group, `Left` and `Right` in a side-by-side group, and the other arrows do nothing.
    - `l` resets the card sizes, the card order, and the dragged column widths of the current tab.
7. **Display language**
    - English is the default; `L` (Shift+l) switches between English and Korean at runtime and saves the choice.
    - The footer chip `[L] EN` or `[L] KO` shows the current language and is clickable; it sits after the other left chips and before help and quit, and it is the last left chip to drop, so it is still visible at the minimum size (60x16) while the other left chips disappear from the right.
    - Tab names, labels, the help overlay, and notices follow the language; data values such as paths, model and tool names, and log text are not translated, and the daily report text stays Korean.
8. **State file**
    - `~/.config/claude-monitor/tui.toml`, next to `config.toml` (it stays there even when the data directory moves), holds `version = 1`, `language`, the card orders, the split ratios, and the table column widths, keyed by stable names such as `settings_rows`, `event_feed`, `tool_table`, and `settings_hooks`.
    - It is written atomically (a temporary file, then a rename) when a change is committed: the end of a card move or a boundary or column drag, `Shift+arrow`, `l`, and `L`.
    - The write runs on a dedicated writer thread that the input thread feeds through a channel: a burst of commits collapses into the latest state, the terminal guard flushes the last commit and joins the thread on every exit path but waits at most 5 seconds, after which it prints a warning on stderr and exits, and a failed write comes back as an error toast.
    - When `tui.toml` is a symbolic link (for example into a synced folder), the dashboard follows the whole link chain (up to 16 links), writes the temporary file next to the real file, and renames it over the real file, so every link in the chain stays in place; a chain deeper than 16 links fails the save with an error.
    - At start only stale temporary files are removed (not modified for 10 minutes and not owned by the running process), so the live temporary file of another running instance survives.
    - A missing file means the defaults (English and the default layout).
    - An unreadable file, an unsupported version, or an invalid entry (an unknown name or a wrong length or range) is skipped with one notice, and the other entries still apply; an unreadable or other-version file is first copied to `tui.toml.bak`.

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
    - `GET /api/power` returns `enabled`, `keep_awake`, `holding` (the sleep postponement is active), and `gaps`, the last 32 standby gaps since the `serve` start as `id`, `start_ms`, `end_ms` (Unix milliseconds), and `active` (Claude Code was active right before the standby).
2. **Token-protected endpoints**
    - `POST /api/reindex` runs the transcript indexer once.
    - `POST /api/report?date=YYYY-MM-DD` generates and stores a report; the default date is yesterday.
    - Every `POST` needs the token in the header `x-cm-token`; a missing or wrong token returns `401`.
3. **Token**
    - The token is 32 random bytes from the OS, written as hex to `admin.token` in the data directory, and regenerated on every `serve` start.

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
    - `POST /hook/pretool-chain`, `POST /hook/relay/{route}`, `POST /hook/standby-hint`, and `GET /health`.
2. **Standby hint route**
    - It reads only `hook_event_name`, `session_id`, and `duration_ms` from the payload.
    - For `PostToolUseFailure` with `duration_ms`, it answers with `additionalContext` when a standby gap overlaps the tool run, on every such failure.
    - For `UserPromptSubmit`, it answers with `additionalContext` for gaps that ended within `power.hint_window_secs` and began while Claude Code was active (a gateway request, hook call, or open stream within `power.awake_grace_secs` before the standby), once per session and gap.
    - A failure without `duration_ms` follows the prompt rule but gets a conditional hint: treat the failure as standby-caused only when the error is a timeout or a dropped connection.
    - Any other event, an unreadable body, `power.enabled = false`, or no matching gap returns `{}`; the call is recorded with route `standby-hint`, and the guard label `standby-hint` marks an answer that carried a hint.
3. **Relay route**
    - `{route}` is `a-z`, `0-9`, and `-` only.
    - The request is forwarded unchanged to `{hooks.relay_target}/hook/{route}`, and the upstream status and body come back as they are.
    - The verdict, guard label, and duration are recorded; an unreachable upstream returns `502`.
4. **Body limit**
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
    - `pretool-chain` rewrites a legacy CLI command, then forwards the payload to your guard at `{hooks.relay_target}/hook/pretool-guard` (default `http://127.0.0.1:47321`).
    - The guard is not part of this project; it must already answer there.
2. **Outcomes**
    - Guard unreachable, late, non-2xx, or not a JSON object: deny with the reason `[cm] pretool-guard 응답 없음 (fail-closed)`.
    - Otherwise the guard's decision stands and cm never grants an allow the guard did not give; a rewritten command carries an `updatedInput` and an `additionalContext` note naming each conversion.
    - [ARCHITECTURE.md](ARCHITECTURE.md) lists every decision case.
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
    - The rewritten commands need `bat`, `eza`, and `delta` on the PATH of the machine that runs Claude Code.

---

## Data And Privacy

### 1. Data Directory

1. **Location**
    - `~/.config/claude-monitor/` holds every file the program keeps; `~` is `USERPROFILE`, or `HOME` when `USERPROFILE` is unset or empty, so on Windows it is `%USERPROFILE%\.config\claude-monitor\`.
    - `CM_DATA_DIR`, or `data_dir` in `config.toml`, moves everything except `config.toml` to another folder; the config file stays at its own path.
2. **Contents**
    - `config.toml`: the optional config file; you create it, and the program only reads it.
    - `cm.sqlite`: the database, in WAL mode, with `cm.sqlite-wal` and `cm.sqlite-shm` beside it while it is open.
    - `admin.token`: the admin token of the current `serve` run.
    - `serve.log`: the output of the detached `serve` that a no-command or `start` launch creates, recreated on each launch.
    - `reports\YYYY-MM-DD.md`: daily reports; each is also stored in the database.
    - `tui.toml`: the dashboard state (display language, card order, card sizes, column widths); the dashboard writes it and a missing file means the defaults.
3. **Creation**
    - The folder is created when the first command that writes needs it, such as `serve`, `start`, or `index`; `doctor` only warns when it is missing.

### 2. Stored Data

1. **Metadata only**
    - Model and agent names, tool names, session ids, project folder names, timestamps, durations, and token counts.
    - Hook verdicts: route, guard label, outcome, and the names of detected legacy commands.
2. **Never stored**
    - Prompt text, command text, file contents, tool inputs and outputs, and auth headers.
    - While parsing, the transcript reader holds up to 400 characters of a tool result in memory; it uses them only to read a guard label and a command name from an error result and writes only those two.
3. **Config inspection**
    - `doctor` and the settings tab read `settings.json` and `.claude.json` on demand; env values are compared, never printed or stored.
4. **Duplicates**
    - The same request can arrive through the transcript, OTLP, and the gateway, so the aggregates count each source for its own purpose; [ARCHITECTURE.md](ARCHITECTURE.md) lists the keys and rules.

---

## Configuration

### 1. Resolution

1. **Order**
    - `--config <path>`, then `CM_CONFIG`, then `~/.config/claude-monitor/config.toml`, then the defaults.
    - `~` is `USERPROFILE`, or `HOME` when `USERPROFILE` is unset or empty; on Windows that is `%USERPROFILE%\.config\claude-monitor\config.toml`.
    - Only that path is searched, so a `CM_DATA_DIR` folder is never searched for a config file, and the file is read when a command starts, so restart `serve` after editing it.
2. **Strictness**
    - Unknown keys are an error; ports must be non-zero and distinct, and `report.run_hour` must be 0-23.
    - `hooks.relay_target` accepts only an `http` loopback address (`127.0.0.1`, `localhost`, or `[::1]`) on a port that is not one of cm's own ports; anything else is a configuration error, so the start fails.
    - `hooks.guard_deadline_ms` must be between 1 and 4500, so even the two guard calls of the `defer` re-check fit inside the 10-second hook timeout.
    - `power.gap_threshold_secs` must be at least 15, so the 5-second monitor tick and its jitter never count as a standby gap.
3. **Data directory**
    - `data_dir` in `config.toml`, then `CM_DATA_DIR`, then `~/.config/claude-monitor`, the same folder as the default config file.
    - When none of `data_dir`, `CM_DATA_DIR`, `USERPROFILE`, and `HOME` is available, the start fails with a configuration error.
4. **Claude home**
    - `claude_home` in `config.toml`, then `CLAUDE_CONFIG_DIR`, then `%USERPROFILE%\.claude`, then `$HOME/.claude` (Linux and macOS).
5. **Template**
    - Copy `docs/config.example.toml` from the repository to `~/.config/claude-monitor/config.toml` and edit it; the same file is in the crates.io package.

### 2. config.toml

1. **Keys**
    - Every key is optional; this block shows the defaults, and `docs/config.example.toml` carries the same keys.
    - `gateway.enabled` defaults to `true`; `false` leaves port `47332` untouched.
    - `gateway.upstream_timeout_secs` limits the wait for upstream response headers (504 on timeout); a streamed body has no deadline after the headers arrive. It must be at least 1.
    - `power.enabled` starts the standby monitor thread; `power.keep_awake` asks Windows to postpone system sleep (`SetThreadExecutionState` with `ES_SYSTEM_REQUIRED`, never `ES_DISPLAY_REQUIRED`) while a gateway stream is open or a gateway request or hook call happened within `power.awake_grace_secs`.
    - A wall-clock jump larger than `power.gap_threshold_secs` between two monitor ticks is recorded as a standby gap; `power.hint_window_secs` is how long after a gap a prompt still gets the hint.

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

[power]
enabled = true
keep_awake = true
awake_grace_secs = 600
gap_threshold_secs = 60
hint_window_secs = 1800
```

2. **Report schedule**
    - On start, `serve` waits for the first index run (up to 10 minutes; a warn notice is sent on timeout) and then regenerates yesterday's report, replacing any stored one.
    - Then it writes the previous day's report daily at `run_hour:05` local time.
    - A report covers one local calendar day; its sections are summary, comparison with the previous day, model and agent usage with an estimated cost, top tool latencies, hook verdicts, top cache breaks, gateway timing when present, and rule-based suggestions.
    - Every table in the report file is a light box table (`┌─┬─┐`) inside a `text` fence under its section heading, with Hangul and CJK counted as two columns; there are no pipe tables.

---

## Measurements

1. **Scope**
    - These are measurements from the author's PC on 2026-11-15, not guarantees; other machines and workloads will differ.
2. **Transcript backfill**
    - A 7-day backfill read 453 files and 602 MB in 1.7 s on the release build.
3. **Hook chain overhead**
    - `pretool-chain` added about 1.6 ms per Bash call over the guard alone: 3.6 ms against 2.0 ms on a reused connection.
4. **Denials**
    - The author's 7-day history showed 266 legacy-CLI guard denials, each costing a median 4.2 s extra model round trip.
    - The rewrite rules cover `cat` and `ls` but not `grep`, so `grep` denials remain.
5. **Network**
    - The round trip to the API is about 190 ms, which a local proxy cannot reduce.
6. **Where the gain comes from**
    - The speed gain comes from avoided denials and avoided hook process spawns, not from network tricks.
    - The dashboard's saved time is an estimate: rewrites multiplied by 4200 ms.
    - The gateway exists to measure first byte and first token; its own added latency was not measured.

---

## Build And Test

1. **Release build**
    - `cargo build --release` produces `target\release\claude-monitor.exe`; see Install for toolchain notes.
    - The icon comes from `assets/icon`: two lossless WebP masters, `claude-monitor.webp` (a 1024 px tile for 40 px and larger) and `claude-monitor-small.webp` (a 256 px hexagon crop for 32 px and smaller).
    - Regenerate the multi-size `claude-monitor.ico` with `python assets/icon/build_icon.py`, which needs ImageMagick; `build.rs` embeds the `.ico` together with the version info and the manifest at build time, and the crates.io package keeps the `.ico` and leaves out the WebP masters and the script.
2. **Checks**
    - Run these from the repository root after every change.

```bat
cargo clippy --all-targets -- -D warnings
cargo test
cargo fmt --check
```

3. **Test layout**
    - Unit tests sit beside their modules under `#[cfg(test)]`, or in `tests.rs` and `test_support.rs` files.
    - `tests/fixtures` holds the fixture Claude homes, transcripts, and OTLP payloads that the tests read.
4. **Coverage**
    - The rewrite table, the chain with a fake guard (allow, deny, deadline, rewrite, recheck), the hook client fail-closed output, and the hook server on temporary ports.
    - The admin token and aggregates, the OTLP mapping and its size limits, the gateway 502 and 504 paths, the SSE usage parser, and the indexer.
    - The port bind and serve supervision rules, the doctor checks, the report flow, the dashboard rendering and mouse handling, and the standby monitor.

---

## Limitations

1. **Gateway**
    - It does not honor `HTTPS_PROXY`.
    - A response with a `Content-Encoding` header is relayed and timed, but its token usage is not read.
2. **Windows networking**
    - On the author's Windows host, HTTP/1.1 requests with `Connection: close` sometimes lose the response (RST), so the built-in blocking clients avoid that header; the cause is unconfirmed.
3. **OTLP**
    - The mapping is tested against fixture files only; it is untested against a live Claude Code export.
    - `DISABLE_TELEMETRY=1` does not block the OTLP export (measured 2026-11-16: `otlp_batches` rose with it set).
4. **Guard dependency**
    - The chain needs an existing guard server at `hooks.relay_target`; without one every Bash call is denied.
5. **Partial start**
    - When the OTLP or gateway port cannot bind, `serve` runs without that server and only prints a warning, so check `doctor` and the start-up log.
    - Once running, the end of OTLP, the indexer, the report scheduler, or the gateway is only announced; the end of admin or hooks stops `serve`.
6. **Language and platform**
    - The daily report text is Korean only, and the dashboard text is English or Korean (`L`); the default data directory needs `USERPROFILE`, `HOME`, or `CM_DATA_DIR`.
7. **Cost estimate**
    - It uses a built-in per-model rate table; an unknown model counts as zero.
8. **Read endpoints**
    - `GET` endpoints on the admin port need no token; they return metadata only, behind the Host and Origin checks.
9. **Standby**
    - Whether Windows honors the system-required request when a policy turns the screen off on a Modern Standby (S0) machine is unverified; when a gap is recorded while the request was held, the notice and `serve.log` say `절전 유예 중`, which shows that the request did not prevent standby.
    - The gap ledger lives in memory only and resets when `serve` restarts.
    - A long background job that makes no gateway request or hook call for `power.awake_grace_secs` before the standby counts as idle, so prompts after that gap get no hint.
    - A wall-clock step forward of more than `power.gap_threshold_secs` (manual or NTP correction) is recorded as a gap; a step backward ends the sleep postponement until the next activity.
    - A gap is seen only when the whole process is paused; a network drop without standby is not detected.

---

## License

1. **Apache-2.0**
    - See [LICENSE.md](LICENSE.md).
