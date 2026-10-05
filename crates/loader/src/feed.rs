//! `feed_recv_ns` for `hood.blocks` from a copy of the raw feed (read-only).
//!
//! Input: recorder out-dirs (`YYYY/MM/DD/feed-*.tsv.zst`; `_torn/` is skipped, it is not raw data
//! for loading). Each hourly file is decoded whole first ([`crate::zst`]). Rule for a block (seq)
//! seen more than once (`references/data-model.md`: repeats are possible, the loader decides):
//! **the earliest `recv_ns` wins**, i.e. the first time the block reached us. A repeat whose
//! `blockHash` differs from an earlier one is a data error and stops the load. Every message of
//! an envelope gets the envelope's `recv_ns`.
//!
//! Lines with `seq_first = seq_last = 0` (pings, confirmations, text frames) are skipped. A
//! sequenced line that does not parse is an error: the recorder writes only valid lines.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use hood_core::feedline::parse_raw_line;
use hood_core::{FeedEnvelope, Range};

/// What the feed says about one block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedSeen {
    /// Earliest receive time, unix ns.
    pub recv_ns: u64,
    /// `blockHash` of the message (lowercase), if the feed sent it.
    pub block_hash: Option<String>,
}

/// Counters of [`FeedIndex`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedStats {
    pub files: usize,
    /// Files without a content checksum in some frame (written before task 002/026).
    pub files_unchecked: usize,
    pub lines: u64,
    pub sequenced_lines: u64,
    /// Messages kept (inside the wanted ranges).
    pub messages: u64,
    /// Messages outside the wanted ranges (not indexed).
    pub messages_out_of_range: u64,
    /// Messages of a block already indexed (earliest kept).
    pub repeats: u64,
}

/// seq -> [`FeedSeen`].
#[derive(Debug, Default)]
pub struct FeedIndex {
    seen: HashMap<u64, FeedSeen>,
    pub stats: FeedStats,
}

impl FeedIndex {
    /// Indexes every hourly file under each of `dirs`. `wanted` limits the index to these block
    /// ranges (`None` = everything), so a long feed does not have to fit in memory.
    ///
    /// # Errors
    /// Unreadable directory or file, broken zstd, a broken sequenced line, a `blockHash` conflict.
    pub fn from_dirs(dirs: &[PathBuf], wanted: Option<&[Range]>) -> Result<Self> {
        let mut idx = Self::default();
        for dir in dirs {
            for path in feed_files(dir)? {
                let z = crate::zst::read_zst(&path)?;
                idx.stats.files += 1;
                if !z.fully_checksummed() && !z.data.is_empty() {
                    idx.stats.files_unchecked += 1;
                }
                idx.add_text(&path.display().to_string(), &z.data, wanted)?;
            }
        }
        Ok(idx)
    }

    /// Indexes the decoded text of one hourly file.
    ///
    /// # Errors
    /// A broken sequenced line, `recv_ns` over `u64`, a `blockHash` conflict.
    pub(crate) fn add_text(&mut self, name: &str, text: &[u8], wanted: Option<&[Range]>) -> Result<()> {
        for (i, line) in text.split(|&b| b == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            self.stats.lines += 1;
            let at = || format!("{name}:{}", i + 1);
            let l = parse_raw_line(line).ok_or_else(|| anyhow!("{}: not a raw feed line", at()))?;
            if l.is_unsequenced() {
                continue;
            }
            self.stats.sequenced_lines += 1;
            let recv_ns = u64::try_from(l.recv_ns).map_err(|_| anyhow!("{}: recv_ns {} over u64", at(), l.recv_ns))?;
            let env: FeedEnvelope = serde_json::from_slice(l.json).with_context(|| format!("{}: envelope", at()))?;
            if env.messages.is_empty() {
                bail!("{}: seq columns {}..{} but no messages", at(), l.seq_first, l.seq_last);
            }
            for m in env.messages {
                let seq = m.sequence_number;
                if wanted.is_some_and(|w| !w.iter().any(|r| (r.from..=r.to).contains(&seq))) {
                    self.stats.messages_out_of_range += 1;
                    continue;
                }
                self.stats.messages += 1;
                let hash = m.block_hash.map(|h| h.to_ascii_lowercase());
                match self.seen.get_mut(&seq) {
                    None => {
                        self.seen.insert(seq, FeedSeen { recv_ns, block_hash: hash });
                    }
                    Some(prev) => {
                        self.stats.repeats += 1;
                        match (&prev.block_hash, &hash) {
                            (Some(a), Some(b)) if a != b => {
                                bail!("{}: block {seq} repeated with blockHash {b}, earlier {a}", at())
                            }
                            (None, Some(_)) => prev.block_hash = hash,
                            _ => {}
                        }
                        prev.recv_ns = prev.recv_ns.min(recv_ns);
                    }
                }
            }
        }
        Ok(())
    }

