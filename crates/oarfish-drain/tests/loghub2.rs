//! Scores the Drain port against loghub-2.0's corrected 2k samples, with
//! loghub-2.0's own grouping metrics, so the numbers sit beside the published
//! Drain results rather than on a scale of our own.
//!
//! Each system is trained twice, through fresh Drains and the curated mask:
//!
//! - `content`: the message after loghub's per-system header split, which is
//!   what the benchmark's parsers see. Comparable with the published column.
//! - `line`: each verbatim line through the syslog listener's own
//!   `frame_to_event`, then `Event::body_lossy`, exactly as the daemon
//!   clusters it: a real syslog header loses its timestamp and host, any
//!   other line clusters whole. This is the number that decides whether an
//!   alarm fires.
//!
//! GA (grouping accuracy) is the share of lines whose port cluster holds
//! exactly the lines of their ground-truth group. FGA is the F1 over groups
//! reproduced exactly. Both are ports of `get_accuracy` in loghub-2.0's
//! `benchmark/logparser/utils/evaluator.py`; ground truth groups by
//! `EventTemplate`, as there. PA and FTA are not ported: they compare template
//! text, and oarfish's `<VAR:IP4>` slots are not loghub's `<*>`.
//!
//! Merges are listed per system, because a merge is the failure that deletes
//! an alarm. Splits only show up as lower scores; the 0.90 threshold chooses
//! them on purpose.
//!
//! Needs `scripts/fetch-corpus.sh`; skips when `corpus/loghub-2.0` is absent.
//! Nothing asserts a score floor: the snapshots are the gate, so a change to
//! the mask or the threshold shows exactly which systems moved, and by how much.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use oarfish_drain::{Config, Drain};

/// Drain's published 2k scores (GA, FGA), from loghub-2.0's
/// `RQs_experiments/RQ2/effectiveness_results.csv` at ac4aad2, first column.
const PUBLISHED: &[(&str, f64, f64)] = &[
    ("Apache", 1.000, 1.000),
    ("BGL", 0.963, 0.833),
    ("HDFS", 0.998, 0.839),
    ("HPC", 0.887, 0.753),
    ("Hadoop", 0.948, 0.820),
    ("HealthApp", 0.780, 0.351),
    ("Linux", 0.690, 0.930),
    ("Mac", 0.786, 0.797),
    ("OpenSSH", 0.789, 0.880),
    ("OpenStack", 0.733, 0.117),
    ("Proxifier", 0.526, 0.538),
    ("Spark", 0.922, 0.870),
    ("Thunderbird", 0.955, 0.773),
    ("Zookeeper", 0.967, 0.854),
];

/// How many merges to print per system. The count is always complete.
const MERGES_SHOWN: usize = 10;

fn corpus_dir() -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../corpus/loghub-2.0")
        .canonicalize()
        .ok()
}

/// One system's 2k sample: the verbatim lines, the header-split content, and
/// the ground-truth template of each line, all indexed alike.
struct Sample {
    lines: Vec<String>,
    contents: Vec<String>,
    truth: Vec<String>,
}

/// Read `<system>_2k.log` and its structured CSV. The CSV's row N is the log's
/// line N (`LineId` is 1-based and dense); a mismatch fails loudly rather than
/// scoring the wrong line.
fn load(dir: &Path, system: &str) -> Sample {
    let log = std::fs::read(dir.join(format!("{system}_2k.log"))).expect("read log");
    let lines: Vec<String> = String::from_utf8_lossy(&log)
        .lines()
        .map(str::to_owned)
        .collect();

    let csv_path = dir.join(format!("{system}_2k.log_structured_corrected.csv"));
    let mut reader = csv::Reader::from_path(&csv_path).expect("read structured csv");
    let headers = reader.byte_headers().expect("csv headers").clone();
    let column = |name: &str| {
        headers
            .iter()
            .position(|h| h == name.as_bytes())
            .unwrap_or_else(|| panic!("{system}: no {name} column"))
    };
    let (line_id, content, template) =
        (column("LineId"), column("Content"), column("EventTemplate"));

    let mut contents = Vec::with_capacity(lines.len());
    let mut truth = Vec::with_capacity(lines.len());
    for (row, record) in reader.byte_records().enumerate() {
        let record = record.expect("csv record");
        let field = |i: usize| String::from_utf8_lossy(&record[i]).into_owned();
        assert_eq!(
            field(line_id),
            (row + 1).to_string(),
            "{system}: LineId out of step at row {row}"
        );
        contents.push(field(content));
        truth.push(field(template));
    }
    assert_eq!(
        contents.len(),
        lines.len(),
        "{system}: csv rows != log lines"
    );
    Sample {
        lines,
        contents,
        truth,
    }
}

/// What ingest clusters for one verbatim line: the body of the event the
/// syslog listener would build from it.
fn ingest_body(line: &str) -> String {
    let peer = "127.0.0.1:514".parse().expect("peer");
    oarfish_ingest::frame_to_event(line.as_bytes(), &peer, time::OffsetDateTime::UNIX_EPOCH)
        .body_lossy()
        .into_owned()
}

/// Mask and train each input through a fresh Drain; return the cluster of
/// each input, plus the Drain for template lookups. Drain clusters never
/// merge after the fact, so the cluster a line trained into is its final one.
fn cluster(inputs: &[String]) -> (Drain, Vec<u64>) {
    let bundle = oarfish_mask::curated();
    let mut drain = Drain::new(Config::default()).expect("default config is valid");
    let seqs = inputs
        .iter()
        .map(|input| drain.train(bundle.mask(input).template()).seq)
        .collect();
    (drain, seqs)
}

