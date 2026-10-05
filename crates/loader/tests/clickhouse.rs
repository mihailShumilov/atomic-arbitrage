//! End-to-end test against a TEMPORARY ClickHouse (never the working one). Ignored by default:
//!
//! ```text
//! HOOD_LOADER_TEST_ENV_FILE=<env file with CLICKHOUSE_URL=http://127.0.0.1:<port>/ and
//!   CLICKHOUSE_PASSWORD> cargo test -p loader --test clickhouse -- --ignored --nocapture
//! ```
//!
//! The server must have `sql/apply.sh` applied and empty `hood.blocks/txs/logs/funding_edges/
//! feed_gaps` (a fresh container on a bind mount, see data-model.md "Загрузчик"). The test refuses
//! port 18123 (the working local ClickHouse). Inputs, all read-only: `data/blocks`,
//! `data/samples/hourly-20260804-20260930.jsonl.zst`, the feed copies `data/feed`,
//! `data/feed-test-009`, `data/feed-test-002` with their `gaps.tsv`, and the decoder fixture
//! `crates/decoders/tests/fixtures/l1-inflows-blocks.jsonl` (real blocks, see
//! `crates/decoders/tests/l1_inflows.rs`). Temporary files go to `$TMPDIR`.
//!
//! Checks: row counts and `uniqExact` of the keys equal the source; a full round trip of every
//! `blocks`/`txs`/`logs` row (ClickHouse output = loader TSV); a second and a third load (the
//! third without the feed copies) change nothing after `FINAL` (content hash); a failing INSERT of
//! the last table rolls the file back; a corrupted file inserts nothing; `feed_gaps` repeats and
//! conflicts.

use std::path::{Path, PathBuf};
use std::process::Command;

use decoders::l1_inflows::GatewayRegistry;
use loader::blocks_file::read_blocks_file;
use loader::ch::Client;
use loader::config::ChConfig;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Env {
    env_file: PathBuf,
    ch: Client,
    rt: tokio::runtime::Runtime,
}

impl Env {
    fn q(&self, sql: &str) -> String {
        self.rt.block_on(self.ch.query(sql)).unwrap_or_else(|e| panic!("{e:#}"))
    }

    fn n(&self, sql: &str) -> u64 {
        self.q(&format!("{sql} FORMAT TabSeparated")).trim().parse().unwrap()
    }

    /// Runs the loader binary; returns (success, output).
    fn load(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_loader"))
            .current_dir(repo())
            .env("NO_COLOR", "1")
            .env("RUST_LOG", "info")
            .arg("--env-file")
            .arg(&self.env_file)
            .args(args)
            .output()
            .unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text)
    }

    /// `count() FINAL`, a content hash over every row after `FINAL`, and the rows the hash missed (0).
    fn fingerprint(&self, table: &str) -> String {
        // cityHash64(*) is NULL when any column is NULL (blocks.feed_recv_ns) and groupBitXor skips
        // NULL: hash the text of the whole row instead (review 032 data-auditor, З1).
        self.q(&format!(
            "SELECT count(), groupBitXor(cityHash64(toString(tuple(*)))), countIf(isNull(cityHash64(toString(tuple(*))))) \
             FROM {table} FINAL FORMAT TabSeparated"
        ))
    }

    fn fingerprints(&self) -> Vec<String> {
        ["hood.blocks", "hood.txs", "hood.logs", "hood.funding_edges", "hood.feed_gaps"]
            .iter()
            .map(|t| {
                let f = self.fingerprint(t);
                assert!(f.ends_with("\t0\n"), "{t}: rows missed by the hash: {f}");
                f
            })
            .collect()
    }
}

