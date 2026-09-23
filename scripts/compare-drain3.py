#!/usr/bin/env python3
"""Compare the Drain port with Python drain3 on loghub-2.0's 2k samples.

    OARFISH_DRAIN3_EXPORT=$PWD/target/drain3 \
        cargo test -p oarfish-drain --test loghub2 -- --ignored export
    uv run --with drain3 scripts/compare-drain3.py target/drain3

The export holds what the port clustered (the masked syslog body, exactly as
the daemon sees it) and which cluster each line landed in. drain3 runs on the
same masked input at the port's settings, and at its own defaults on both the
masked input and the raw line, the way most people run it. Scores are
loghub-2.0's GA and merges: clusters mixing more than one ground-truth
template, the failure that hides an alarm.
"""

import collections
import pathlib
import sys

from drain3.drain import Drain

SETUPS = {
    "drain3 @ port settings": {"sim_th": 0.9, "depth": 8, "raw": False},
    "drain3 defaults, masked": {"sim_th": 0.4, "depth": 4, "raw": False},
    "drain3 defaults, raw line": {"sim_th": 0.4, "depth": 4, "raw": True},
}


def grouping_accuracy(truth, predicted):
    size = collections.Counter(predicted)
    groups = collections.defaultdict(list)
    for t, p in zip(truth, predicted):
        groups[t].append(p)
    accurate = sum(
        len(ps)
        for ps in groups.values()
        if len(set(ps)) == 1 and size[ps[0]] == len(ps)
    )
    return accurate / len(truth)


def merges(truth, predicted):
    mixed = collections.defaultdict(set)
    for t, p in zip(truth, predicted):
        mixed[p].add(t)
    return sum(1 for ts in mixed.values() if len(ts) > 1)


def same_groups(a, b):
    """Share of lines whose cluster has exactly the same members in both runs."""
    members_a, members_b = collections.defaultdict(list), collections.defaultdict(list)
    for i, (x, y) in enumerate(zip(a, b)):
        members_a[x].append(i)
        members_b[y].append(i)
    return sum(members_a[x] == members_b[y] for x, y in zip(a, b)) / len(a)


def drain3(lines, sim_th, depth):
    miner = Drain(sim_th=sim_th, depth=depth, max_children=100, max_clusters=None)
    return [miner.add_log_message(line)[0].cluster_id for line in lines]


def main(export_dir):
    files = sorted(pathlib.Path(export_dir).glob("*.tsv"))
    if not files:
        sys.exit(f"no .tsv files in {export_dir}; run the export test first")
    columns = ["port", *SETUPS]
    totals = {c: [0.0, 0, 0.0] for c in columns}
    print("| system | " + " | ".join(columns) + " |")
    print("|---" * (len(columns) + 1) + "|")
    for path in files:
        rows = [line.rstrip("\n").split("\t") for line in path.open(encoding="utf-8")]
        truth = [r[0] for r in rows]
        port = [int(r[1]) for r in rows]
        runs = {"port": port}
        for name, setup in SETUPS.items():
            inputs = [r[3] if setup["raw"] else r[2] for r in rows]
            runs[name] = drain3(inputs, setup["sim_th"], setup["depth"])
        cells = []
        for name in columns:
            ga, m, same = (
                grouping_accuracy(truth, runs[name]),
                merges(truth, runs[name]),
                same_groups(port, runs[name]),
            )
            totals[name][0] += ga
            totals[name][1] += m
            totals[name][2] += same
            cells.append(
                f"{ga:.3f} / {m}" + ("" if name == "port" else f" / {same:.0%}")
            )
        print(f"| {path.stem} | " + " | ".join(cells) + " |")
    n = len(files)
    cells = [
        f"**{g / n:.3f} / {m}**" + ("" if name == "port" else f" / {s / n:.0%}")
        for name, (g, m, s) in totals.items()
    ]
    print("| **average / total** | " + " | ".join(cells) + " |")
    print("\ncells: GA / merges / same groups as the port")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "target/drain3")
