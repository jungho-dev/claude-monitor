//! transcript/lines.rs
//! 기록 한 줄의 최소 역직렬화 형태와 훅 거부 문구 판독.

use serde::Deserialize;
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::fmt;

const TEXT_PREFIX_LIMIT: usize = 400;
const LABEL_LIMIT: usize = 40;
const SKILL_LABEL_LIMIT: usize = 80;
const DENIAL_NAME_MARKERS: [&str; 2] = ["(detected: ", "(감지: "];

// 1. 기록 한 줄 ---------------------------------------------------------------
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TranscriptLine {
    pub r#type: Option<String>,
    pub timestamp: Option<String>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub is_sidechain: Option<bool>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub message: Option<LineMessage>,
}

// 2. 메시지 -------------------------------------------------------------------
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LineMessage {
    pub id: Option<String>,
    pub model: Option<String>,
    pub usage: Option<LineUsage>,
    #[serde(deserialize_with = "read_blocks")]
    pub content: Vec<LineBlock>,
}

// 3. 토큰 사용량 --------------------------------------------------------------
#[derive(Debug, Default, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct LineUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub cache_creation: Option<LineCacheCreation>,
}

// 3-1. 캐시 쓰기 분리 ---------------------------------------------------------
#[derive(Debug, Default, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct LineCacheCreation {
    pub ephemeral_5m_input_tokens: Option<i64>,
    pub ephemeral_1h_input_tokens: Option<i64>,
}

// 4. 내용 블록 ----------------------------------------------------------------
// 도구 입력은 스킬 이름만 읽고 나머지는 건너뜀, 결과 본문은 앞부분만 보관
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LineBlock {
    pub r#type: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub tool_use_id: Option<String>,
    pub is_error: Option<bool>,
    pub content: Option<TextPrefix>,
    pub input: Option<ToolInputPeek>,
}

// 4-1. 도구 입력 엿보기 -------------------------------------------------------
// Skill 도구의 skill 값만 짧은 식별자로 보관 (형식이 다르면 빈 값)
#[derive(Debug, Default)]
pub struct ToolInputPeek {
    pub skill: String,
}

struct ToolInputVisitor;

impl<'de> Deserialize<'de> for ToolInputPeek {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ToolInputVisitor)
    }
}

impl<'de> Visitor<'de> for ToolInputVisitor {
    type Value = ToolInputPeek;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("도구 입력 객체")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ToolInputPeek, A::Error> {
        let mut peek = ToolInputPeek::default();
        while let Some(key) = map.next_key::<String>()? {
            if key == "skill" {
                peek.skill = skill_label(map.next_value::<serde_json::Value>()?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(peek)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ToolInputPeek, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(ToolInputPeek::default())
    }

    fn visit_str<E: serde::de::Error>(self, _text: &str) -> Result<ToolInputPeek, E> {
        Ok(ToolInputPeek::default())
    }

    fn visit_bool<E: serde::de::Error>(self, _value: bool) -> Result<ToolInputPeek, E> {
        Ok(ToolInputPeek::default())
    }

    fn visit_i64<E: serde::de::Error>(self, _value: i64) -> Result<ToolInputPeek, E> {
        Ok(ToolInputPeek::default())
    }

    fn visit_u64<E: serde::de::Error>(self, _value: u64) -> Result<ToolInputPeek, E> {
        Ok(ToolInputPeek::default())
    }

    fn visit_f64<E: serde::de::Error>(self, _value: f64) -> Result<ToolInputPeek, E> {
        Ok(ToolInputPeek::default())
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<ToolInputPeek, E> {
        Ok(ToolInputPeek::default())
    }
}

// 스킬 이름 정규화: 앞 슬래시 제거, 식별자 문자와 길이 한도를 벗어나면 버림
fn skill_label(value: serde_json::Value) -> String {
    let serde_json::Value::String(text) = value else {
        return String::new();
    };
    let name = text.trim().trim_start_matches('/');
    let valid = !name.is_empty()
        && name.len() <= SKILL_LABEL_LIMIT
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':' | '.'));
    if valid {
        name.to_string()
    } else {
        String::new()
    }
}

// 5. 본문 앞부분 --------------------------------------------------------------
#[derive(Debug, Default)]
pub struct TextPrefix(pub String);

struct TextPrefixVisitor;

#[derive(Default, Deserialize)]
#[serde(default)]
struct TextPart {
    text: Option<TextPrefix>,
}

impl<'de> Deserialize<'de> for TextPrefix {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(TextPrefixVisitor)
    }
}

impl<'de> Visitor<'de> for TextPrefixVisitor {
    type Value = TextPrefix;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("문자열 또는 텍스트 배열")
    }

    fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<TextPrefix, E> {
        let mut end = text.len().min(TEXT_PREFIX_LIMIT);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Ok(TextPrefix(text[..end].to_string()))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<TextPrefix, E> {
        Ok(TextPrefix::default())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<TextPrefix, A::Error> {
        let mut first = String::new();
        while let Some(part) = seq.next_element::<TextPart>()? {
            if first.is_empty() {
                first = part.text.map(|text| text.0).unwrap_or_default();
            }
        }
        Ok(TextPrefix(first))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<TextPrefix, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(TextPrefix::default())
    }
}

// 6. 블록 배열 ----------------------------------------------------------------
// 문자열 본문은 블록 없음으로 취급
struct BlocksVisitor;

fn read_blocks<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<LineBlock>, D::Error> {
    deserializer.deserialize_any(BlocksVisitor)
}

impl<'de> Visitor<'de> for BlocksVisitor {
    type Value = Vec<LineBlock>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("문자열 또는 블록 배열")
    }

    fn visit_str<E: serde::de::Error>(self, _text: &str) -> Result<Vec<LineBlock>, E> {
        Ok(Vec::new())
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Vec<LineBlock>, E> {
        Ok(Vec::new())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<LineBlock>, A::Error> {
        let mut blocks = Vec::new();
        while let Some(block) = seq.next_element::<LineBlock>()? {
            blocks.push(block);
        }
        Ok(blocks)
    }
}

// 7. 훅 거부 표지 -------------------------------------------------------------
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenialMark {
    pub route: String,
    pub guard: String,
    pub command: String,
}

// 8. 훅 거부 판독 -------------------------------------------------------------
// 가드 라벨·감지 명령 이름만 추출, 원문은 보관하지 않음
pub fn parse_denial_text(text: &str, fallback_tool: &str) -> Option<DenialMark> {
    // 1. 접두 분기
    let head = text.trim_start();
    let (route, rest) = match head.strip_prefix("PreToolUse:") {
        Some(after) => {
            let tool = after.split_whitespace().next().unwrap_or("");
            let marker_at = after.find("hook error:")?;
            let rest = &after[marker_at + "hook error:".len()..];
            if tool.is_empty()
                || tool.len() > LABEL_LIMIT
                || !tool
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':'))
            {
                return None;
            }
            (format!("PreToolUse:{tool}"), rest)
        }
        None => (format!("PreToolUse:{fallback_tool}"), head),
    };

    // 2. 가드 라벨
    let labeled = rest.trim_start().strip_prefix('[')?;
    let guard = labeled.split(']').next().unwrap_or("");
    if guard.is_empty()
        || guard.len() > LABEL_LIMIT
        || !guard
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return None;
    }
    if !head.starts_with("PreToolUse:") && !guard.ends_with("-guard") {
        return None;
    }

    // 3. 감지 명령 이름
    let mut command = String::new();
    let found = DENIAL_NAME_MARKERS
        .iter()
        .find_map(|marker| rest.find(marker).map(|at| &rest[at + marker.len()..]));
    if let Some(after) = found {
        let name = after.split(')').next().unwrap_or("");
        if !name.is_empty()
            && name.len() <= LABEL_LIMIT
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        {
            command = name.to_string();
        }
    }

    Some(DenialMark {
        route,
        guard: guard.to_string(),
        command,
    })
}