fn setup() -> Env {
    let Ok(env_file) = std::env::var("HOOD_LOADER_TEST_ENV_FILE") else {
        panic!("set HOOD_LOADER_TEST_ENV_FILE (temporary ClickHouse), see the module docs");
    };
    // Review 032 data-auditor, Н5: an explicitly requested run must not pass without its inputs.
    for p in ["data/blocks", SAMPLES, "data/feed", "data/feed-test-009", "data/feed-test-002"] {
        assert!(
            repo().join(p).exists(),
            "{p} is missing: this test needs the local data/ copies (see the module docs)"
        );
    }
    let cfg = ChConfig::load(Path::new(&env_file)).unwrap();
    assert!(!cfg.url.contains(":18123"), "refusing the working local ClickHouse ({})", cfg.url);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let env = Env { env_file: env_file.into(), ch: Client::new(cfg).unwrap(), rt };
    for t in ["hood.blocks", "hood.txs", "hood.logs", "hood.funding_edges", "hood.feed_gaps"] {
        assert_eq!(env.n(&format!("SELECT count() FROM {t}")), 0, "{t} is not empty: use a fresh temporary server");
    }
    env
}

/// Data lines of a TSV batch body (header dropped), sorted by the first two numeric columns.
fn sorted_lines(bodies: &[Vec<u8>], key: impl Fn(&str) -> (u64, u64)) -> Vec<String> {
    let mut v: Vec<String> = bodies
        .iter()
        .flat_map(|b| String::from_utf8(b.clone()).unwrap().lines().skip(1).map(str::to_owned).collect::<Vec<_>>())
        .collect();
    v.sort_by_key(|l| key(l));
    v
}

fn cols(l: &str, a: usize, b: usize) -> (u64, u64) {
    let f: Vec<&str> = l.split('\t').collect();
    (f[a].parse().unwrap(), f[b].parse().unwrap())
}

const SAMPLES: &str = "data/samples/hourly-20260804-20260930.jsonl.zst";
const FIXTURE: &str = "crates/decoders/tests/fixtures/l1-inflows-blocks.jsonl";

