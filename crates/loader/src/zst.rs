//! Whole-file zstd decoding before any row is built (review 026, Н1).
//!
//! `zstd -dc` and streaming readers hand out decoded bytes before the frame's XXH64 content
//! checksum is compared at the frame end. The loader therefore decodes the whole file into memory
//! first: a checksum mismatch, a truncated frame or garbage fails the file before a single row
//! exists, which is what `zstd -t` checks. Files written before task 026 have no checksum
//! (`Check: None`): a corrupted byte there can decode silently, so [`Zst`] reports how many
//! frames carry one and the loader prints it.

use std::path::Path;

use anyhow::{bail, Context, Result};

/// zstd frame magic (little endian on disk).
const ZSTD_MAGIC: u32 = 0xFD2F_B528;
/// Skippable frames: magic `0x184D2A50..=0x184D2A5F`.
const SKIPPABLE_MASK: u32 = 0xFFFF_FFF0;
const SKIPPABLE_MAGIC: u32 = 0x184D_2A50;
/// `Content_Checksum_flag` in the Frame_Header_Descriptor (RFC 8878, 3.1.1.1.1).
const CHECKSUM_FLAG: u8 = 0x04;

/// Decoded file and what its frames carry.
#[derive(Debug)]
pub struct Zst {
    pub data: Vec<u8>,
    /// zstd frames (skippable frames not counted).
    pub frames: usize,
    /// Frames with a content checksum.
    pub frames_with_checksum: usize,
}

impl Zst {
    /// Every frame has a content checksum (and there is at least one frame).
    pub fn fully_checksummed(&self) -> bool {
        self.frames > 0 && self.frames == self.frames_with_checksum
    }
}

/// Reads and decodes a whole `.zst` file (all frames). An empty file decodes to no data.
///
/// # Errors
/// IO error, a broken frame, a content checksum mismatch.
pub fn read_zst(path: &Path) -> Result<Zst> {
    let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    decode(&raw).with_context(|| format!("zstd {}", path.display()))
}

/// [`read_zst`] on bytes.
///
/// # Errors
/// See [`read_zst`].
pub fn decode(raw: &[u8]) -> Result<Zst> {
    if raw.is_empty() {
        return Ok(Zst { data: Vec::new(), frames: 0, frames_with_checksum: 0 });
    }
    let (frames, frames_with_checksum) = frame_flags(raw)?;
    // The decoder reads across concatenated frames and verifies each frame's checksum.
    let data = zstd::stream::decode_all(raw).context("decode")?;
    Ok(Zst { data, frames, frames_with_checksum })
}

/// `(frames, frames with checksum)` by walking the frame headers.
fn frame_flags(mut rest: &[u8]) -> Result<(usize, usize)> {
    let (mut frames, mut checked) = (0, 0);
    while !rest.is_empty() {
        let Some(magic) = rest.get(..4).map(|m| u32::from_le_bytes([m[0], m[1], m[2], m[3]])) else {
            bail!("trailing {} bytes after the last frame", rest.len());
        };
        let len = zstd::zstd_safe::find_frame_compressed_size(rest)
            .map_err(|code| anyhow::anyhow!("frame {}: {}", frames + 1, zstd::zstd_safe::get_error_name(code)))?;
        if magic == ZSTD_MAGIC {
            frames += 1;
            if rest.get(4).is_some_and(|d| d & CHECKSUM_FLAG != 0) {
                checked += 1;
            }
        } else if magic & SKIPPABLE_MASK != SKIPPABLE_MAGIC {
            bail!("frame {}: bad magic {magic:#010x}", frames + 1);
        }
        // Explicit guard instead of trusting the C library's contract (same as recorder recovery).
        rest = rest
            .get(len..)
            .filter(|_| len > 0)
            .ok_or_else(|| anyhow::anyhow!("frame {}: bad size {len} of {} bytes left", frames + 1, rest.len()))?;
    }
    Ok((frames, checked))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn frame(data: &[u8], checksum: bool) -> Vec<u8> {
        let mut e = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        e.include_checksum(checksum).unwrap();
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn counts_frames_and_checksums() {
        let mut raw = frame(b"a\n", true);
        raw.extend(frame(b"b\n", false));
        let z = decode(&raw).unwrap();
        assert_eq!((z.data.as_slice(), z.frames, z.frames_with_checksum), (&b"a\nb\n"[..], 2, 1));
        assert!(!z.fully_checksummed());
        assert!(decode(&frame(b"x", true)).unwrap().fully_checksummed());
        assert_eq!(decode(b"").unwrap().frames, 0);
    }

    #[test]
    fn corruption_and_truncation_fail_the_whole_file() {
        let data: Vec<u8> = (0..3_000u32).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
        let good = frame(&data, true);
        assert!(decode(&good).is_ok());
        assert!(decode(&good[..good.len() - 1]).is_err(), "truncated");
        // Every single-byte change of the frame body is caught when the checksum is there
        // (the last 4 bytes are the checksum itself).
        let mut caught = 0;
        let body = 6..good.len() - 4;
        for i in body.clone() {
            let mut bad = good.clone();
            bad[i] ^= 0x01;
            if decode(&bad).map(|z| z.data != data).unwrap_or(true) {
                caught += 1;
            }
            assert!(decode(&bad).map(|z| z.data == data).unwrap_or(true), "byte {i}: wrong data decoded silently");
        }
        assert!(caught > 0);
        let mut garbage = good.clone();
        garbage.extend_from_slice(b"xyz!");
        assert!(decode(&garbage).is_err(), "trailing garbage");
    }
}
