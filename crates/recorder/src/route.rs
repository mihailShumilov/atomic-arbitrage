//! Turn a received WebSocket frame into one raw line (task 002, items 5 and 6).
//!
//! Nothing is dropped here:
//! - text frames with `messages` -> seq_first/seq_last from the messages;
//! - text frames without `messages` (e.g. only `confirmedSequenceNumber`) and
//!   unparseable text -> stored verbatim with `seq_first = seq_last = 0`;
//! - non-text frames (binary, ping, pong, close) and text with invalid UTF-8
//!   -> a recorder-made JSON wrapper with the payload in base64, also seq 0:
//!   `{"recorderFrame":{"opcode":"ping","payloadBase64":"..."}}`.
//!   The key `recorderFrame` never occurs in the feed, so a parser can tell
//!   the two apart; the line format itself is unchanged.
//!
//! Filtering is the job of the decode stage.

use base64::Engine;
use hood_core::{FeedEnvelope, Gap};

/// One recorded line, handed from the network task to the writer thread.
#[derive(Debug, Clone)]
pub struct Line {
    pub recv_ns: u128,
    /// 0 for lines that carry no sequence numbers.
    pub seq_first: u64,
    pub seq_last: u64,
    /// Highest sequence number in the envelope (== seq_last unless the
    /// envelope is out of order internally). 0 if none.
    pub seq_max: u64,
    /// Holes between consecutive messages of the same envelope.
    pub intra_gaps: Vec<Gap>,
    /// Consecutive messages in the envelope that went backwards or repeated.
    pub intra_disorder: u32,
    pub raw: String,
}

impl Line {
    pub fn has_seq(&self) -> bool {
        self.seq_max > 0
    }

    fn unsequenced(recv_ns: u128, raw: String) -> Self {
        Line {
            recv_ns,
            seq_first: 0,
            seq_last: 0,
            seq_max: 0,
            intra_gaps: Vec::new(),
            intra_disorder: 0,
            raw,
        }
    }
}

/// Holes and disorder inside one envelope. Every pair `(a, b)` of consecutive
/// sequence numbers must satisfy `b == a + 1`.
pub fn intra_envelope_gaps(seqs: &[u64]) -> (Vec<Gap>, u32) {
    let mut gaps = Vec::new();
    let mut disorder = 0u32;
    for w in seqs.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b > a + 1 {
            gaps.push(Gap {
                from: a + 1,
                to: b - 1,
            });
        } else if b <= a {
            disorder += 1;
        }
    }
    (gaps, disorder)
}

/// A text frame. `raw` is stored as is.
pub fn route_text(recv_ns: u128, raw: String) -> Line {
    let env: FeedEnvelope = match serde_json::from_str(&raw) {
        Ok(e) => e,
        Err(_) => return Line::unsequenced(recv_ns, raw),
    };
    let Some((seq_first, seq_last)) = env.seq_range() else {
        return Line::unsequenced(recv_ns, raw);
    };
    let seqs: Vec<u64> = env.messages.iter().map(|m| m.sequence_number).collect();
    let (intra_gaps, intra_disorder) = intra_envelope_gaps(&seqs);
    let seq_max = seqs.iter().copied().max().unwrap_or(seq_last);
    Line {
        recv_ns,
        seq_first,
        seq_last,
        seq_max,
        intra_gaps,
        intra_disorder,
        raw,
    }
}

/// A frame that is not valid UTF-8 text: binary, control frames, or a text
/// frame with broken UTF-8 (`opcode = "text_invalid_utf8"`).
pub fn route_opaque(recv_ns: u128, opcode: &str, payload: &[u8]) -> Line {
    let b64 = base64::engine::general_purpose::STANDARD.encode(payload);
    let raw = serde_json::json!({
        "recorderFrame": { "opcode": opcode, "payloadBase64": b64 }
    })
    .to_string();
    Line::unsequenced(recv_ns, raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shape of a real envelope captured on 2026-09-28 (block 74755960,
    // l2Msg/signature shortened), see hood-core tests.
    fn msg(seq: u64) -> String {
        format!(
            r#"{{"sequenceNumber":{seq},"message":{{"message":{{"header":{{"kind":3,"sender":"0xa4b000000000000000000073657175656e636572","blockNumber":26075606,"timestamp":1790594344,"requestId":null,"baseFeeL1":0}},"l2Msg":"AAAA"}},"delayedMessagesRead":328658}},"blockHash":"0x529d8dcb881a6f5ed00232db376cf449ad881aa2a7ac4f8eb0078ea40a17f0f4","signatureV2":"AA==","blockMetadata":null}}"#
        )
    }

    fn envelope(seqs: &[u64]) -> String {
        let m: Vec<String> = seqs.iter().map(|&s| msg(s)).collect();
        format!(r#"{{"version":1,"messages":[{}]}}"#, m.join(","))
    }

    #[test]
    fn contiguous_envelope_has_no_gaps() {
        let l = route_text(1, envelope(&[74755960, 74755961, 74755962]));
        assert_eq!(
            (l.seq_first, l.seq_last, l.seq_max),
            (74755960, 74755962, 74755962)
        );
        assert!(l.intra_gaps.is_empty());
        assert_eq!(l.intra_disorder, 0);
    }

    #[test]
    fn gap_inside_envelope_is_reported() {
        let l = route_text(1, envelope(&[100, 101, 105, 106, 108]));
        assert_eq!((l.seq_first, l.seq_last), (100, 108));
        assert_eq!(
            l.intra_gaps,
            vec![Gap { from: 102, to: 104 }, Gap { from: 107, to: 107 }]
        );
        assert_eq!(l.intra_disorder, 0);
    }

    #[test]
    fn disorder_inside_envelope_is_counted_not_a_gap() {
        let (g, d) = intra_envelope_gaps(&[10, 11, 11, 9, 10]);
        assert!(g.is_empty());
        assert_eq!(d, 2);
        let l = route_text(1, envelope(&[10, 12, 11]));
        assert_eq!(l.seq_max, 12);
        assert_eq!(l.seq_last, 11);
    }

    #[test]
    fn envelope_without_messages_is_kept_verbatim() {
        // confirmedSequenceNumber-only envelope. Shape taken from the Arbitrum
        // broadcaster source (BroadcastMessage), not yet observed on this feed.
        let raw = r#"{"version":1,"confirmedSequenceNumberMessage":{"sequenceNumber":74755900}}"#;
        let l = route_text(7, raw.to_string());
        assert_eq!((l.recv_ns, l.seq_first, l.seq_last), (7, 0, 0));
        assert!(!l.has_seq());
        assert_eq!(l.raw, raw);
        let l = route_text(7, r#"{"version":1,"messages":[]}"#.to_string());
        assert!(!l.has_seq());
    }

    #[test]
    fn unparseable_text_is_kept_verbatim() {
        let l = route_text(7, "not json".to_string());
        assert_eq!(
            (l.seq_first, l.seq_last, l.raw.as_str()),
            (0, 0, "not json")
        );
    }

    #[test]
    fn opaque_frames_are_base64_wrapped() {
        let l = route_opaque(9, "binary", &[0, 255, 10, 9]);
        assert_eq!((l.seq_first, l.seq_last), (0, 0));
        assert_eq!(
            l.raw,
            r#"{"recorderFrame":{"opcode":"binary","payloadBase64":"AP8KCQ=="}}"#
        );
        // The wrapper is valid JSON and parses as an envelope without messages.
        let env: FeedEnvelope = serde_json::from_str(&l.raw).unwrap();
        assert!(env.seq_range().is_none());
        assert!(!l.raw.contains(['\n', '\t']));
    }
}
