//! net/gateway/ratelimit.rs
//! 응답 헤더의 rate-limit 값 추출: 문서화된 헤더와 retry-after, 미문서 anthropic-ratelimit-* 는 JSON 으로 보존.

use crate::store::ratelimit::RatelimitSnapshotRow;
use chrono::{SecondsFormat, Utc};
use hyper::HeaderMap;
use std::collections::BTreeMap;

const RATELIMIT_PREFIX: &str = "anthropic-ratelimit-";
const RETRY_AFTER_HEADER: &str = "retry-after";
const MAX_EXTRA_ENTRIES: usize = 32;
const MAX_VALUE_CHARS: usize = 128;

// 1. 스냅샷 추출 --------------------------------------------------------------
// 읽는 헤더는 retry-after 와 anthropic-ratelimit-* 뿐 (인증 헤더·본문 미접근),
// 해당 헤더가 하나도 없으면 429 만 빈 값으로 기록
pub fn extract_ratelimit_snapshot(
    headers: &HeaderMap,
    status: u16,
) -> Option<RatelimitSnapshotRow> {
    let mut snapshot = RatelimitSnapshotRow {
        ts: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        status: Some(i64::from(status)),
        ..RatelimitSnapshotRow::default()
    };
    let mut extra: BTreeMap<String, String> = BTreeMap::new();
    let mut found = false;
    for (name, value) in headers.iter() {
        let name = name.as_str();
        let Ok(text) = value.to_str() else {
            continue;
        };
        let text: String = text.trim().chars().take(MAX_VALUE_CHARS).collect();

        // 1. retry-after (초 단위만 컬럼, 날짜 형식은 extra)
        if name == RETRY_AFTER_HEADER {
            found = true;
            match text.parse::<f64>() {
                Ok(seconds) if seconds.is_finite() && seconds >= 0.0 => {
                    snapshot.retry_after_secs = Some(seconds.ceil() as i64);
                }
                _ => {
                    extra.insert(name.to_string(), text);
                }
            }
            continue;
        }
        let Some(suffix) = name.strip_prefix(RATELIMIT_PREFIX) else {
            continue;
        };
        found = true;

        // 2. 문서화된 4종 x (limit, remaining, reset)
        let count_slot = match suffix {
            "requests-limit" => Some(&mut snapshot.requests_limit),
            "requests-remaining" => Some(&mut snapshot.requests_remaining),
            "tokens-limit" => Some(&mut snapshot.tokens_limit),
            "tokens-remaining" => Some(&mut snapshot.tokens_remaining),
            "input-tokens-limit" => Some(&mut snapshot.input_tokens_limit),
            "input-tokens-remaining" => Some(&mut snapshot.input_tokens_remaining),
            "output-tokens-limit" => Some(&mut snapshot.output_tokens_limit),
            "output-tokens-remaining" => Some(&mut snapshot.output_tokens_remaining),
            _ => None,
        };
        if let Some(slot) = count_slot
            && let Ok(count) = text.parse::<i64>()
        {
            *slot = Some(count);
            continue;
        }
        let reset_slot = match suffix {
            "requests-reset" => Some(&mut snapshot.requests_reset),
            "tokens-reset" => Some(&mut snapshot.tokens_reset),
            "input-tokens-reset" => Some(&mut snapshot.input_tokens_reset),
            "output-tokens-reset" => Some(&mut snapshot.output_tokens_reset),
            _ => None,
        };
        if let Some(slot) = reset_slot {
            *slot = Some(text);
            continue;
        }

        // 3. 그 외 (미문서 unified 등, 파싱 실패한 문서화 값 포함): 개수 상한 안에서 보존
        if extra.len() < MAX_EXTRA_ENTRIES {
            extra.insert(name.to_string(), text);
        }
    }
    if !found && status != 429 {
        return None;
    }
    snapshot.extra_json = serde_json::to_string(&extra).unwrap_or_else(|_| "{}".to_string());
    Some(snapshot)
}

#[cfg(test)]
mod tests {
    use super::extract_ratelimit_snapshot;
    use hyper::HeaderMap;
    use hyper::header::{HeaderName, HeaderValue};

