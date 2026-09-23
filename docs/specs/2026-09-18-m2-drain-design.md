# M2 — `oarfish-drain`, a Drain port over masked lines

**Status:** accepted, pre-implementation
**Date:** 2026-09-18
**Parent:** `docs/specs/2026-09-17-oarfish-design.md` — milestone M2 in §11, mechanism in §5.4

---

## 1. What this milestone is for

§11 gives M2 one contract: *templates match drain3 over the same corpus.* That sentence
needs unpacking, because "match" cannot mean byte-identical output.

drain3 ran on raw lines. Oarfish runs Drain on *masked* lines, which is the entire point
of M1: masking already separated variables from constants, and better than Drain's own
"a token containing a digit is a variable" heuristic ever could. So the port's templates
contain `<VAR:IP4>` where drain3's contain `<*>` or a literal, and the strings will never
be equal. "Match" therefore means *grouping agreement*: lines drain3 puts together, the
port puts together, and — the safety direction — lines drain3 keeps apart, the port keeps
apart. Merging `task succeeded` into `task failed` silently deletes an alarm; splitting
one event into two costs a re-verdict and a merge review. The validation in §6 is built
around that asymmetry: impurity is a failure, over-splitting is a report.

## 2. Boundaries

**M2 produces** cluster assignment for masked lines: a stable `TemplateId` per cluster,
the cluster's template text, and its size. Nothing else.

**M2 takes masked text as `&str`.** Like M1 takes `&str` until ingest exists, the
clusterer takes the masked template string, not `Masked` and not `Event`. Tokenization
is `split_whitespace`: placeholders contain no spaces, so masking is token-count
preserving in the common case, which is what makes Drain's first-level token-count
grouping work well on this input. Depending on `oarfish-mask` here would buy nothing
and cost the boundary M1 just drew.

**M2 does not aggregate `Slot`s.** `core::Slot` carries `seen: u64` across a cluster's
history, and building it needs the bundle's verbatim patterns, which the clusterer
deliberately cannot see (it takes `&str`). Whoever accumulates per-line matches into
board-facing aggregates owns the counting — that is the store or the engine, not the
every-line path. The clusterer holds no per-slot state.

**M2 does not persist.** Clusters live in memory. The verdict store in M4 is what makes
a template judgment survive a restart; M2's table is rebuilt from the stream.

## 3. What the port takes from Loki, and what it leaves behind

Ported from Grafana Loki's `pkg/pattern/drain` (MIT, © 2022 faceair), not from drain3.
The parent spec already decided this: Loki's version ships the operational piece that
matters, a bounded cluster table. The port is faithful where Loki is algorithm and
divergent where Loki is deployment. Concretely:

**Taken verbatim** (same structure, same decisions, translated to Rust):

- The prefix tree: first level keyed by token count, then one level per leading token
  down to `max_node_depth = depth - 2`, exact-token child first with fallback to the
  parameter child. Missing path means no candidate, not a scan.
- Numeric-token routing in the tree: a token containing a digit descends the parameter
  path when one exists. This only shapes the search structure — which leaf's candidates
  get compared — never the verdict. Similarity decides. Masked placeholders containing
  digits (`<VAR:IP4>`, `<VAR:NUM>`, `<VAR:IP6>`) route parameter-ward, which is what you
  want: they are the variable positions.
- Similarity: equal tokens over total tokens, parameter positions in the cluster
  template skipped rather than counted. Best candidate wins ties by parameter count;
  it must reach the threshold.
- Template update: positions where the line and the cluster template differ become the
  parameter string, in place. Generalization is monotonic — a position generalizes at
  most once — which is what bounds the id churn in §5.
- Stale-id hygiene: eviction takes the id out of its tree leaf, found by the path
  recorded at insert, and prunes nodes it leaves empty, so the tree holds exactly the
  live clusters. *(Revised 2026-09-22. This said "filtered on read and cleaned on
  insert", but nothing cleaned them, and leaves grew without bound under the hostile
  input `max_clusters` exists to bound.)*

