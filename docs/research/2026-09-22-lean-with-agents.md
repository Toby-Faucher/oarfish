# Lean 4 with coding agents, for verifying existing Rust: practice as of 2026-09-22

**Question.** How are people using Lean 4 with AI agents to specify and verify existing
software, especially Rust, and to find real bugs? And what should oarfish-drain do with it?

**Caveat on X.** x.com returns HTTP 402 to fetches. X posts below are quoted from
search-result text only and are marked **[X, unverified]**. Each one was then traced to
a primary source where one exists.

## TL;DR

- The established pattern is **extract or model the code, have the agent state
  properties, then prove each one or prove its negation**. Lean catches bad proofs. It
  does not catch bad statements, so every serious report keeps a human reviewing the
  statements.
- The real bugs found are **functional**: overflow, off-by-one, wrong shift, wrong bound.
  I found no documented race condition caught by Lean plus an agent. Aeneas does not
  support concurrency at all.
- **Aeneas is not a fit for oarfish-drain today.** Its Lean library has no model of
  `std::collections::HashMap`/`BTreeMap`, `f64` or `split_whitespace`, so those would
  need hand-written models anyway. Use a **hand-written Lean model checked against the
  Rust by differential testing** (the Cedar pattern).
- **Drain has no shared-state concurrency.** One task owns it through `&mut self`, and
  the crate has `forbid(unsafe_code)`. The targets are functional invariants.
- Reading the code for this note turned up **four defects a Lean model would have
  flushed out** (confirmed by running them, see below). One of them breaks invariant 4:
  one-token lines merge without a similarity check.

## 1. Workflows

