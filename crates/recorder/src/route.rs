//! Turn a received WebSocket frame into one raw line (task 002, items 5 and 6).
//!
//! Nothing is dropped here:
//! - text frames with `messages` -> seq_first/seq_last from the messages;
//! - valid JSON text without `messages` (e.g. only
//!   `confirmedSequenceNumberMessage`) -> stored verbatim with
//!   `seq_first = seq_last = 0`;
//! - text that is not valid JSON (task 008, remark З3 of the 002 audit),
//!   non-text frames (binary, ping, pong, close) and text with invalid UTF-8
//!   -> a recorder-made JSON wrapper with the payload in base64, also seq 0:
//!   `{"recorderFrame":{"opcode":"ping","payloadBase64":"..."}}`
//!   (`opcode` is `text` for unparseable text). The payload bytes survive
//!   exactly, including any CR/LF/TAB that would clash with the line format.
//!   The key `recorderFrame` never occurs in the feed, so a parser can tell
//!   the two apart; the line format itself is unchanged.
//!
//! Filtering is the job of the decode stage.

use base64::Engine;
use hood_core::{detect_gap, FeedEnvelope, Gap};

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
    /// Newest `header.timestamp` (unix s) among kind-3 (sequencer) messages,
    /// if any. Only used for the backlog statistics of task 009; delayed
    /// messages (kind 9/13) carry an L1 inbox time hundreds of seconds old.
    pub kind3_ts: Option<u64>,
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
            kind3_ts: None,
            raw,
        }
    }
}

/// Newest kind-3 `header.timestamp` in an envelope value
/// (`messages[].message.message.header.{kind,timestamp}`).
fn newest_kind3_ts(v: &serde_json::Value) -> Option<u64> {
    v.get("messages")?
        .as_array()?
        .iter()
        .filter_map(|m| {
            let h = m.get("message")?.get("message")?.get("header")?;
            (h.get("kind")?.as_u64()? == 3).then(|| h.get("timestamp")?.as_u64())?
        })
        .max()
}

/// Sequence numbers of one envelope, as the writer accounts for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvSeqs {
    /// First message's seq.
    pub seq_first: u64,
    /// Last message's seq.
    pub seq_last: u64,
    /// Highest seq (== `seq_last` unless the envelope is out of order).
    pub seq_max: u64,
    /// Holes between consecutive messages.
    pub intra_gaps: Vec<Gap>,
    /// Consecutive messages that went backwards or repeated.
    pub intra_disorder: u32,
}

/// The one place that derives `seq_max` and the holes from an envelope's
/// messages (task 025 item 1, remark Р3 of the 021 review): used for live
/// frames ([`route_text`]) and for raw lines read back from disk
/// (`rawline::line_seqs`). None for an envelope without messages.
pub fn envelope_seqs(env: &FeedEnvelope) -> Option<EnvSeqs> {
    let (seq_first, seq_last) = env.seq_range()?;
    let seqs: Vec<u64> = env.messages.iter().map(|m| m.sequence_number).collect();
    let (intra_gaps, intra_disorder) = intra_envelope_gaps(&seqs);
    let seq_max = seqs.iter().copied().max().unwrap_or(seq_last);
    Some(EnvSeqs { seq_first, seq_last, seq_max, intra_gaps, intra_disorder })
}

/// Holes and disorder inside one envelope. Every pair `(a, b)` of consecutive
/// sequence numbers must satisfy `b == a + 1`.
fn intra_envelope_gaps(seqs: &[u64]) -> (Vec<Gap>, u32) {
    let mut gaps = Vec::new();
    let mut disorder = 0u32;
    for w in seqs.windows(2) {
        let (a, b) = (w[0], w[1]);
        if let Some(g) = detect_gap(Some(a), b) {
            gaps.push(g);
        } else if b <= a {
            disorder += 1;
        }
    }
    (gaps, disorder)
}