One mechanics note, verified while porting: a word-varying early position (e.g. two
usernames as token 2) never meets in the tree — the search follows exact-or-parameter
paths, and no parameter node exists until a digit-bearing token paves one. Numbers
pave parameter paths; words follow. With masked input this means unmasked variables in
early positions (usernames, hostnames) split rather than merge. That is the
conservative direction the threshold already chose, and merge review in M5 is what
repairs it — not a shallower tree.

**Deliberately not ported:**

- The tokenizer. Loki splits on punctuation and tracks constant-vs-variable token state
  per format (JSON, logfmt, plain). Oarfish has no formats at this layer: masking already
  did the separation, on purpose, in M1. Whitespace splitting is the whole tokenizer.
  This is a deletion of hundreds of lines, and it is the point of the pipeline order.
- Numeric pre-parameterization. drain3's miner replaces digit-bearing tokens with `<*>`
  when opening a cluster. Kept, that would shred `<VAR:IP4>` into `<*>` on arrival and
  destroy every typed slot M1 produced. A new cluster's template is the masked line
  verbatim. This is the single most important divergence, and it is why masking runs
  *before* clustering rather than inside it.
- Minimum-token guards. Loki declines lines under 4 tokens (and over 80, and over 3000
  bytes) with a metric. Oarfish clusters every line: a dropped line never reaches a
  window, and a silent hole in the every-line path is worse than a one-token cluster.
  The 3-token `task <VAR:NUM> succeeded` / `failed` pair from the parent spec clusters
  correctly, which a 4-token floor would have refused.
- Metrics, chunks, samples, pruning and eviction-ratio throttling. Loki's Drain is an
  instrument inside a distributed ingester; oarfish's is a plain struct. The one
  operational piece kept is the bounded table (§4).
- The marked-token exactness mechanism (Loki's leading-`0x00`-byte constants). That is
  tokenizer state, and there is no tokenizer here to produce it.

**License note.** Loki is MIT, like oarfish. The port carries an attribution comment
naming the source file. No code is vendored; it is rewritten in Rust against this
spec.

## 4. Parameters

| Parameter | Value | Why |
|---|---|---|
| `similarity` | 0.90 | The parent spec (§5.4), non-negotiable this milestone. Over-splits, never merges. |
| `depth` | 8 | Deeper than drain3's 4 because masked leading tokens are stable constants — M1 removed the timestamps and counters that made raw prefixes unreliable, so prefix paths can be trusted further. Shallower than Loki's 30 because there is no punctuation-tokenizer state distinguishing constants from variables mid-line. Tunable; the snapshots are the guard. |
| `max_children` | 100 | drain3's default. The fan-out cap before a level collapses to the parameter node. Rarely reached at homelab scale; present so hostile input cannot blow up the tree. |
| `max_clusters` | 65_536 | The Loki rationale: bound the table. Sized above the "few thousand templates" steady state from §2 of the parent spec, with LRU eviction past it. Eviction forgets the cluster; later matching lines open a new one (new id, re-verdict — bounded, and only under explosion). |
| `param` | `<*>` | drain3's convention, deliberately distinct from `<VAR:NAME>`: a Drain wildcard is untyped, a mask slot is typed, and the board renders them differently. |
| `max_tokens` | 128 | Lines past this are clustered on their first 128 tokens. Bounds per-cluster memory against hostile input while keeping the function total. Normal lines never notice. |

`depth < 3` is rejected at construction (the tree needs a count level and at least one
token level), mirroring Loki's panic as a `thiserror` `DrainError`. Everything else is a
plain value, documented where it is declared.

## 5. Identity: what gets hashed, and the churn youth

`TemplateId::of` is called on the cluster's template text (tokens joined with single
spaces), exactly as M1 §2 prescribed: on the cluster's representative, not on every
line. Whitespace runs are normalized as a consequence of tokenization; two lines that
differ only in spacing share a cluster, which is correct.

The honest caveat: cluster templates evolve. Each new variant generalizes the positions
where it differs, so a young cluster's id moves — at most once per position, then it
settles. The verdict cache in M4 absorbs this as ordinary new-template events, which is
what the per-new-template pricing already covers. What never happens is the reverse: an
established cluster's id never changes under a line it already matches, so a judged
template stays judged. Stability where it matters (the judged past), churn where it is
priced (the unseen youth).

