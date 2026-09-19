# Mask synthesis — `oarfish masks synthesize`

**Status:** designed, on branch `mask-synthesis`
**Date:** 2026-09-19
**Parent:** `docs/specs/2026-09-17-oarfish-design.md` — the cost-model table in §2 ("Once, at
install | LLM synthesizes the regex mask bundle | `bundle_hash`") and open question #1 in
§12 ("which model synthesizes masks... the prompt and its validation need designing")

---

## 1. What this is for

Every milestone doc from M1 onward has deferred the same line: *"`oarfish masks
synthesize` | After M6 | not on the critical path."* M6's critical path (the live
board) is done; ntfy delivery is deliberately parked until it's wanted. This is the
next item the project's own roadmap actually names.

The curated bundle (`crates/oarfish-mask/bundle/default.toml`) covers the general
case — timestamps, UUIDs, IPs, paths, sizes — and every milestone through M6 has run
on it alone. What it cannot cover is what's specific to one operator's homelab: an
internal job-id format, a proprietary app's log shape, a hostname convention. Mask
synthesis exists to find that gap and close it, once, at install, with one LLM call —
never on a path that runs more than that.

## 2. Boundaries

| In | Out |
|---|---|
| `oarfish masks synthesize --corpus <path>` | Runtime/automatic synthesis during ingest |
| `--bundle <path>` daemon flag, loading a non-curated bundle | Automatic re-synthesis on drift |
| Candidate detection via a throwaway `Drain` pass | Editing the curated bundle itself |
| The `oarfish-synth` transport crate | A second LLM provider or local-model path |

**Augment, not replace.** The curated bundle's slots are hand-tuned and precedence-
ordered; nothing about them is homelab-specific. Synthesis's job is to find what
they *don't* cover and append slots for that, never to regenerate what already works
from a necessarily smaller sample than went into curating the originals.

## 3. Crate layout

| Crate | Gains |
|---|---|
| `oarfish-synth` (new) | The chat/completions transport. Zero workspace dependencies — it hands back a raw string, nothing more. |
| `oarfish-mask` | A `synth` module: candidate detection, prompt building, response parsing, merge. Cold-path only; the daemon's ingest loop never touches it. |
| `oarfish` (bin) | The `masks synthesize` subcommand, and a `--bundle <path>` flag for the daemon path. |

**Why a new crate for the transport, rather than extending `oarfish-jev`.**
`oarfish-jev` is documented as transport only for the decisions endpoint — its
`Question`/`Answer`/confidence types exist to carry calibrated answers, and
`chat/completions` rejects decisions models outright (§5.11 of the parent doc). A
generative call — free-form text in, free-form text out — is a different shape
entirely. Bolting it onto `oarfish-jev` would dilute exactly the thinness that crate
exists to protect. `oarfish-synth` mirrors `oarfish-jev`'s own conventions instead:
`Client::new(base_url, api_key, model)`, `Client::from_env()`, one method,
`thiserror`-based errors (`Transport`, a non-2xx status variant, `MissingApiKey`).

**The provider sits behind a trait, same as `Decide` everywhere else in this
codebase.** `oarfish-mask::synth` defines `Synthesize`, one method mirroring
`oarfish-synth::Client::synthesize`; `Client` implements it for production, a fake
implements it for tests. This is what lets the retry-on-`BundleError` loop and the
merge logic be tested with chosen responses and no network, while `wiremock` stays
scoped to what it's for elsewhere in this codebase: proving the transport itself,
not the business logic built on top of it.

**Why the candidate-detection logic lives in `oarfish-mask`, not a third crate.**
"What does this bundle fail to mask" is squarely a masking-coverage question — it's
`oarfish-mask`'s own domain, and it already owns `Bundle`, `SlotDef`, and the
reserved-name and precedence rules any merge has to respect. `oarfish-drain` is used
here purely as a tool to find that gap; it changes no clustering behavior and adds no
cost to the crate's hot path, because this code only ever runs from the CLI
subcommand. `oarfish` (the binary) already links both crates for the real pipeline,
so this adds no new dependency to what ships.

**Why the CLI keeps the daemon as its default, with `masks` as the one named
subcommand.** `oarfish --syslog ... --api ...` is the one command this project has
already documented, scripted, and put in a systemd unit. Making every invocation
require a name (`oarfish run ...`) would break all of that for a project that is
pre-alpha but already has those written. `clap` supports an optional
`#[command(subcommand)]` that falls through to the flattened daemon args when absent,
so this is additive:

```rust
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    daemon: DaemonArgs,
}

#[derive(Subcommand)]
enum Command {
    Masks { #[command(subcommand)] action: MasksAction },
}

#[derive(Subcommand)]
enum MasksAction {
    Synthesize {
        #[arg(long)] corpus: PathBuf,
        #[arg(long)] bundle: Option<PathBuf>,
        #[arg(long, default_value_t = 3)] min_occurrences: u64,
        #[arg(long, default_value_t = 50)] max_candidates: usize,
        #[arg(long)] out: PathBuf,
        #[arg(long)] model: String,
    },
}
```

`--model` has no default: this is a rare, deliberate, install-time command, and
guessing a specific model id here would assert a choice with no basis — this doc
takes no position on which model, per open question #1. The operator names one
explicitly; nothing about `oarfish-synth`'s transport constrains the choice beyond
"reachable through OpenRouter's `chat/completions`."

`None` for `command` runs the daemon exactly as today, `daemon` fields unchanged.
`--bundle` on `DaemonArgs` is new: when set, the daemon loads and parses that file
via `Bundle::parse` instead of `oarfish_mask::curated()`; when absent, behavior is
byte-for-byte what M1 through M6 have always run on. This closes a gap M1's own
design doc left open — *"loading a bundle from a config path | M4 or later"* — that
was never picked up, and mask synthesis cannot ship without it: a synthesized bundle
with no way to load it at runtime is a file nobody can use.

The `model` default above is a starting point, not a considered choice — swappable by
flag, same as every other threshold in this system that's a guess until there's data
behind it.

## 4. Finding candidates

Sending raw sampled log lines to an LLM and asking for regexes has no ground truth
behind it — it produces plausible patterns with no evidence they're right. Drain
already does the exact classification this needs: separating constant scaffold from
variable positions.

**The pipeline, run once per `synthesize` invocation:**

1. Mask the corpus with the starting bundle (curated, or `--bundle` if given — so a
   second run augments further rather than starting over).
2. Train a throwaway `Drain` instance over the masked output, at the same 0.90
   threshold the daemon runs.
3. For each cluster reaching `--min-occurrences`, find token positions `generalize()`
   turned into `<*>` that **are not already** a `<VAR:NAME>` placeholder. That is
   mechanical evidence of a variable slot the bundle doesn't know about — not a
   guess, and not something a raw-line sample could point at directly.
4. For each such position, collect its constant surrounding context (a few tokens
   either side) and a handful of the real varying values seen across the corpus.
5. Keep the `--max-candidates` highest-occurrence positions; drop the rest. This
   bounds both the prompt's size and its cost.

**The prompt** carries only these candidates — context plus example values per
position — never raw corpus lines. Far less noisy for the model, and far cheaper in
tokens than sampling whole lines and hoping the model finds the same positions
unaided.

**The response is JSON**, `[{name, pattern, why}]`, not TOML directly. LLMs are
reliable at JSON; TOML's escaping rules for a regex containing backslashes are
exactly the kind of thing that produces subtly-broken output if the model has to get
the quoting right unsupervised. The JSON is deserialized into `SlotDef`s and
serialized into the bundle format with the `toml` crate already in the workspace —
one place that owns TOML's escaping, not the model's own text generation.

## 5. Validation and merge

**Structural, mechanical, already exists.** `Bundle::parse` — unchanged — already
rejects a bad name, a reserved name, a duplicate name, a pattern that doesn't
compile, a pattern that smuggles a capturing group, and an alternation that exceeds
the size limit. Every synthesized slot goes through exactly this, no new checks
invented for the structural half.

**One retry on failure.** If the merged bundle fails to parse, the specific
`BundleError` is sent back to the model once as feedback — *"slot `JOBID2`: pattern
introduces a capturing group"* — and it gets one attempt to fix just the offending
slot. A second failure drops that slot and the run reports it, rather than either
blocking on an LLM that can't converge or discarding every slot that *did* validate
alongside the one that didn't.

**Idempotency, the one property this can afford to check for free.** After a
merge parses, the full corpus is re-masked with the new bundle and checked that
`mask(mask(line)) == mask(line)` for every line — the same property `proptest`
already guards for the curated bundle. It's the cheapest real signal that a
synthesized pattern isn't pathological (unbounded backtracking, or a slot that
re-matches its own placeholder text), and it costs nothing beyond a second pass over
a corpus already in memory.