    /// What the feed says about block `n`.
    pub fn get(&self, n: u64) -> Option<&FeedSeen> {
        self.seen.get(&n)
    }

    /// Number of indexed blocks.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// No block indexed.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// Hourly files `feed-*.tsv.zst` under `dir`, recursively, sorted; `_torn/` is skipped.
///
/// # Errors
/// Unreadable directory.
pub(crate) fn feed_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).with_context(|| format!("read dir {}", d.display()))? {
            let e = e.with_context(|| format!("read dir {}", d.display()))?;
            let path = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if e.file_type()?.is_dir() {
                if name != "_torn" {
                    stack.push(path);
                }
            } else if name.starts_with("feed-") && name.ends_with(".tsv.zst") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(recv: u64, seqs: &[(u64, &str)]) -> String {
        let m: Vec<String> =
            seqs.iter().map(|(s, h)| format!(r#"{{"sequenceNumber":{s},"blockHash":"{h}"}}"#)).collect();
        let (a, b) = (seqs[0].0, seqs[seqs.len() - 1].0);
        format!("{recv}\t{a}\t{b}\t{{\"version\":1,\"messages\":[{}]}}\n", m.join(","))
    }

    #[test]
    fn earliest_recv_wins_and_hash_conflicts_fail() {
        let mut idx = FeedIndex::default();
        let text = format!(
            "{}{}1\t0\t0\t{{\"version\":1,\"confirmedSequenceNumberMessage\":{{\"sequenceNumber\":9}}}}\n{}{}",
            line(30, &[(10, "0xAA")]),
            line(20, &[(10, "0xaa"), (11, "0xbb")]),
            line(40, &[(12, "0xcc")]),
            line(50, &[(99, "0xdd")]),
        );
        let wanted = [Range { from: 10, to: 12 }];
        idx.add_text("t", text.as_bytes(), Some(&wanted)).unwrap();
        assert_eq!(idx.get(10), Some(&FeedSeen { recv_ns: 20, block_hash: Some("0xaa".into()) }));
        assert_eq!(idx.get(11).unwrap().recv_ns, 20);
        assert_eq!(idx.get(12).unwrap().recv_ns, 40);
        assert_eq!(idx.get(99), None);
        assert_eq!(
            idx.stats,
            FeedStats {
                lines: 5,
                sequenced_lines: 4,
                messages: 4,
                messages_out_of_range: 1,
                repeats: 1,
                ..FeedStats::default()
            }
        );
        let conflict = line(60, &[(12, "0xee")]);
        assert!(idx.add_text("t", conflict.as_bytes(), None).is_err());
        assert!(idx.add_text("t", b"x\ty\n", None).is_err());
        assert!(idx.add_text("t", b"5\t7\t7\t{broken\n", None).is_err());
    }

    /// Real line: block 76491176 of data/feed/2026/09/30/feed-20260930-11.tsv.zst (2026-09-30),
    /// `l2Msg` and `signatureV2` shortened. Its blockHash equals the RPC `block.hash` of
    /// data/blocks/blocks-76491176-76491195.jsonl.zst.
    #[test]
    fn real_feed_line() {
        let l = "1790768868979402000\t76491176\t76491176\t{\"version\":1,\"messages\":[{\"sequenceNumber\":76491176,\"message\":{\"message\":{\"header\":{\"kind\":3,\"sender\":\"0xa4b000000000000000000073657175656e636572\",\"blockNumber\":26090091,\"timestamp\":1790768868,\"requestId\":null,\"baseFeeL1\":0},\"l2Msg\":\"AAAA\"},\"delayedMessagesRead\":336446},\"blockHash\":\"0x8c2fc16f5b33c0f8c227dbd045647fb625671f96e25b934e05e45ed3f327858b\",\"signatureV2\":\"AA==\",\"blockMetadata\":null}]}\n";
        let mut idx = FeedIndex::default();
        idx.add_text("t", l.as_bytes(), None).unwrap();
        assert_eq!(idx.get(76491176).unwrap().recv_ns, 1_790_768_868_979_402_000);
        assert_eq!(
            idx.get(76491176).unwrap().block_hash.as_deref(),
            Some("0x8c2fc16f5b33c0f8c227dbd045647fb625671f96e25b934e05e45ed3f327858b")
        );
    }
}
