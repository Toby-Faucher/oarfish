#!/usr/bin/env bash
# Fetch the third-party corpora listed in corpus/manifest.toml.
#
# These are not committed: see the licence note at the top of the manifest.
# Nothing in CI needs this. The committed corpus under
# crates/oarfish-mask/tests/corpus/ is what keeps the tree green offline.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$root/corpus/manifest.toml"

python3 - "$manifest" "$root/corpus" <<'PY'
import hashlib, pathlib, sys, tomllib, urllib.request

manifest, dest_root = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
entries = tomllib.loads(manifest.read_text())["file"]

for entry in entries:
    dest = dest_root / entry["path"]
    dest.parent.mkdir(parents=True, exist_ok=True)

    if dest.exists() and hashlib.sha256(dest.read_bytes()).hexdigest() == entry["sha256"]:
        print(f"have  {entry['path']}")
        continue

    print(f"fetch {entry['path']}")
    with urllib.request.urlopen(entry["url"], timeout=60) as response:
        body = response.read()

    got = hashlib.sha256(body).hexdigest()
    if got != entry["sha256"]:
        sys.exit(
            f"digest mismatch for {entry['path']}\n"
            f"  expected {entry['sha256']}\n"
            f"  got      {got}\n"
            "The upstream file changed. Review it, then update the manifest deliberately."
        )
    dest.write_bytes(body)

print(f"\ncorpus ready in {dest_root}")
PY
