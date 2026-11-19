//! net/gateway/decode.rs
//! 압축 응답 계측용 증분 해제: 계측 사본만 해제하고 클라이언트 바이트는 건드리지 않음 (gzip, deflate 만 지원).

use flate2::write::{GzDecoder, ZlibDecoder};
use std::io::Write;

// 1. 해제기 -------------------------------------------------------------------
enum Codec {
    Gzip(GzDecoder<Vec<u8>>),
    Deflate(ZlibDecoder<Vec<u8>>),
}

// 해제 실패 이후에는 빈 조각만 돌려줌 (이미 읽은 사용량은 유지)
pub struct ChunkDecoder {
    codec: Codec,
    failed: bool,
}

// 2. 인코딩 판정 --------------------------------------------------------------
pub enum EncodingPlan {
    Plain,
    Decode(Box<ChunkDecoder>),
    Unsupported,
}

/// Content-Encoding 값으로 해제 방식 결정 (단일 gzip·deflate 만 해제, br·zstd·다중 인코딩은 미계측).
pub fn plan_decoding(content_encoding: &str) -> EncodingPlan {
    match content_encoding.trim().to_ascii_lowercase().as_str() {
        "" | "identity" => EncodingPlan::Plain,
        "gzip" | "x-gzip" => EncodingPlan::Decode(Box::new(ChunkDecoder {
            codec: Codec::Gzip(GzDecoder::new(Vec::new())),
            failed: false,
        })),
        // HTTP deflate 는 zlib 래퍼 형식
        "deflate" => EncodingPlan::Decode(Box::new(ChunkDecoder {
            codec: Codec::Deflate(ZlibDecoder::new(Vec::new())),
            failed: false,
        })),
        _ => EncodingPlan::Unsupported,
    }
}

impl ChunkDecoder {
    /// 압축 조각 1개를 넣고 그 시점까지 풀린 바이트 반환.
    pub fn decode_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
        if self.failed {
            return Vec::new();
        }
        let (written, sink) = match &mut self.codec {
            Codec::Gzip(decoder) => (decoder.write_all(chunk), decoder.get_mut()),
            Codec::Deflate(decoder) => (decoder.write_all(chunk), decoder.get_mut()),
        };
        if written.is_err() {
            self.failed = true;
        }
        std::mem::take(sink)
    }
}
