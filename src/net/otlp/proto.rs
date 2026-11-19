//! net/otlp/proto.rs
//! OTLP protobuf 와이어 해석 (로그·지표 요청을 OTLP/JSON 형태 Value 로 변환, 외부 crate 없음).

use anyhow::{Context, bail};
use serde_json::{Map, Number, Value};

const MAX_ANY_DEPTH: usize = 32;
const MAX_NODES: usize = 2_000_000;
const MAX_FIELD_NUMBER: u64 = 0x1FFF_FFFF;
const WIRE_VARINT: u8 = 0;
const WIRE_FIXED64: u8 = 1;
const WIRE_LEN: u8 = 2;
const WIRE_FIXED32: u8 = 5;
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
const BASE64_DIGITS: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// 1. 스키마 형태 --------------------------------------------------------------
type FieldDef = (u32, &'static str, Kind);
type Schema = [FieldDef];

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Hex,
    Bool,
    UInt32,
    Int32,
    Fixed32,
    Fixed64,
    SFixed64,
    Double,
    Message(&'static Schema),
    Messages(&'static Schema),
    Any,
    Anys,
    Empty,
}

impl Kind {
    fn wire_type(self) -> u8 {
        match self {
            Kind::Bool | Kind::UInt32 | Kind::Int32 => WIRE_VARINT,
            Kind::Fixed64 | Kind::SFixed64 | Kind::Double => WIRE_FIXED64,
            Kind::Fixed32 => WIRE_FIXED32,
            _ => WIRE_LEN,
        }
    }
}

// 2. 공통 메시지 스키마 -------------------------------------------------------
// opentelemetry-proto common/v1, resource/v1 필드 번호 기준
const KEY_VALUE: &Schema = &[(1, "key", Kind::Text), (2, "value", Kind::Any)];
const ARRAY_VALUE: &Schema = &[(1, "values", Kind::Anys)];
const KVLIST_VALUE: &Schema = &[(1, "values", Kind::Messages(KEY_VALUE))];
const RESOURCE: &Schema = &[
    (1, "attributes", Kind::Messages(KEY_VALUE)),
    (2, "droppedAttributesCount", Kind::UInt32),
];
const SCOPE: &Schema = &[
    (1, "name", Kind::Text),
    (2, "version", Kind::Text),
    (3, "attributes", Kind::Messages(KEY_VALUE)),
    (4, "droppedAttributesCount", Kind::UInt32),
];

// 3. 로그 메시지 스키마 -------------------------------------------------------
// opentelemetry-proto logs/v1, collector/logs/v1 필드 번호 기준
const LOG_RECORD: &Schema = &[
    (1, "timeUnixNano", Kind::Fixed64),
    (11, "observedTimeUnixNano", Kind::Fixed64),
    (2, "severityNumber", Kind::Int32),
    (3, "severityText", Kind::Text),
    (5, "body", Kind::Any),
    (6, "attributes", Kind::Messages(KEY_VALUE)),
    (7, "droppedAttributesCount", Kind::UInt32),
    (8, "flags", Kind::Fixed32),
    (9, "traceId", Kind::Hex),
    (10, "spanId", Kind::Hex),
    (12, "eventName", Kind::Text),
];
const SCOPE_LOGS: &Schema = &[
    (1, "scope", Kind::Message(SCOPE)),
    (2, "logRecords", Kind::Messages(LOG_RECORD)),
    (3, "schemaUrl", Kind::Text),
];
const RESOURCE_LOGS: &Schema = &[
    (1, "resource", Kind::Message(RESOURCE)),
    (2, "scopeLogs", Kind::Messages(SCOPE_LOGS)),
    (3, "schemaUrl", Kind::Text),
];
const EXPORT_LOGS_REQUEST: &Schema = &[(1, "resourceLogs", Kind::Messages(RESOURCE_LOGS))];

// 4. 지표 메시지 스키마 -------------------------------------------------------
// opentelemetry-proto metrics/v1, collector/metrics/v1 필드 번호 기준 (히스토그램류는 빈 객체)
const NUMBER_DATA_POINT: &Schema = &[
    (7, "attributes", Kind::Messages(KEY_VALUE)),
    (2, "startTimeUnixNano", Kind::Fixed64),
    (3, "timeUnixNano", Kind::Fixed64),
    (4, "asDouble", Kind::Double),
    (6, "asInt", Kind::SFixed64),
    (8, "flags", Kind::UInt32),
];
const GAUGE: &Schema = &[(1, "dataPoints", Kind::Messages(NUMBER_DATA_POINT))];
const SUM: &Schema = &[
    (1, "dataPoints", Kind::Messages(NUMBER_DATA_POINT)),
    (2, "aggregationTemporality", Kind::Int32),
    (3, "isMonotonic", Kind::Bool),
];
const METRIC: &Schema = &[
    (1, "name", Kind::Text),
    (2, "description", Kind::Text),
    (3, "unit", Kind::Text),
    (5, "gauge", Kind::Message(GAUGE)),
    (7, "sum", Kind::Message(SUM)),
    (9, "histogram", Kind::Empty),
    (10, "exponentialHistogram", Kind::Empty),
    (11, "summary", Kind::Empty),
];
const SCOPE_METRICS: &Schema = &[
    (1, "scope", Kind::Message(SCOPE)),
    (2, "metrics", Kind::Messages(METRIC)),
    (3, "schemaUrl", Kind::Text),
];
const RESOURCE_METRICS: &Schema = &[
    (1, "resource", Kind::Message(RESOURCE)),
    (2, "scopeMetrics", Kind::Messages(SCOPE_METRICS)),
    (3, "schemaUrl", Kind::Text),
];
const EXPORT_METRICS_REQUEST: &Schema = &[(1, "resourceMetrics", Kind::Messages(RESOURCE_METRICS))];

// 5. 공개 진입점 --------------------------------------------------------------
/// ExportLogsServiceRequest 바이트를 OTLP/JSON 형태 Value 로 변환.
pub fn decode_logs_request(payload: &[u8]) -> anyhow::Result<Value> {
    let mut decoder = WireDecoder {
        nodes_left: MAX_NODES,
    };
    decoder
        .decode_message(payload, EXPORT_LOGS_REQUEST, 0)
        .context("OTLP 로그 protobuf 형식 오류")
}

/// ExportMetricsServiceRequest 바이트를 OTLP/JSON 형태 Value 로 변환.
pub(crate) fn decode_metrics_request(payload: &[u8]) -> anyhow::Result<Value> {
    let mut decoder = WireDecoder {
        nodes_left: MAX_NODES,
    };
    decoder
        .decode_message(payload, EXPORT_METRICS_REQUEST, 0)
        .context("OTLP 지표 protobuf 형식 오류")
}

// 6. 바이트 읽기 --------------------------------------------------------------
struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> anyhow::Result<&'a [u8]> {
        if len > self.buf.len() {
            bail!("선언 길이가 남은 버퍼 초과");
        }
        let (head, tail) = self.buf.split_at(len);
        self.buf = tail;
        Ok(head)
    }

    fn take_array<const N: usize>(&mut self) -> anyhow::Result<[u8; N]> {
        let mut array = [0u8; N];
        array.copy_from_slice(self.take(N)?);
        Ok(array)
    }

    fn read_varint(&mut self) -> anyhow::Result<u64> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let Some((&byte, rest)) = self.buf.split_first() else {
                bail!("varint 중간에 버퍼 종료");
            };
            self.buf = rest;
            if shift == 63 && byte > 1 {
                bail!("varint 64비트 초과");
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        bail!("varint 길이 초과")
    }

    fn read_tag(&mut self) -> anyhow::Result<(u32, u8)> {
        let tag = self.read_varint()?;
        let number = (tag >> 3)
            .try_into()
            .ok()
            .filter(|number: &u32| *number != 0 && u64::from(*number) <= MAX_FIELD_NUMBER)
            .context("필드 번호 범위 오류")?;
        Ok((number, (tag & 7) as u8))
    }

    fn read_len_delimited(&mut self) -> anyhow::Result<&'a [u8]> {
        let len = usize::try_from(self.read_varint()?).context("길이 값 범위 초과")?;
        self.take(len)
    }

    fn skip_field(&mut self, wire: u8) -> anyhow::Result<()> {
        match wire {
            WIRE_VARINT => {
                self.read_varint()?;
            }
            WIRE_FIXED64 => {
                self.take(8)?;
            }
            WIRE_LEN => {
                self.read_len_delimited()?;
            }
            WIRE_FIXED32 => {
                self.take(4)?;
            }
            _ => bail!("지원하지 않는 와이어 타입 {wire}"),
        }
        Ok(())
    }
}

