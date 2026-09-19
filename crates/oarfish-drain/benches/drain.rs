//! The every-line path, measured after warmup.
//!
//! A fresh Drain measures tree growth; a trained one measures the steady state
//! the daemon lives in. Bench the latter: pre-train over the committed corpus,
//! then time individual trains.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use oarfish_drain::{Config, Drain};

fn trained() -> Drain {
    let bundle = oarfish_mask::curated();
    let mut drain = Drain::new(Config::default()).expect("valid");
    for file in ["kernel", "sshd", "nginx", "storage"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../oarfish-mask/tests/corpus/{file}.log"));
        let contents = std::fs::read_to_string(path).expect("corpus");
        for line in contents.lines().filter(|l| !l.trim().is_empty()) {
            drain.train(bundle.mask(line).template());
        }
    }
    drain
}

const CASES: &[(&str, &str)] = &[
    // Hits an existing cluster: search plus an in-place update.
    (
        "match",
        "sshd[12346]: Failed password for invalid user admin from 192.168.1.44 port 54321 ssh2",
    ),
    // Same shape, new values: search plus generalization.
    (
        "generalize",
        "sshd[99999]: Failed password for invalid user root from 10.9.9.9 port 2222 ssh2",
    ),
];

fn bench_train(c: &mut Criterion) {
    // Mask once outside the loop: the bench measures Drain, not the masker
    // (which has its own bench in oarfish-mask).
    let bundle = oarfish_mask::curated();
    let masked: Vec<(&str, String)> = CASES
        .iter()
        .map(|(name, line)| (*name, bundle.mask(line).template().to_owned()))
        .collect();

    let mut group = c.benchmark_group("train");
    for (name, line) in &masked[..2] {
        group.bench_with_input(BenchmarkId::from_parameter(name), line, |b, line| {
            let mut drain = trained();
            b.iter(|| drain.train(std::hint::black_box(line)));
        });
    }
    // Creation, every iteration: a unique line that never matches, so each
    // pass pays tree insert plus table insert. This is the hostile-input
    // path the cluster cap exists for.
    group.bench_function(BenchmarkId::from_parameter("new"), |b| {
        let mut drain = trained();
        let mut n = 0u64;
        b.iter(|| {
            n += 1;
            let line = format!("kernel: never-before-seen subsystem {n} reports code xyzzy");
            drain.train(std::hint::black_box(&line))
        });
    });
    group.finish();
}

criterion_group!(benches, bench_train);
criterion_main!(benches);
