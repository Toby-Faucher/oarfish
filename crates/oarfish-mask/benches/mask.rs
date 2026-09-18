//! The every-line path, measured.
//!
//! "Fast" without a regression guard is a claim, not a property. A runaway
//! container emitting 100k lines/sec is a normal homelab failure, so the
//! throughput number here is a real operating constraint.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use oarfish_mask::curated;

const LINES: &[(&str, &str)] = &[
    ("plain", "systemd: Reached target Multi-User System"),
    (
        "kernel",
        "EXT4-fs error (device sda1): ext4_find_entry:1455: inode #98304: comm ls: reading directory lblock 0",
    ),
    (
        "sshd",
        "sshd[12346]: Failed password for invalid user admin from 192.168.1.44 port 54321 ssh2",
    ),
    (
        "nginx",
        "10.0.0.9 - - [18/Sep/2026:03:14:07 +0000] \"GET /api/alarms HTTP/1.1\" 200 4523 \"-\" \"curl/8.5.0\"",
    ),
];

fn bench_mask(c: &mut Criterion) {
    let bundle = curated();
    let mut group = c.benchmark_group("mask");

    for (name, line) in LINES {
        group.throughput(Throughput::Bytes(line.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(name), line, |b, line| {
            b.iter(|| bundle.mask(std::hint::black_box(line)));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_mask);
criterion_main!(benches);