// 7. 메시지 해석 --------------------------------------------------------------
struct WireDecoder {
    nodes_left: usize,
}

impl WireDecoder {
    fn decode_message(
        &mut self,
        buf: &[u8],
        schema: &Schema,
        depth: usize,
    ) -> anyhow::Result<Value> {
        // 1. 노드 예산 차감
        self.nodes_left = self
            .nodes_left
            .checked_sub(1)
            .context("해석 노드 수 상한 초과")?;

        // 2. 필드 순회 (미지 필드는 와이어 타입대로 건너뜀)
        let mut reader = Reader { buf };
        let mut object = Map::new();
        while !reader.buf.is_empty() {
            let (number, wire) = reader.read_tag()?;
            let def = schema
                .iter()
                .find(|(field, _, kind)| *field == number && kind.wire_type() == wire);
            let Some(&(_, name, kind)) = def else {
                reader.skip_field(wire)?;
                continue;
            };
            let value = self.decode_field(&mut reader, kind, depth)?;
            if matches!(kind, Kind::Messages(_) | Kind::Anys) {
                if let Value::Array(items) = object
                    .entry(name)
                    .or_insert_with(|| Value::Array(Vec::new()))
                {
                    items.push(value);
                }
            } else {
                object.insert(name.to_owned(), value);
            }
        }
        Ok(Value::Object(object))
    }