    fn build_headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                HeaderName::from_bytes(name.as_bytes()).expect("헤더 이름"),
                HeaderValue::from_str(value).expect("헤더 값"),
            );
        }
        headers
    }

    // 1. 문서화된 헤더 전부 ---------------------------------------------------
    #[test]
    fn documented_headers_fill_every_column() {
        let headers = build_headers(&[
            ("anthropic-ratelimit-requests-limit", "50"),
            ("anthropic-ratelimit-requests-remaining", "49"),
            ("anthropic-ratelimit-requests-reset", "2026-11-19T00:01:00Z"),
            ("anthropic-ratelimit-tokens-limit", "40000"),
            ("anthropic-ratelimit-tokens-remaining", "39000"),
            ("anthropic-ratelimit-tokens-reset", "2026-11-19T00:01:01Z"),
            ("anthropic-ratelimit-input-tokens-limit", "30000"),
            ("anthropic-ratelimit-input-tokens-remaining", "29000"),
            (
                "anthropic-ratelimit-input-tokens-reset",
                "2026-11-19T00:01:02Z",
            ),
            ("anthropic-ratelimit-output-tokens-limit", "8000"),
            ("anthropic-ratelimit-output-tokens-remaining", "7000"),
            (
                "anthropic-ratelimit-output-tokens-reset",
                "2026-11-19T00:01:03Z",
            ),
            ("retry-after", "12"),
            ("x-api-key", "secret-key"),
            ("authorization", "Bearer secret"),
        ]);
        let snapshot = extract_ratelimit_snapshot(&headers, 429).expect("스냅샷");
        assert_eq!(snapshot.status, Some(429));
        assert_eq!(snapshot.requests_limit, Some(50));
        assert_eq!(snapshot.requests_remaining, Some(49));
        assert_eq!(
            snapshot.requests_reset.as_deref(),
            Some("2026-11-19T00:01:00Z")
        );
        assert_eq!(snapshot.tokens_limit, Some(40000));
        assert_eq!(snapshot.tokens_remaining, Some(39000));
        assert_eq!(
            snapshot.tokens_reset.as_deref(),
            Some("2026-11-19T00:01:01Z")
        );
        assert_eq!(snapshot.input_tokens_limit, Some(30000));
        assert_eq!(snapshot.input_tokens_remaining, Some(29000));
        assert_eq!(
            snapshot.input_tokens_reset.as_deref(),
            Some("2026-11-19T00:01:02Z")
        );
        assert_eq!(snapshot.output_tokens_limit, Some(8000));
        assert_eq!(snapshot.output_tokens_remaining, Some(7000));
        assert_eq!(
            snapshot.output_tokens_reset.as_deref(),
            Some("2026-11-19T00:01:03Z")
        );
        assert_eq!(snapshot.retry_after_secs, Some(12));
        assert_eq!(snapshot.extra_json, "{}");
        assert!(!snapshot.ts.is_empty());
    }

    // 2. 미문서 unified 헤더 보존, 인증 헤더 제외 -----------------------------
    #[test]
    fn unknown_ratelimit_headers_go_to_extra_json_without_auth() {
        let headers = build_headers(&[
            ("anthropic-ratelimit-unified-status", "allowed"),
            ("anthropic-ratelimit-unified-5h-utilization", "0.42"),
            ("anthropic-ratelimit-requests-remaining", "not-a-number"),
            ("authorization", "Bearer secret"),
            ("x-api-key", "secret-key"),
        ]);
        let snapshot = extract_ratelimit_snapshot(&headers, 200).expect("스냅샷");
        assert_eq!(snapshot.requests_remaining, None);
        let extra: serde_json::Value = serde_json::from_str(&snapshot.extra_json).expect("JSON");
        assert_eq!(extra["anthropic-ratelimit-unified-status"], "allowed");
        assert_eq!(extra["anthropic-ratelimit-unified-5h-utilization"], "0.42");
        assert_eq!(
            extra["anthropic-ratelimit-requests-remaining"],
            "not-a-number"
        );
        assert!(!snapshot.extra_json.contains("secret"));
    }

    // 3. 헤더 없음: 429 만 기록 -----------------------------------------------
    #[test]
    fn missing_headers_record_only_for_429() {
        let headers = build_headers(&[("content-type", "application/json")]);
        assert!(extract_ratelimit_snapshot(&headers, 200).is_none());
        assert!(extract_ratelimit_snapshot(&headers, 500).is_none());
        let rejected = extract_ratelimit_snapshot(&headers, 429).expect("429 스냅샷");
        assert_eq!(rejected.status, Some(429));
        assert_eq!(rejected.requests_limit, None);
        assert_eq!(rejected.retry_after_secs, None);
        assert_eq!(rejected.extra_json, "{}");
    }

    // 4. retry-after 형식 -----------------------------------------------------
    #[test]
    fn retry_after_accepts_seconds_and_keeps_dates_in_extra() {
        let seconds = build_headers(&[("retry-after", "3")]);
        let snapshot = extract_ratelimit_snapshot(&seconds, 429).expect("초 단위");
        assert_eq!(snapshot.retry_after_secs, Some(3));

        let date = build_headers(&[("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")]);
        let snapshot = extract_ratelimit_snapshot(&date, 429).expect("날짜 형식");
        assert_eq!(snapshot.retry_after_secs, None);
        assert!(snapshot.extra_json.contains("retry-after"));
    }

    // 5. 미문서 헤더 개수 상한 ------------------------------------------------
    #[test]
    fn extra_entries_are_bounded() {
        let names: Vec<String> = (0..100)
            .map(|index| format!("anthropic-ratelimit-unified-{index}"))
            .collect();
        let pairs: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "v")).collect();
        let snapshot = extract_ratelimit_snapshot(&build_headers(&pairs), 200).expect("스냅샷");
        let extra: serde_json::Value = serde_json::from_str(&snapshot.extra_json).expect("JSON");
        assert_eq!(extra.as_object().expect("객체").len(), 32);
    }
}