#[test]
#[ignore = "needs a temporary ClickHouse: HOOD_LOADER_TEST_ENV_FILE"]
fn load_counts_roundtrip_idempotency_rollback() {
    let env = setup();
    let full: Vec<&str> = vec![
        "--blocks",
        "data/blocks",
        "--blocks",
        SAMPLES,
        "--feed-dir",
        "data/feed",
        "--feed-dir",
        "data/feed-test-009",
        "--feed-dir",
        "data/feed-test-002",
        "--gaps",
        "data/feed-test-009/gaps.tsv",
        "--gaps",
        "data/feed-test-002/gaps.tsv",
    ];

    // Source side, through the loader's own reader (round trip) and plain counters.
    let mut files = loader::expand_blocks_paths(&[repo().join("data/blocks")]).unwrap();
    files.push(repo().join(SAMPLES));
    let (mut blocks, mut txs, mut logs) = (Vec::new(), Vec::new(), Vec::new());
    let (mut n_blocks, mut n_txs, mut n_logs) = (0u64, 0u64, 0u64);
    for f in &files {
        let r = read_blocks_file(f, &GatewayRegistry::builtin()).unwrap();
        n_blocks += r.stats.blocks as u64;
        n_txs += r.stats.txs as u64;
        n_logs += r.stats.logs as u64;
        assert_eq!(r.edges.rows(), 0, "no L1 messages in data/ (checked by 027)");
        let mut b = loader::tsv::Batch::<loader::rows::BlockRow>::default();
        for row in &r.blocks {
            b.push(row).unwrap();
        }
        blocks.push(b.body().to_vec());
        txs.push(r.txs.body().to_vec());
        logs.push(r.logs.body().to_vec());
    }

    // 1. First load.
    let (ok, out) = env.load(&full);
    assert!(ok, "{out}");
    println!("first load:\n{out}");
    assert_eq!(env.n("SELECT count() FROM hood.blocks FINAL"), n_blocks);
    assert_eq!(env.n("SELECT uniqExact(block_number) FROM hood.blocks"), n_blocks);
    assert_eq!(env.n("SELECT count() FROM hood.txs FINAL"), n_txs);
    assert_eq!(env.n("SELECT uniqExact(block_number, tx_index) FROM hood.txs"), n_txs);
    assert_eq!(env.n("SELECT count() FROM hood.logs FINAL"), n_logs);
    assert_eq!(env.n("SELECT uniqExact(block_number, log_index) FROM hood.logs"), n_logs);
    assert_eq!(env.n("SELECT count() FROM hood.funding_edges"), 0);
    assert_eq!(env.n("SELECT countIf(block_number = 0) FROM hood.txs"), 0);
    // Every tx of a block that is in hood.blocks, and tx_count agrees.
    assert_eq!(env.n("SELECT sum(tx_count) FROM hood.blocks FINAL"), n_txs);
    assert_eq!(
        env.n("SELECT count() FROM hood.txs WHERE block_number NOT IN (SELECT block_number FROM hood.blocks)"),
        0
    );

    // Full round trip: what ClickHouse returns equals what the loader sent.
    let got = env.q(
        "SELECT block_number, block_hash, toUnixTimestamp(ts), l1_block, base_fee_wei, tx_count FROM hood.blocks FINAL \
         ORDER BY block_number FORMAT TabSeparated",
    );
    let want: Vec<String> = sorted_lines(&blocks, |l| cols(l, 0, 0))
        .iter()
        .map(|l| l.rsplit_once('\t').unwrap().0.to_owned()) // drop feed_recv_ns (\N before the merge)
        .collect();
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
    let got = env.q("SELECT * FROM hood.txs FINAL ORDER BY block_number, tx_index FORMAT TabSeparated");
    assert_eq!(got.lines().map(str::to_owned).collect::<Vec<_>>(), sorted_lines(&txs, |l| cols(l, 0, 1)));
    let got = env.q("SELECT * FROM hood.logs FINAL ORDER BY block_number, log_index FORMAT TabSeparated");
    assert_eq!(got.lines().map(str::to_owned).collect::<Vec<_>>(), sorted_lines(&logs, |l| cols(l, 0, 2)));

    // Feed time: the real first line of block 76491176 in data/feed (2026-09-30, 11 h file).
    assert_eq!(
        env.n("SELECT feed_recv_ns FROM hood.blocks FINAL WHERE block_number = 76491176"),
        1_790_768_868_979_402_000
    );
    let feed = loader::feed::FeedIndex::from_dirs(
        &["data/feed", "data/feed-test-009", "data/feed-test-002"].map(|d| repo().join(d)),
        None,
    )
    .unwrap();
    let with_time = env.q(
        "SELECT block_number, feed_recv_ns FROM hood.blocks FINAL WHERE feed_recv_ns IS NOT NULL FORMAT TabSeparated",
    );
    let mut n_with_time = 0;
    for r in with_time.lines() {
        let (n, t) = cols(r, 0, 1);
        assert_eq!(feed.get(n).unwrap().recv_ns, t, "block {n}");
        n_with_time += 1;
    }
    let expected_with_time =
        sorted_lines(&blocks, |l| cols(l, 0, 0)).iter().filter(|l| feed.get(cols(l, 0, 0).0).is_some()).count();
    assert_eq!(n_with_time, expected_with_time);
    println!("blocks with feed_recv_ns: {n_with_time}");

    // Gaps: two files, two distinct gaps, none filled.
    assert_eq!(env.n("SELECT count() FROM hood.feed_gaps FINAL"), 2);
    assert_eq!(env.n("SELECT countIf(filled = 1) FROM hood.feed_gaps FINAL"), 0);

    // 2. Second load: same result after FINAL. 3. Third load without the feed: feed_recv_ns kept.
    let before = env.fingerprints();
    let (ok, out) = env.load(&full);
    assert!(ok, "{out}");
    assert_eq!(env.fingerprints(), before, "second load changed the content");
    let no_feed: Vec<&str> = vec!["--blocks", "data/blocks", "--blocks", SAMPLES];
    let (ok, out) = env.load(&no_feed);
    assert!(ok, "{out}");
    assert_eq!(env.fingerprints(), before, "load without the feed changed the content");
    println!("fingerprints after 3 loads: {before:?}");

    // 4. Rollback: the INSERT of hood.blocks (the last table) fails -> the fixture's rows in
    // txs/logs/funding_edges are deleted again. The constraint exists only for this step.
    let fixture_blocks = "77285521, 77285531, 77300695, 77312169";
    env.q("ALTER TABLE hood.blocks ADD CONSTRAINT loader_test_fail CHECK block_number < 1000");
    let (ok, out) = env.load(&["--blocks", FIXTURE]);
    env.q("ALTER TABLE hood.blocks DROP CONSTRAINT loader_test_fail");
    assert!(!ok, "{out}");
    assert!(out.contains("rolled back"), "{out}");
    for t in ["hood.blocks", "hood.txs", "hood.logs", "hood.funding_edges"] {
        assert_eq!(env.n(&format!("SELECT count() FROM {t} WHERE block_number IN ({fixture_blocks})")), 0, "{t}");
    }
    assert_eq!(env.fingerprints(), before, "rollback left something behind");

    // 5. A corrupted file (zstd with checksum, one byte flipped) inserts nothing.
    let tmp = std::env::temp_dir().join(format!("loader-test-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let raw = std::fs::read(repo().join(FIXTURE)).unwrap();
    let mut e = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    e.include_checksum(true).unwrap();
    std::io::Write::write_all(&mut e, &raw).unwrap();
    let mut z = e.finish().unwrap();
    let good = tmp.join("fixture.jsonl.zst");
    std::fs::write(&good, &z).unwrap();
    let mid = z.len() / 2;
    z[mid] ^= 0x10;
    let bad = tmp.join("fixture-corrupt.jsonl.zst");
    std::fs::write(&bad, &z).unwrap();
    let (ok, out) = env.load(&["--blocks", bad.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(out.contains("zstd"), "{out}");
    assert_eq!(env.fingerprints(), before, "corrupted file inserted rows");

    // 6. The fixture itself (checksummed copy): L1 edges go in, twice = once.
    for _ in 0..2 {
        let (ok, out) = env.load(&["--blocks", good.to_str().unwrap()]);
        assert!(ok, "{out}");
    }
    assert_eq!(env.n("SELECT count() FROM hood.funding_edges FINAL"), 3);
    assert_eq!(
        env.n("SELECT countIf(kind = 'l1_token' AND gateway_status = 'verified') FROM hood.funding_edges FINAL"),
        1
    );
    assert_eq!(env.n("SELECT countIf(kind = 'l1_eth' AND log_index = 4294967295) FROM hood.funding_edges FINAL"), 2);
    assert_eq!(env.n("SELECT uniqExact(to_addr, block_number, tx_index, kind, log_index) FROM hood.funding_edges"), 3);
    assert_eq!(env.n(&format!("SELECT count() FROM hood.blocks FINAL WHERE block_number IN ({fixture_blocks})")), 4);

    // 7. feed_gaps: filled = 1 for a range fully in hood.blocks, repeats collapse, a conflicting
    // to_seq for a known from_seq is refused and changes nothing.
    let gaps = tmp.join("gaps.tsv");
    std::fs::write(&gaps, "713002\t713201\t1\n713002\t713201\t5\n27607490\t27607500\t2\n").unwrap();
    for _ in 0..2 {
        let (ok, out) = env.load(&["--gaps", gaps.to_str().unwrap()]);
        assert!(ok, "{out}");
    }
    assert_eq!(
        env.q("SELECT * FROM hood.feed_gaps FINAL WHERE from_seq < 30000000 ORDER BY from_seq FORMAT TabSeparated"),
        "713002\t713201\t1\t1\n27607490\t27607500\t2\t0\n"
    );
    let before_gaps = env.fingerprint("hood.feed_gaps");
    std::fs::write(&gaps, "713002\t713202\t1\n").unwrap();
    let (ok, out) = env.load(&["--gaps", gaps.to_str().unwrap()]);
    assert!(!ok && out.contains("to_seq"), "{out}");
    assert_eq!(env.fingerprint("hood.feed_gaps"), before_gaps);

    std::fs::remove_dir_all(&tmp).unwrap();
}
