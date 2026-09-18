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

/// The fetched tier lives next to the manifest, never in the tree.
fn loghub_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/loghub");
    dir.canonicalize().ok()
}

/// One ground-truth template: its event id and a matcher for message content.
/// `<*>` becomes a non-greedy wildcard; everything else is literal.
struct TruthTemplate {
    id: String,
    wildcards: usize,
    literal_len: usize,
    matcher: regex::Regex,
}

fn load_truth(csv_path: &Path) -> Vec<TruthTemplate> {
    let mut reader = csv::Reader::from_path(csv_path).expect("read templates csv");
    let mut templates = Vec::new();
    for record in reader.records() {
        let record = record.expect("parse csv record");
        let id = record[0].to_owned();
        let raw = &record[1];
        let wildcards = raw.matches("<*>").count();
        let literal_len = raw.len() - wildcards * "<*>".len();
        let pattern = raw
            .split("<*>")
            .map(regex::escape)
            .collect::<Vec<_>>()
            .join(".*?");
        let matcher = regex::Regex::new(&format!("^{pattern}$")).expect("template regex");
        templates.push(TruthTemplate {
            id,
            wildcards,
            literal_len,
            matcher,
        });
    }
    templates
}

/// The syslog header (`Mmm DD HH:MM:SS host proc[pid]`) is not part of the
/// ground-truth templates, so matching runs on the message content: everything
/// after the first `": "`, which is always the header separator (headers never
/// contain one). Surrounding whitespace is printk alignment, not content.
/// Masking and training still see the verbatim line.
fn content_of(line: &str) -> &str {
    line.split_once(": ")
        .map(|(_, rest)| rest)
        .unwrap_or(line)
        .trim()
}

/// Match message content against ground truth. Most specific wins (fewest
/// wildcards, then longest literal); a tie is genuinely ambiguous.
fn match_truth<'a>(line: &str, truth: &'a [TruthTemplate]) -> TruthMatch<'a> {
    let content = content_of(line);
    let mut best: Option<&TruthTemplate> = None;
    let mut tied = false;
    for candidate in truth.iter().filter(|t| t.matcher.is_match(content)) {
        match &best {
            None => best = Some(candidate),
            Some(current)
                if (
                    candidate.wildcards,
                    std::cmp::Reverse(candidate.literal_len),
                ) < (current.wildcards, std::cmp::Reverse(current.literal_len)) =>
            {
                best = Some(candidate);
                tied = false;
            }
            Some(current)
                if candidate.wildcards == current.wildcards
                    && candidate.literal_len == current.literal_len =>
            {
                tied = true;
            }
            _ => {}
        }
    }
    match (best, tied) {
        (Some(template), false) => TruthMatch::One(template.id.as_str()),
        (Some(_), true) => TruthMatch::Ambiguous,
        (None, _) => TruthMatch::Unmatched,
    }
}

enum TruthMatch<'a> {
    One(&'a str),
    Unmatched,
    Ambiguous,
}

/// Ground-truth event pairs the port is *reviewed* to merge: single-token
/// differences in lines of ten or more tokens, which is exactly what a 0.90
/// similarity threshold means. All three are same-verdict merges (a username,
/// a cache kind, a driver name) — identity detail lost at template level,
/// preserved in the raw lines. Anything outside this list fails the gate:
///
/// - E18/E19: `... user=root` vs `... user=test` (12/13 tokens equal)
/// - E55/E71: `Inode-cache ...` vs `Mount-cache ...` (12/13 tokens equal)
/// - E110/E111: `... driver hub` vs `... driver usbfs` (9/10 tokens equal)
/// - E20/E21: `BIOS-e820: ... (usable)` vs `... (reserved)` (9/10 tokens equal)
/// - E41/E50/E74: `DMA zone:` vs `HighMem zone:` vs `Normal zone:` (9/10 equal)
/// - OpenSSH E4/E5: `... failures for admin` vs `... for root` (username)
/// - OpenSSH E15/E16: `... authentication failure;` vs `... failures;`
///
/// Raising the threshold to split these is rejected: the parent spec fixes
/// 0.90, and a threshold that never merges single-token differences in long
/// lines is a threshold that never generalizes at all.
/// Event ids live per file (Linux E4 is not OpenSSH E4), so each entry names
/// its file.
const REVIEWED_MERGES: &[(&str, &[&str])] = &[
    ("Linux_2k", &["E18", "E19"]),
    ("Linux_2k", &["E55", "E71"]),
    ("Linux_2k", &["E110", "E111"]),
    ("Linux_2k", &["E20", "E21"]),
    ("Linux_2k", &["E41", "E50", "E74"]),
    ("OpenSSH_2k", &["E4", "E5"]),
    ("OpenSSH_2k", &["E15", "E16"]),
];