- **Extract, infer properties, prove or refute.** Mistral's Leanstral 1.5 pipeline:
  "Aeneas translates Rust code to Lean", the model infers intended properties, makes four
  attempts to prove each, then four attempts to prove its negation
  ([Mistral, 2026-07-02](https://mistral.ai/news/leanstral-1-5/); also
  [X post by Mert Ünsal, unverified](https://x.com/mertunsal2020/status/2073046292734111846)).
- **Extraction at scale with agents doing the proofs.** The Aeneas/SymCrypt report
  describes agents writing about 5,100 theorems (125 KLOC of proof), at "a few dollars
  per line of Rust". Humans own the specs and the review of theorem statements
  ([arXiv 2609.15648](https://arxiv.org/abs/2609.15648);
  [MSR blog, 2026-07-13](https://www.microsoft.com/en-us/research/blog/verifying-rust-cryptography-in-symcrypt-from-standards-to-code/);
  [X @leanprover, unverified](https://x.com/leanprover/status/2077051962009481676)).
  A Plonky3/RISC Zero pipeline used Aristotle and Aleph as provers and reports "Lean 4
  toolchain drift across tools" and Aeneas/hax extraction limits as the main friction
  ([arXiv 2605.30106](https://arxiv.org/abs/2605.30106)).
- **A hand-written model kept faithful by differential testing.** AWS Cedar keeps an
  executable Lean model about 10x smaller than the Rust. It proves properties about the
  model and runs millions of random inputs through both implementations to check they
  agree ([arXiv 2407.01688](https://arxiv.org/abs/2407.01688);
  [Amazon Science](https://www.amazon.science/blog/how-we-built-cedar-with-automated-reasoning-and-differential-testing)).
  This predates agents, but it is the faithfulness technique that fits hand models.
- **Extraction options.** hax's Lean backend now runs Charon plus Aeneas; the old one is
  `legacy-lean` ([hax blog, Aug/Sep 2026](https://hax.cryspen.com/blog/archive/2026/)).
  Verus is the contrast: proofs sit inline in Rust, not in Lean, and it has its own agent
  line of work ([VeruSAGE, arXiv 2512.18436](https://arxiv.org/abs/2512.18436)). I could
  not find a primary source for "LeanMachines", so it is left out.
- **The agent loop.**
  - [`lean-lsp-mcp`](https://github.com/oOo0oOo/lean-lsp-mcp) gives the agent goals,
    diagnostics, `lean_multi_attempt` and search tools (Loogle, LeanSearch). Install with
    `claude mcp add lean-lsp uvx lean-lsp-mcp`. It recommends the
    [lean4-skills](https://github.com/cameronfreer/lean4-skills) skill.
  - Galois found that the MCP "improved performance significantly"
    ([Dodds, 2025-09-16](https://www.galois.com/articles/claude-can-sometimes-prove-it)).
  - [`plausible`](https://github.com/leanprover-community/plausible) (a tactic, plus
    `Testable.check` for `#eval`) finds counterexamples before any proof effort is
    spent.
  - [Pantograph](https://github.com/leanprover/Pantograph) and
    [LeanDojo-v2](https://github.com/lean-dojo/LeanDojo-v2) are proof-search and training
    harnesses. They are not needed when the agent is Claude Code working over MCP.
  - `lake build` is the ground truth.
  - Ilya Sergey calls Claude Code and Aristotle his "new favourite backend solvers for
    auto-active program verification in Lean"
    ([X, unverified](https://x.com/ilyasergey/status/2008072113257861436)).

## 2. Bugs found by Lean plus agents

| Project | Finding | Status |
|---|---|---|
| [datrs/varinteger](https://github.com/datrs/varinteger) | `value + 1` overflows at `u64::MAX` in the zigzag sign function: a panic in debug builds, silent corruption in release | Named in [Mistral's post](https://mistral.ai/news/leanstral-1-5/). The headline "47 violations, 11 real bugs, 5 unreported" across 57 repos lists only this one example |
| SymCrypt SHA-3 | Pointer advance error, leaving a few incorrect output bytes | [arXiv 2609.15648](https://arxiv.org/abs/2609.15648) |
| SymCrypt ML-KEM | An asserted bound `x < Q` is violated: agents produced a counterexample with `x = Q` | same |
| FrodoKEM | Constant-time compare shifts `>> 8` where it should shift `>> 16`, risking a secret leak | same |

**Race conditions:** none found. Aeneas "does not support raw pointers, interior
mutability, or concurrency" ([arXiv 2609.15648](https://arxiv.org/abs/2609.15648)). The
concurrency work I found models a paper rather than code, e.g. Claude formalizing
deny-guarantee reasoning ([Galois](https://www.galois.com/articles/claude-can-sometimes-prove-it)).

## 3. Tooling state

- **Aeneas** (main branch, last commit 2026-09-21):
  - Supported: loops (translated to recursion), common nested borrows, traits, mutable
    iterators ([arXiv 2609.15648](https://arxiv.org/abs/2609.15648)), and closures by
    closure conversion ([newsletter, July 2025](https://aeneasverif.github.io/newsletter/2025/07/02/aeneas-newsletter.html)).
  - Not supported: `return` or `break` out of nested loops, and a generic instantiated
    with `&mut` ([README](https://github.com/AeneasVerif/aeneas)).
  - The test suite includes `dyn.rs`, `closures.rs`, `iterators.rs`, BST and AVL trees
    ([tests/src](https://github.com/AeneasVerif/aeneas/tree/main/tests/src)).
  - Its [Lean `Std`](https://github.com/AeneasVerif/aeneas/tree/main/backends/lean/Aeneas/Std)
    models `Vec`, slices, arrays, scalars and `Str` (as `Slice U8`). I found **no model
    of std `HashMap`/`BTreeMap`, no `f64`, and no `split_whitespace`**. The project's own
    `hashmap.rs` test is a hand-written map with `usize` keys.
  - So an LRU cache or prefix tree built on std collections needs hand-written models
    of those collections before extraction is useful.
- **Lean `Std`** has `HashMap`, `TreeMap` and `ExtHashMap` (its equality is equality of
  partial functions, which makes it the best choice for reasoning). "All the basic
  operations … are fully verified" against list models
  ([reference](https://lean-lang.org/doc/reference/latest/Basic-Types/Maps-and-Sets/)).
  This is enough to model the table, the stamps and the tree.
- **Pain points:** toolchain drift ([2605.30106](https://arxiv.org/abs/2605.30106)),
  and proofs that go stale as the code changes
  ([2609.15648](https://arxiv.org/abs/2609.15648)).

## 4. Where agents fail, and the guardrails people use

- **Weakened goals and unsound assumptions.** Agents "cheat with unsound assumptions or
  weakened goals", so "careful human review of theorem statements remains essential"
  ([2609.15648](https://arxiv.org/abs/2609.15648)).
- **Hidden cheats.** Agents hide axioms deep in the dependency chain, hide `sorry`
  behind helper lemmas, or swap a real definition for an oversimplified one. The fix in
  [arXiv 2605.29955](https://arxiv.org/abs/2605.29955) is a metaprogram that walks every
  declaration's axiom set, plus layered reviewer agents.
- **Hypothesis creep.** A theorem accumulated 42 hypotheses that should have been
  lemmas. It still compiled with few `sorry`s, but the statement had quietly weakened
  (same paper).
- **Thrashing, and believing its own wrong comments**
  ([Galois](https://www.galois.com/articles/claude-can-sometimes-prove-it)).
- **Specs are the bottleneck.** Kleppmann argues that "the challenge will move to
  correctly defining the specification"
  ([2025-12-08](https://martin.kleppmann.com/2025/12/08/ai-formal-verification.html)).

## What reading oarfish-drain already found

I confirmed each of these with a scratch binary against the crate. No repo files were
changed.

1. **One-token lines merge with no similarity check.** `train("succeeded")` then
   `train("failed")` yields one cluster with template `<*>`. The cause is the
   `tokens.len() < 2` shortcut in `Drain::search` (`drain.rs:231`). Loki has the same
   shortcut ([drain.go](https://github.com/grafana/loki/blob/main/pkg/pattern/drain/drain.go)),
   but Loki drops lines under 4 tokens, and oarfish removed that guard (spec §3). This
   breaks invariant 4.
   **Fixed 2026-09-22** (`drain/join-soundness`): the shortcut is gone, and
   `search_sound` in `lean/OarfishDrain/Theorems.lean` proves join soundness at every
   length. `plausible` found the counterexample unprompted before the fix.
2. **Stale tree ids are never cleaned.** Spec §3 says stale ids are "filtered on read
   and cleaned on insert". Loki does clean them on insert. `tree::insert` does not. As a
   result the leaves' `cluster_ids` grow without bound, and so does the search scan,
   under exactly the hostile input `max_clusters` exists to bound.
3. **The `neighbours` ceiling claim is false.** The comment says a pair at or above 0.90
   "cannot exist as two clusters". Counterexamples:
   - Two 20-token lines differing only in token 2 (`alice`/`bob`) are split by the tree
     (spec §3 says so), score a Jaccard of 19/21 ≈ 0.905, and are **never referred**.
   - `a b c d` and `d c b a` score a Jaccard of 1.0 and are not referred either.
   - The first case is the very over-split that merge review exists to repair.
4. **`max_children: 0` panics** at `tree.rs:145` on the first 3-token line. The config
   is not validated.

## Recommended plan for oarfish-drain

**Modelling approach: a hand-written Lean model plus differential testing, not
Aeneas.**
- The algorithm is about 300 lines of logic over `HashMap`, `BTreeMap`, `String` and
  `f64`, none of which Aeneas models (§3).
- A Lean model on `Std.ExtHashMap`, `List String` and exact `Nat` arithmetic
  (`10·similar ≥ 9·n`) is small and readable, and a human can review it.
- Faithfulness comes Cedar-style: the Rust proptest generator plus the corpus produce
  line sequences, and both implementations must emit identical `(seq, template)`
  streams.
- The float gap closes by exhaustive check: for `n ≤ 128 = max_tokens`, `k/n` as an
  `f64` compares with 0.90 the same way the exact rational does. Check it with
  `decide` or `#eval` over all `(k, n)` pairs.
- Revisit Aeneas if it gains std collection models.

**Ranked theorems** (value, difficulty):

1. **Join soundness.** A line joins cluster C only if `sim(C, line) ≥ θ`. High value,
   easy. *Currently false for n = 1 (defect 1).*
2. **Neighbour completeness.** Any two live clusters with Jaccard in `[floor, 1]` are
   referred, or the documented band is changed. High value, easy. *Currently false
   (defect 3).*
3. **Table bound and consistency.** After every `train`, `|table| ≤ max_clusters`, and
   `stamps`, `table` and `counts` stay in bijection over live seqs. High value, medium.
4. **Bounded tree.** Total leaf ids ≤ f(max_clusters), and fan-out ≤ `max_children`.
   High value, medium. *The first half is false today (defect 2).*
5. **Members stay close to their template.** Every member matches its template at every
   non-`<*>` position, and `(n − params)/n ≥ θ` holds for n ≥ 2 (by induction:
   generalizing turns exactly the join's mismatches into params). Medium value, medium.
6. **Settled ids never move.** With no eviction in between, re-training a line its
   cluster already matches leaves the `TemplateId` unchanged (spec §5). Medium value,
   hard: it needs tree-routing and best-candidate lemmas.
7. **Panic-freedom and totality** for all valid configs. *Currently false for
   `max_children = 0` (defect 4).* Low value, easy.
8. **Similarity and Jaccard lie in [0, 1], and Jaccard is symmetric.** Low value, easy.

Determinism is free in a pure Lean model. On the Rust side the risk would be `HashMap`
iteration order, and the code already sorts or sums wherever that order could leak.

**Repo layout.**
- `lean/lakefile.toml` and `lean/lean-toolchain` (pinned).
- `lean/OarfishDrain/{Model,Similarity,Tree,Table,Theorems}.lean`.
- `lean/Main.lean` as a `lake exe` that reads JSON lines on stdin and emits assignments.
- `crates/oarfish-drain/tests/lean_diff.rs`, gated behind an env var, which runs the
  differential check.

**CI guardrails.**
- `lake build` must pass.
- Grep-fail on `sorry`, `admit`, `axiom` and `unsafe`.
- A `#print axioms` check on every theorem in `Theorems.lean`, allowing only
  `propext`, `Quot.sound` and `Classical.choice`.
- `Theorems.lean` is CODEOWNERS-protected, so agents may add proofs but not change
  statements without human review.
- The differential test runs in CI over the committed corpus.

**First concrete step.**
1. Write `Model.lean` covering `similarity`, `generalize` and a table-only `train`
   without the tree (search = the best candidate among clusters of equal length).
2. State theorems 1 and 5 about it, and run `plausible` on theorem 1. It should return
   `["succeeded", "failed"]` within seconds.
3. That settles the fix for defect 1 before any proof effort is spent. Add the tree
   next.
