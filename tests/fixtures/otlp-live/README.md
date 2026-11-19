# OTLP live fixtures

## Overview

1. **What they are**
    - Sanitized OTLP exports captured from a real Claude Code CLI run (`claude -p`, one Bash tool call).
    - Used by `src/net/otlp/live_tests.rs` to check the log mapping against real payloads instead of hand-written ones.
2. **Capture**
    - Claude Code version: `2.1.328` (`service.version` in the resource attributes).
    - Date: 2026-11-19.
    - Exporter: `OTel-OTLP-Exporter-JavaScript/0.208.0`, no `Content-Encoding`, sent to a local capture server.
    - Settings: `OTEL_LOGS_EXPORT_INTERVAL=1000`, `OTEL_METRIC_EXPORT_INTERVAL=2000`, logs and metrics both `otlp`.
3. **Environment note**
    - Process environment variables did not win over the `env` block of the user settings file: with only process variables set, nothing reached the capture server.
    - The captures were taken with the same keys passed through the per-process `--settings` flag.

---

## Files

1. **JSON run (`http/json`)**
    - `logs_json_1.json` to `logs_json_4.json`: four `/v1/logs` bodies in arrival order.
    - `metrics_json_1.json` to `metrics_json_3.json`: three `/v1/metrics` bodies.
2. **Protobuf run (`http/protobuf`)**
    - `logs_proto_1.json` to `logs_proto_3.json` and `metrics_proto_1.json` to `metrics_proto_3.json`.
    - These are the decoded JSON form of the protobuf bodies, produced with the project decoder (`decode_logs_request`, `decode_metrics_request`); `intValue` and `asInt` are strings.
    - The binary protobuf bodies are not stored: re-encoding needs a test-only encoder, so only the decoded JSON is kept.
3. **Numbering**
    - Placeholder numbers run across all files in arrival order, so the JSON run is session `...0001` and the protobuf run is session `...0002`.

---

## Sanitizing

1. **Replaced with stable placeholders**
    - `user.email`, `user.account_uuid`, `user.account_id`, `user.id`, `organization.id`, `session.id`, `prompt.id`, `terminal.type`, `request_id`, `message.uuid`, `tool_use_id`.
    - `plugin.name`, `marketplace.name`, `plugin_id_hash` (local plugin inventory).
    - `prompt`, `prompt_text`, `response` (text only; `prompt_length` and `response_length` keep their real numbers).
    - Any other string value that looks like a path would be replaced; none was found.
2. **Kept as captured**
    - Event names, attribute keys and value types, timestamps, token, cost, and duration values, model ids, tool names, and the envelope structure.
3. **Placeholder shapes**
    - UUID-like keys: `aaaaaaaa-0000-4000-8000-000000000001` with a fixed per-key prefix; emails: `user1@example.invalid`; request ids: `req_FAKE000001`; tool use ids: `toolu_FAKE000001`.
    - The same real value always maps to the same placeholder, so joins between events still work.

---

## Observed shape

1. **Log events**
    - `api_request`, `tool_result`, `hook_execution_complete` map to rows.
    - `assistant_response`, `hook_execution_start`, `hook_registered`, `managed_settings_resolved`, `mcp_server_connection`, `plugin_loaded`, `tool_decision`, `user_prompt` are counted as ignored.
2. **Metrics**
    - `claude_code.session.count`, `claude_code.cost.usage`, `claude_code.token.usage`, `claude_code.active_time.total` (all Sum); the server only counts them.