The crate also hands out the internal `u64` sequence per cluster for tree bookkeeping,
but that number is never persisted and never leaves the crate: restart it and the same
stream gets different sequence numbers. Only `TemplateId` is stable across restarts.

## 6. Validation: what "matches drain3" means, operationally

The Loghub `_templates.csv` files fetched in M1 are Drain-derived ground truth: each
carries `EventId` plus the template with `<*>` wildcards. The harness maps every corpus
line to its ground-truth event (by matching the raw line against the templates as
regexes, reporting ambiguous and unmatched lines rather than guessing) and trains the
port over the masked lines, then reports per file:

- template count: ground truth vs port (expect more port clusters — §1's asymmetry plus
  M1's unmasked usernames/hostnames, which split what truth merges);
- impurity: port clusters containing lines of more than one ground-truth event. **This
  is the failure gate, with reviewed exceptions.** Single-token differences in lines
  of ten or more tokens merge at 0.90 by definition (12/13, 9/10), and the fetched
  corpora contain three such pairs (a username, a cache kind, a driver name — all
  same-verdict merges where the detail survives in the raw lines). Those pairs are
  pinned in the harness; anything outside the list fails. Raising the threshold to
  split them is rejected: the parent spec fixes 0.90, and a threshold that never
  merges single-token differences in long lines never generalizes at all;
- completeness: ground-truth events split across port clusters, reported per event for
  review, never asserted — over-splitting is the designed direction;
- the full cluster list (template, size, one example line) as an insta snapshot, so a
  parameter change shows exactly which templates moved, exactly like M1.

On top of the review sits one hard unit test, using the parent spec's own example:
`task <VAR:NUM> succeeded` and `task <VAR:NUM> failed` must never share a cluster.
That pair is the reason the threshold is 0.90, so it is asserted, not assumed.
A threshold-boundary test pins the arithmetic (9/10 merges, 8/10 splits).

The committed corpus gets the same train-and-snapshot treatment without ground truth:
reviewed templates, plus an idempotence-of-assignment check (same lines twice, same
clusters).

## 7. Testing

- `insta` snapshots: committed-corpus cluster lists, fetched-tier comparison reports.
  Tuning depth or the threshold shows exactly which templates moved.
- `rstest` tables: similarity boundaries, tree-search basics (length groups, prefix
  sharing, parameter fallback), eviction, truncation.
- `proptest`: assignment determinism over generated token sequences; repeated identical
  lines never move a settled id; template ids are stable for lines a cluster already
  matches (the §5 promise that matters).
- `criterion` on `train`: the every-line path, and `CLAUDE.md` already lists
  `cargo bench -p oarfish-drain` — M2 is the milestone that makes it real.
- `oarfish-mask` as a dev-dependency: the harness masks the corpus through `curated()`
  before training. The library itself keeps depending only on `oarfish-core`.

## 8. Deferred, deliberately

| Deferred | Lands in | Why not now |
|---|---|---|
| `Slot.seen` accumulation | Whoever aggregates (store/engine) | Needs bundle patterns the clusterer cannot see; counting belongs to the accumulator, not the every-line path. |
| Merge review of over-splits | M5 (`oarfish-engine`) | M2's job is to split safely; repair is the engine's, per §5.6. |
| Persistence of clusters | M4 (`oarfish-store`) | The table rebuilds from the stream; what survives a restart is verdicts, not clusters. |
| Anything touching `Event` | M3 | Input stays `&str` until `oarfish-ingest` exists. |
| Running drain3 itself in CI | Never | Version-sensitive Python in CI for a comparison the ground-truth files already encode. The CSVs are the comparison. |

## 9. Changes to existing documents

- `crates/oarfish-drain/src/lib.rs`: the stub doc comment already describes this design;
  keep it accurate (attribution to Loki's file, parameter table pointer).
- `CLAUDE.md` commands: `cargo bench -p oarfish-drain` is already listed — no change.
  If the bench shows the every-line path needs a note (e.g. throughput vs the 100k
  lines/sec bar), record it in the commit message, not the docs.