/// loghub-2.0's GA and FGA. A ground-truth group scores when every line in it
/// lands in one port cluster and that cluster holds nothing else.
fn grouping_accuracy(truth: &[String], predicted: &[u64]) -> (f64, f64) {
    let mut predicted_size: HashMap<u64, usize> = HashMap::new();
    for seq in predicted {
        *predicted_size.entry(*seq).or_default() += 1;
    }
    let mut groups: HashMap<&str, Vec<u64>> = HashMap::new();
    for (template, seq) in truth.iter().zip(predicted) {
        groups.entry(template).or_default().push(*seq);
    }

    let (mut accurate_lines, mut accurate_groups) = (0, 0);
    for seqs in groups.values() {
        let distinct: HashSet<u64> = seqs.iter().copied().collect();
        if let [only] = distinct.into_iter().collect::<Vec<_>>()[..]
            && predicted_size[&only] == seqs.len()
        {
            accurate_lines += seqs.len();
            accurate_groups += 1;
        }
    }

    let ga = accurate_lines as f64 / truth.len() as f64;
    let precision = accurate_groups as f64 / predicted_size.len() as f64;
    let recall = accurate_groups as f64 / groups.len() as f64;
    let fga = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    (ga, fga)
}

/// One port cluster holding lines from more than one ground-truth group.
struct Merge {
    lines: usize,
    port: String,
    /// `(truth template, lines)`, biggest first.
    parts: Vec<(String, usize)>,
}

/// Every merge in a trained Drain, biggest first.
fn merges(drain: &Drain, truth: &[String], predicted: &[u64]) -> Vec<Merge> {
    let mut by_seq: HashMap<u64, BTreeMap<&str, usize>> = HashMap::new();
    for (template, seq) in truth.iter().zip(predicted) {
        *by_seq.entry(*seq).or_default().entry(template).or_default() += 1;
    }
    let mut merged: Vec<_> = by_seq
        .into_iter()
        .filter(|(_, templates)| templates.len() > 1)
        .map(|(seq, templates)| {
            let port = drain
                .get(seq)
                .map_or("?", |c| c.template.as_str())
                .to_owned();
            let mut parts: Vec<(String, usize)> = templates
                .into_iter()
                .map(|(t, n)| (t.to_owned(), n))
                .collect();
            parts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            Merge {
                lines: parts.iter().map(|(_, n)| n).sum(),
                port,
                parts,
            }
        })
        .collect();
    merged.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.port.cmp(&b.port)));
    merged
}

fn distinct<T: Eq + std::hash::Hash>(items: impl IntoIterator<Item = T>) -> usize {
    items.into_iter().collect::<HashSet<_>>().len()
}

#[test]
fn the_port_scores_against_loghub2_ground_truth() {
    let Some(dir) = corpus_dir() else {
        eprintln!("skipping: corpus/loghub-2.0 is absent; run scripts/fetch-corpus.sh");
        return;
    };

    let mut summary = String::from(
        "system        truth  content: clusters  GA     FGA   |  line: clusters  GA     FGA   merges |  published GA     FGA\n",
    );
    for &(system, published_ga, published_fga) in PUBLISHED {
        let sample = load(&dir, system);
        let truth_groups = distinct(sample.truth.iter());

        let (_, content_seqs) = cluster(&sample.contents);
        let (content_ga, content_fga) = grouping_accuracy(&sample.truth, &content_seqs);

        let bodies: Vec<String> = sample.lines.iter().map(|l| ingest_body(l)).collect();
        let (line_drain, line_seqs) = cluster(&bodies);
        let (line_ga, line_fga) = grouping_accuracy(&sample.truth, &line_seqs);
        let line_merges = merges(&line_drain, &sample.truth, &line_seqs);

        writeln!(
            summary,
            "{system:<12}  {truth_groups:>5}  {:>17}  {content_ga:.3}  {content_fga:.3} | {:>14}  {line_ga:.3}  {line_fga:.3}  {:>6} | {published_ga:>12.3}  {published_fga:.3}",
            distinct(content_seqs.iter()),
            distinct(line_seqs.iter()),
            line_merges.len(),
        )
        .expect("write to string");

        let mut detail = format!(
            "line mode: {} port clusters mix ground-truth groups ({} lines)\n",
            line_merges.len(),
            line_merges.iter().map(|m| m.lines).sum::<usize>(),
        );
        for merge in line_merges.iter().take(MERGES_SHOWN) {
            writeln!(detail, "\n{} {}", merge.lines, merge.port).expect("write to string");
            for (template, n) in &merge.parts {
                writeln!(detail, "  {n:>4} {template}").expect("write to string");
            }
        }
        insta::with_settings!({ snapshot_suffix => system, omit_expression => true }, {
            insta::assert_snapshot!("merges", detail);
        });
    }
    insta::with_settings!({ omit_expression => true }, {
        insta::assert_snapshot!("summary", summary);
    });
}

/// The metric port against hand-worked cases, so a score is never wrong
/// because the arithmetic is.
#[test]
fn grouping_accuracy_matches_the_loghub_definition() {
    let t = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();

    // Perfect grouping, whatever the ids are called.
    assert_eq!(
        grouping_accuracy(&t(&["a", "a", "b"]), &[7, 7, 9]),
        (1.0, 1.0)
    );

    // A split: group "a" spans two clusters, so only "b"'s line scores.
    // GA 1/3; precision 1/3 clusters, recall 1/2 groups, F1 0.4.
    let (ga, fga) = grouping_accuracy(&t(&["a", "a", "b"]), &[1, 2, 3]);
    assert!((ga - 1.0 / 3.0).abs() < 1e-9 && (fga - 0.4).abs() < 1e-9);

    // A merge: one cluster holds both groups, so neither scores.
    assert_eq!(
        grouping_accuracy(&t(&["a", "a", "b"]), &[1, 1, 1]),
        (0.0, 0.0)
    );
}
