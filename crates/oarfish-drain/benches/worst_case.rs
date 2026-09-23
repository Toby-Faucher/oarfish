//! The worst case one line can cost: a leaf packed with clusters, all of the
//! line's length, none similar enough to merge.
//!
//! `line_cost_bounded` in `lean/OarfishDrain/Theorems.lean` proves one search
//! makes at most `max_clusters × max_tokens` token comparisons. This measures
//! what that ceiling costs in time. Every line here shares its first six
//! tokens, so all of them take one tree path to one leaf, and the rest differ,
//! so no two ever reach 0.90. Each measured train then scans the whole leaf.
//!
//! Filling a leaf is itself quadratic (each fill line scans the leaf so far),
//! so the sizes stop well short of the 65,536 default cap. Time is linear in
//! both axes, as the bound says, and the larger sizes follow from the slope.
//!
//! Run on its own: `cargo bench -p oarfish-drain --bench worst_case`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use oarfish_drain::{Config, Drain};

/// One hostile line: a fixed six-token prefix, then `len - 6` tokens unique
/// to line `i`.
fn line(i: usize, len: usize) -> String {
    let mut tokens = vec!["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    tokens.extend((6..len).map(|j| format!("v{i}x{j}")));
    tokens.join(" ")
}

/// A Drain fed `lines` hostile lines under `leaf_cap` (0: uncapped).
fn packed(lines: usize, len: usize, leaf_cap: usize) -> Drain {
    let mut drain = Drain::new(Config {
        max_leaf_clusters: leaf_cap,
        ..Config::default()
    })
    .expect("valid");
    for i in 0..lines {
        let assignment = drain.train(&line(i, len));
        assert_eq!(assignment.size, 1, "fill lines must not merge");
    }
    drain
}

fn bench_worst_case(c: &mut Criterion) {
    // Uncapped: one line's cost grows with the leaf, as `line_cost_bounded`
    // allows. A repeat of the first fill line joins, so the table does not
    // grow between iterations, but search still scores the whole leaf.
    let mut group = c.benchmark_group("uncapped");
    group.sample_size(20);
    for &len in &[16usize, 128] {
        for &clusters in &[256usize, 1024, 4096] {
            let mut drain = packed(clusters, len, 0);
            let repeat = line(0, len);
            group.bench_with_input(
                BenchmarkId::new(format!("tokens_{len}"), clusters),
                &repeat,
                |b, repeat| b.iter(|| drain.train(std::hint::black_box(repeat))),
            );
        }
    }
    group.finish();

    // Capped: the hostile stream itself, every line new, after 4,096 lines
    // have already filled the leaf. Each pays a full-leaf scan, an insert and
    // a leaf eviction, and the cost stays flat however long the stream runs.
    let mut group = c.benchmark_group("capped");
    group.sample_size(20);
    for &len in &[16usize, 128] {
        for &cap in &[32usize, 64, 128, 256] {
            let mut drain = packed(4096, len, cap);
            let mut next = 4096;
            group.bench_function(BenchmarkId::new(format!("tokens_{len}"), cap), |b| {
                b.iter(|| {
                    next += 1;
                    drain.train(std::hint::black_box(&line(next, len)))
                })
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_worst_case);
criterion_main!(benches);