/// Train a fetched file and report grouping agreement with ground truth (when
/// a `_templates.csv` sibling exists) or plain cluster lists (when not).
/// The impurity assertion is the gate: over unambiguous lines, every port
/// cluster mixing ground-truth events must be a reviewed pair. Over-splitting
/// is reported for review, never asserted — it is the designed direction.
#[test]
fn the_fetched_corpus_agrees_with_ground_truth() {
    let Some(dir) = loghub_dir() else {
        eprintln!("skipping: corpus/loghub is absent; run scripts/fetch-corpus.sh");
        return;
    };

    for path in log_files(&dir) {
        let name = path
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .into_owned();
        let contents = std::fs::read_to_string(&path).expect("read corpus file");
        let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();

        let csv_path = path.with_extension("log_templates.csv");
        let truth = csv_path.exists().then(|| load_truth(&csv_path));

        // Train every line; the gate only judges unambiguous ones.
        let bundle = oarfish_mask::curated();
        let mut drain = Drain::new(Config::default()).expect("valid");
        let mut seq_of_line: Vec<u64> = Vec::with_capacity(lines.len());
        let mut event_of_line: Vec<Option<&str>> = Vec::with_capacity(lines.len());
        let mut unmatched = 0;
        let mut ambiguous = 0;
        for line in &lines {
            let masked = bundle.mask(line);
            seq_of_line.push(drain.train(masked.template()).seq);
            if let Some(truth) = &truth {
                match match_truth(line, truth) {
                    TruthMatch::One(event) => event_of_line.push(Some(event)),
                    TruthMatch::Unmatched => {
                        unmatched += 1;
                        event_of_line.push(None);
                    }
                    TruthMatch::Ambiguous => {
                        ambiguous += 1;
                        event_of_line.push(None);
                    }
                }
            }
        }

        let mut out = String::new();
        if let Some(truth) = &truth {
            // seq -> set of truth events over unambiguous lines.
            let mut events_of_seq: std::collections::HashMap<u64, Vec<&str>> =
                std::collections::HashMap::new();
            for (seq, event) in seq_of_line.iter().zip(event_of_line.iter()) {
                if let Some(event) = event {
                    let entry = events_of_seq.entry(*seq).or_default();
                    if !entry.contains(event) {
                        entry.push(event);
                    }
                }
            }
            let mut impure: Vec<(u64, Vec<&str>)> = events_of_seq
                .into_iter()
                .filter(|(_, events)| events.len() > 1)
                .collect();
            impure.sort_by_key(|(seq, _)| *seq);

            // event -> set of port seqs, for the split report.
            let mut seqs_of_event: std::collections::HashMap<&str, Vec<u64>> =
                std::collections::HashMap::new();
            for (seq, event) in seq_of_line.iter().zip(event_of_line.iter()) {
                if let Some(event) = event {
                    let entry = seqs_of_event.entry(event).or_default();
                    if !entry.contains(seq) {
                        entry.push(*seq);
                    }
                }
            }
            let mut split: Vec<(&str, usize)> = seqs_of_event
                .iter()
                .map(|(event, seqs)| (*event, seqs.len()))
                .filter(|(_, n)| *n > 1)
                .collect();
            split.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

            let unambiguous = event_of_line.iter().filter(|e| e.is_some()).count();
            out.push_str(&format!(
                "truth_templates: {} port_clusters: {}\n",
                truth.len(),
                drain.clusters().count()
            ));
            out.push_str(&format!(
                "unambiguous: {unambiguous} unmatched: {unmatched} ambiguous: {ambiguous}\n"
            ));
            out.push_str(&format!("impure_clusters: {}\n", impure.len()));
            for (seq, events) in &impure {
                let template = drain.get(*seq).map(|c| c.template.as_str()).unwrap_or("?");
                out.push_str(&format!("  seq {seq} mixes {events:?}: {template}\n"));
            }
            out.push_str(&format!("split_events: {}\n", split.len()));
            for (event, n) in split.iter().take(10) {
                out.push_str(&format!("  {event} in {n} clusters\n"));
            }

            assert!(
                impure.iter().all(|(_, events)| {
                    let mut sorted = events.clone();
                    sorted.sort();
                    REVIEWED_MERGES.contains(&(name.as_str(), sorted.as_slice()))
                }),
                "port merged ground-truth events outside the reviewed list in {name}: {impure:?}"
            );
        }

        let examples: Vec<(u64, String)> = seq_of_line
            .iter()
            .zip(lines.iter())
            .map(|(seq, line)| (*seq, (*line).to_owned()))
            .collect();
        out.push_str(&render(&drain, &examples));

        insta::with_settings!({ snapshot_suffix => name, omit_expression => true }, {
            insta::assert_snapshot!(out);
        });
    }
}
