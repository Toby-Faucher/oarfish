//! Trains one Drain per corpus file and snapshots the cluster list.
//!
//! Tuning depth or the threshold should show exactly which templates moved.
//! That is the whole point of reviewing these diffs rather than asserting on
//! them one at a time.
//!
//! The committed corpus lives under `oarfish-mask`: it is read from there, not
//! duplicated here. `insta::glob!` cannot see across crates, so this harness
//! reads the directory directly and snapshots per file stem, the same shape as
//! M1's fetched tier.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use oarfish_drain::{Config, Drain};

/// The committed corpus, owned by `oarfish-mask`.
pub fn committed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../oarfish-mask/tests/corpus")
}

/// `.log` files in a directory, sorted. Adding a file never means editing a test.
pub fn log_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read corpus dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "log"))
        .collect();
    files.sort();
    files
}

/// Mask every non-blank line and train them through a fresh Drain, recording
/// which raw line first opened each cluster.
pub fn train_contents(contents: &str) -> (Drain, Vec<(u64, String)>) {
    let bundle = oarfish_mask::curated();
    let mut drain = Drain::new(Config::default()).expect("default config is valid");
    let mut examples = Vec::new();
    for line in contents.lines().filter(|l| !l.trim().is_empty()) {
        let masked = bundle.mask(line);
        let assignment = drain.train(masked.template());
        examples.push((assignment.seq, line.to_owned()));
    }
    (drain, examples)
}

/// Render the cluster list: header counts, then `size template` lines with one
/// example raw line each. Sorted by size desc so the review reads head-first.
pub fn render(drain: &Drain, examples: &[(u64, String)]) -> String {
    let mut first_seen: HashMap<u64, &str> = HashMap::new();
    for (seq, raw) in examples {
        first_seen.entry(*seq).or_insert(raw.as_str());
    }
    let mut clusters: Vec<_> = drain.clusters().collect();
    let total: u64 = clusters.iter().map(|c| c.size).sum();
    let mut out = format!("lines: {total} clusters: {}\n", clusters.len());
    clusters.sort_by(|a, b| b.size.cmp(&a.size).then(a.template.cmp(&b.template)));
    for cluster in clusters {
        let example = first_seen.get(&cluster.seq).copied().unwrap_or("?");
        out.push_str(&format!(
            "{} {}\n  eg: {example}\n",
            cluster.size, cluster.template
        ));
    }
    out
}

#[test]
fn the_committed_corpus_clusters_as_snapshotted() {
    for path in log_files(&committed_dir()) {
        let name = path
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .into_owned();
        let contents = std::fs::read_to_string(&path).expect("read corpus file");
        let (drain, examples) = train_contents(&contents);
        insta::with_settings!({ snapshot_suffix => name, omit_expression => true }, {
            insta::assert_snapshot!(render(&drain, &examples));
        });
    }
}

#[test]
fn the_committed_corpus_trains_deterministically() {
    for path in log_files(&committed_dir()) {
        let contents = std::fs::read_to_string(&path).expect("read corpus file");
        let (first, _) = train_contents(&contents);
        let (second, examples) = train_contents(&contents);
        // Same file twice: identical cluster multisets. Rendered (sorted)
        // output makes the comparison order-independent.
        assert_eq!(
            render(&first, &examples),
            render(&second, &examples),
            "nondeterministic clustering in {}",
            path.display()
        );
    }
}