    fn decode_field(
        &mut self,
        reader: &mut Reader,
        kind: Kind,
        depth: usize,
    ) -> anyhow::Result<Value> {
        let value = match kind {
            Kind::Text => {
                let text = std::str::from_utf8(reader.read_len_delimited()?)
                    .context("문자열 UTF-8 오류")?;
                Value::String(text.to_owned())
            }
            Kind::Hex => {
                let bytes = reader.read_len_delimited()?;
                let mut hex = String::with_capacity(bytes.len() * 2);
                for byte in bytes {
                    hex.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
                    hex.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
                }
                Value::String(hex)
            }
            Kind::Bool => Value::Bool(reader.read_varint()? != 0),
            Kind::UInt32 => Value::from(reader.read_varint()? as u32),
            Kind::Int32 => Value::from(reader.read_varint()? as i32),
            Kind::Fixed32 => Value::from(u32::from_le_bytes(reader.take_array()?)),
            Kind::Fixed64 => Value::String(u64::from_le_bytes(reader.take_array()?).to_string()),
            Kind::SFixed64 => Value::String(i64::from_le_bytes(reader.take_array()?).to_string()),
            Kind::Double => convert_double(f64::from_le_bytes(reader.take_array()?)),
            Kind::Message(schema) | Kind::Messages(schema) => {
                self.decode_message(reader.read_len_delimited()?, schema, depth)?
            }
            Kind::Any | Kind::Anys => self.decode_any(reader.read_len_delimited()?, depth + 1)?,
            Kind::Empty => {
                reader.read_len_delimited()?;
                Value::Object(Map::new())
            }
        };
        Ok(value)
    }

    fn decode_any(&mut self, buf: &[u8], depth: usize) -> anyhow::Result<Value> {
        // 1. 깊이·노드 예산 검사
        if depth > MAX_ANY_DEPTH {
            bail!("AnyValue 중첩 깊이 {MAX_ANY_DEPTH} 초과");
        }
        self.nodes_left = self
            .nodes_left
            .checked_sub(1)
            .context("해석 노드 수 상한 초과")?;

        // 2. oneof 필드 순회 (마지막 값 우선)
        let mut reader = Reader { buf };
        let mut object = Map::new();
        while !reader.buf.is_empty() {
            let (number, wire) = reader.read_tag()?;
            let (name, value) = match (number, wire) {
                (1, WIRE_LEN) => {
                    let text = std::str::from_utf8(reader.read_len_delimited()?)
                        .context("문자열 UTF-8 오류")?;
                    ("stringValue", Value::String(text.to_owned()))
                }
                (2, WIRE_VARINT) => ("boolValue", Value::Bool(reader.read_varint()? != 0)),
                (3, WIRE_VARINT) => (
                    "intValue",
                    Value::String((reader.read_varint()? as i64).to_string()),
                ),
                (4, WIRE_FIXED64) => (
                    "doubleValue",
                    convert_double(f64::from_le_bytes(reader.take_array()?)),
                ),
                (5, WIRE_LEN) => (
                    "arrayValue",
                    self.decode_message(reader.read_len_delimited()?, ARRAY_VALUE, depth)?,
                ),
                (6, WIRE_LEN) => (
                    "kvlistValue",
                    self.decode_message(reader.read_len_delimited()?, KVLIST_VALUE, depth)?,
                ),
                (7, WIRE_LEN) => (
                    "bytesValue",
                    Value::String(encode_base64(reader.read_len_delimited()?)),
                ),
                _ => {
                    reader.skip_field(wire)?;
                    continue;
                }
            };
            object.clear();
            object.insert(name.to_owned(), value);
        }
        Ok(Value::Object(object))
    }
}

// 8. 값 변환 ------------------------------------------------------------------
// proto3 JSON 규칙: 유한수는 숫자, NaN·무한대는 문자열
fn convert_double(value: f64) -> Value {
    if let Some(number) = Number::from_f64(value) {
        return Value::Number(number);
    }
    let text = if value.is_nan() {
        "NaN"
    } else if value > 0.0 {
        "Infinity"
    } else {
        "-Infinity"
    };
    Value::String(text.to_owned())
}

// 표준 알파벳·패딩 base64 (bytesValue 용)
fn encode_base64(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = u32::from(chunk.get(1).copied().unwrap_or(0));
        let third = u32::from(chunk.get(2).copied().unwrap_or(0));
        let group = (first << 16) | (second << 8) | third;
        encoded.push(char::from(BASE64_DIGITS[(group >> 18) as usize & 63]));
        encoded.push(char::from(BASE64_DIGITS[(group >> 12) as usize & 63]));
        if chunk.len() > 1 {
            encoded.push(char::from(BASE64_DIGITS[(group >> 6) as usize & 63]));
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(char::from(BASE64_DIGITS[group as usize & 63]));
        } else {
            encoded.push('=');
        }
    }
    encoded
}
