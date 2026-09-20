#!/usr/bin/env python3
"""Seed the oarfish daemon with log lines so the board has something to chew on.

Replays a log file (e.g. LogPAI/LogHub) or generates synthetic bursts, wrapped
in minimal RFC 3164 envelopes and sent over syslog TCP (default) or UDP to the
daemon's ingest listener (default 127.0.0.1:514).

Examples:
    scripts/seed.py --synthetic ext4 --count 40 --rate 20
    scripts/seed.py --file HDFS.log --rate 50 --repeat 2
    scripts/seed.py --synthetic mixed --count 200 --seed 7 --dry-run

Only the standard library is used.
"""

import argparse
import random
import socket
import sys
import time
from datetime import datetime, timezone

PRIORITY = 13  # user.notice


def envelope(line: str, host: str) -> bytes:
    """Wrap a raw line in a minimal RFC 3164 envelope; the daemon reads the message part."""
    stamp = datetime.now(timezone.utc).strftime("%b %d %H:%M:%S")
    return f"<{PRIORITY}>{stamp} {host} oarfish-seed: {line}\n".encode("utf-8", "replace")


def synthetic_ext4(rng: random.Random, count: int):
    """A failing disk: EXT4 errors across devices, inodes and processes."""
    devs = ["sda1", "sda1", "sda1", "nvme0n1p2"]
    procs = ["find", "updatedb", "rsync", "scrub"]
    for _ in range(count):
        yield (
            f"kernel: EXT4-fs error (device {rng.choice(devs)}): "
            f"ext4_find_entry:{rng.randint(1200, 1400)}: "
            f"inode #{rng.randint(40000, 60000)}: "
            f"comm {rng.choice(procs)}: "
            f"reading directory lblock {rng.randint(1, 200)}"
        )


def synthetic_routine(rng: random.Random, count: int):
    """Normal operational noise: cron, ssh, systemd heartbeats."""
    jobs = ["backup", "scrub", "sync", "prune"]
    users = ["toby", "root", "deploy"]
    for _ in range(count):
        kind = rng.random()
        if kind < 0.4:
            yield f"CRON[1234]: ({users[0]}) CMD ( /usr/local/bin/{rng.choice(jobs)}.sh )"
        elif kind < 0.7:
            yield (
                f"sshd[5678]: Accepted publickey for {rng.choice(users)} "
                f"from 192.168.1.{rng.randint(2, 50)} port {rng.randint(1024, 65000)} ssh2"
            )
        else:
            yield f"systemd[1]: Started Daily {rng.choice(jobs)} job."


def synthetic_mixed(rng: random.Random, count: int):
    """Mostly noise with one failing disk buried in it, like a real night."""
    ext4_at = rng.sample(range(count), k=max(1, count // 10))
    routine = synthetic_routine(rng, count)
    ext4 = synthetic_ext4(rng, len(ext4_at))
    ext4_set = set(ext4_at)
    for i in range(count):
        if i in ext4_set:
            yield next(ext4)
        else:
            yield next(routine)


def file_lines(path: str, repeat: int):
    for _ in range(repeat):
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            for line in fh:
                line = line.rstrip("\n")
                if line:
                    yield line


def send_tcp(host: str, port: int, payloads: list[bytes]) -> None:
    with socket.create_connection((host, port)) as sock:
        for payload in payloads:
            sock.sendall(payload)


def send_udp(host: str, port: int, payloads: list[bytes]) -> None:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        for payload in payloads:
            sock.sendto(payload, (host, port))
    finally:
        sock.close()


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--file", help="log file to replay, one line per message")
    source.add_argument(
        "--synthetic",
        choices=["ext4", "routine", "mixed"],
        help="generate lines instead of reading a file",
    )
    parser.add_argument("--count", type=int, default=100, help="synthetic lines to generate")
    parser.add_argument("--repeat", type=int, default=1, help="file replay passes")
    parser.add_argument("--seed", type=int, default=None, help="RNG seed for reproducible runs")
    parser.add_argument("--host", default="seedbox", help="hostname stamped in the envelope")
    parser.add_argument("--target", default="127.0.0.1", help="daemon syslog address")
    parser.add_argument("--port", type=int, default=514, help="daemon syslog port")
    parser.add_argument("--proto", choices=["tcp", "udp"], default="tcp")
    parser.add_argument("--rate", type=float, default=20.0, help="lines per second, 0 for flat out")
    parser.add_argument("--dry-run", action="store_true", help="print lines instead of sending")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    rng = random.Random(args.seed)

    if args.file:
        lines = file_lines(args.file, args.repeat)
    else:
        gen = {"ext4": synthetic_ext4, "routine": synthetic_routine, "mixed": synthetic_mixed}[
            args.synthetic
        ]
        lines = gen(rng, args.count)

    interval = 1.0 / args.rate if args.rate > 0 else 0.0
    batch: list[bytes] = []
    sent = 0
    for line in lines:
        if args.dry_run:
            print(line)
            sent += 1
            continue
        batch.append(envelope(line, args.host))
        sent += 1
        if interval:
            time.sleep(interval)

    if not args.dry_run:
        if args.proto == "tcp":
            send_tcp(args.target, args.port, batch)
        else:
            send_udp(args.target, args.port, batch)
        print(f"sent {sent} lines to {args.target}:{args.port}/{args.proto}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