**Merge order: insert before the curated bundle's final slot.** Not a hardcoded
search for a slot named `NUM` — that breaks the moment `NUM` is renamed. The rule
instead trusts the same convention the curated bundle's own header comment already
requires: *"add specific patterns above general ones, always."* A bundle that follows
that convention ends with its single most general pattern, so "insert immediately
before the last slot" is the mechanical expression of "synthesized slots are more
specific than any generic catch-all, so they must precede one." This is verified by
`Drain`'s own confirmed ordering in the shipped file (`TS` through `PID`, `NUM`
last) and holds regardless of what the curated bundle's slots are named.

Each synthesized slot's `why` is written as `"synthesized: " + the model's own
reasoning + " (n corpus occurrences)"` — traceable at a glance to how it got there,
the same discipline the curated bundle's own `why` field enforces by being required.

## 6. Testing

| Property | Test |
|---|---|
| Candidate positions are found correctly | `insta` snapshot over a fixture corpus, deterministic, no network |
| Occurrence and candidate-count limits are respected | Unit tests against the fixture corpus |
| The transport carries a prompt and returns a response | `wiremock` against `oarfish-synth::Client`, proving the wire shape only |
| A structural failure retries once, then drops | Fake `Synthesize` returning a bad slot once, a fixed one second time — no network |
| A twice-bad slot is dropped, not blocking | Fake `Synthesize` returning the same bad slot twice |
| Idempotency holds after merge | Full corpus re-mask, both directions |
| Merge always inserts before the last slot | `proptest` over generated curated bundles and slot sets |
| `--bundle` loads a file instead of the embedded default | Integration test on the daemon's flag parsing |
| **Acceptance** | One integration test: a corpus with an obvious unmasked pattern (a fixed job-id format repeated with varying hex suffixes) produces a bundle that masks it, driven end to end through a fake `Synthesize` |

## 7. Deferred, deliberately

| Deferred | Why |
|---|---|
| Automatic re-synthesis on drift | Needs a signal for "the bundle stopped covering the corpus," which doesn't exist yet |
| A second LLM provider or local-model path | §13 of the parent doc: unscheduled, a config change by construction once needed |
| Editing or removing existing slots | Augment-only by design (§2); an LLM rewriting curated, hand-tuned patterns is exactly the risk this avoids |
| Multi-bundle merge (several delta files) | One `--bundle` input, one `--out` output; accumulation happens by re-running against the previous output |

## 8. Changes to existing documents

- `docs/specs/2026-09-17-oarfish-design.md` §12 — resolve open question #1 with a
  pointer to this doc.
- `docs/specs/2026-09-18-m1-mask-design.md` — its deferred table's "loading a bundle
  from a config path" row is picked up here, not still open.
- `crates/oarfish-mask/src/lib.rs` — doc comment gains the `synth` module and its
  `oarfish-drain` dependency.
- `README.md` — the "not on the critical path" line updates once this ships.