/// A text frame. Valid JSON is stored as is; anything else goes into a
/// base64 `recorderFrame` with `opcode: "text"`.
pub fn route_text(recv_ns: u128, raw: String) -> Line {
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return route_opaque(recv_ns, "text", raw.as_bytes()),
    };
    let kind3_ts = newest_kind3_ts(&value);
    // Valid JSON, but not an envelope we understand (e.g. `messages` of an
    // unexpected shape): keep verbatim, unsequenced.
    let env: FeedEnvelope = match serde_json::from_value(value) {
        Ok(e) => e,
        Err(_) => return Line::unsequenced(recv_ns, raw),
    };
    let Some(EnvSeqs { seq_first, seq_last, seq_max, intra_gaps, intra_disorder }) = envelope_seqs(&env) else {
        return Line::unsequenced(recv_ns, raw);
    };
    Line { recv_ns, seq_first, seq_last, seq_max, intra_gaps, intra_disorder, kind3_ts, raw }
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
        assert_eq!((l.seq_first, l.seq_last, l.seq_max), (74755960, 74755962, 74755962));
        assert!(l.intra_gaps.is_empty());
        assert_eq!(l.intra_disorder, 0);
    }

    #[test]
    fn kind3_timestamp_is_extracted() {
        // msg() is kind 3 with timestamp 1790594344.
        let l = route_text(1, envelope(&[5, 6]));
        assert_eq!(l.kind3_ts, Some(1_790_594_344));
        // A delayed message (kind 13) alone gives no timestamp.
        let delayed = envelope(&[7]).replace("\"kind\":3", "\"kind\":13");
        assert_eq!(route_text(1, delayed).kind3_ts, None);
        assert_eq!(route_opaque(1, "ping", b"").kind3_ts, None);
    }

    #[test]
    fn gap_inside_envelope_is_reported() {
        let l = route_text(1, envelope(&[100, 101, 105, 106, 108]));
        assert_eq!((l.seq_first, l.seq_last), (100, 108));
        assert_eq!(l.intra_gaps, vec![Gap { from: 102, to: 104 }, Gap { from: 107, to: 107 }]);
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
    fn envelope_seqs_derivation() {
        let env: FeedEnvelope = serde_json::from_str(&envelope(&[10, 12, 11])).unwrap();
        let want = EnvSeqs {
            seq_first: 10,
            seq_last: 11,
            seq_max: 12,
            intra_gaps: vec![Gap { from: 11, to: 11 }],
            intra_disorder: 1,
        };
        assert_eq!(envelope_seqs(&env), Some(want));
        let empty: FeedEnvelope = serde_json::from_str(r#"{"version":1,"messages":[]}"#).unwrap();
        assert_eq!(envelope_seqs(&empty), None);
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
    fn unparseable_text_is_base64_wrapped() {
        use base64::Engine;
        let text = "not json\r\nsecond\tline";
        let l = route_text(7, text.to_string());
        assert_eq!((l.recv_ns, l.seq_first, l.seq_last), (7, 0, 0));
        assert!(!l.has_seq());
        let v: serde_json::Value = serde_json::from_str(&l.raw).unwrap();
        assert_eq!(v["recorderFrame"]["opcode"], "text");
        let b64 = v["recorderFrame"]["payloadBase64"].as_str().unwrap();
        let back = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        assert_eq!(back, text.as_bytes()); // byte-exact, CR/LF/TAB included
        assert!(!l.raw.contains(['\n', '\r', '\t']));
        // Truncated envelope (not valid JSON) is wrapped, too.
        let cut = &envelope(&[5])[..40];
        let l = route_text(8, cut.to_string());
        assert!(l.raw.starts_with(r#"{"recorderFrame":{"opcode":"text""#));
    }

    #[test]
    fn valid_json_of_unknown_shape_is_kept_verbatim() {
        for raw in [r#"[1,2]"#, r#""s""#, r#"{"messages":"x"}"#, r#"{"a":1}"#] {
            let l = route_text(7, raw.to_string());
            assert_eq!((l.seq_first, l.seq_last, l.raw.as_str()), (0, 0, raw));
        }
    }

    #[test]
    fn opaque_frames_are_base64_wrapped() {
        let l = route_opaque(9, "binary", &[0, 255, 10, 9]);
        assert_eq!((l.seq_first, l.seq_last), (0, 0));
        assert_eq!(l.raw, r#"{"recorderFrame":{"opcode":"binary","payloadBase64":"AP8KCQ=="}}"#);
        // The wrapper is valid JSON and parses as an envelope without messages.
        let env: FeedEnvelope = serde_json::from_str(&l.raw).unwrap();
        assert!(env.seq_range().is_none());
        assert!(!l.raw.contains(['\n', '\t']));
    }
}
