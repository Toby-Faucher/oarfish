/-!
# A Lean model of `oarfish-drain`'s clustering decisions

Step 1 of `docs/research/2026-09-22-lean-with-agents.md`: `similarity`,
`generalize` and a table-only `train`. The prefix tree is left out: search
considers every live cluster of the line's token count, which is a superset
of what any tree leaf holds. Soundness facts proved here ("a line only joins a
cluster it is similar to") therefore hold for the tree too, since the tree
only removes candidates.

Each definition names the Rust it mirrors. Where the model departs from the
Rust, the departure is stated.

- **Exact threshold.** Rust compares `similar as f64 / n as f64 >= 0.90`.
  Here that is `10 * similar ≥ 9 * n` over `Nat`. That the two agree for every
  `n ≤ max_tokens = 128` is a separate, finite check, not yet done.
- **No eviction.** `max_clusters` is taken to be unbounded. Theorem 3 of the
  plan brings the LRU in.
- **Empty lines.** `similarEnough 0 0` holds, matching `tree::similarity`'s
  explicit 1.0 for two empty token lists (not `0.0 / 0.0`, which is NaN).
-/

namespace OarfishDrain

abbrev Token := String

/-- `Config::param`, the generalized-position marker. -/
def param : Token := "<*>"

/-- `Cluster`, minus the rendered template text and its hash, which are pure
functions of `tokens` (`tokens.join(" ")`, `TemplateId::of`). -/
structure Cluster where
  seq : Nat
  tokens : List Token
  size : Nat
  deriving Repr, DecidableEq

/-- The table. `clusters` is in creation order, which is also the order a
tree leaf's `cluster_ids` lists them in (`tree::insert` pushes). -/
structure State where
  clusters : List Cluster
  nextSeq : Nat
  deriving Repr

/-- `Drain::new`: an empty table, sequence numbers from 1. -/
def State.init : State := ⟨[], 1⟩

/-- `tree::similarity`: `(similar, params)`. Positions where the cluster holds
`param` are skipped, not counted as similar. Lengths are equal by construction
in the Rust; here `zip` semantics truncate. -/
def simCounts : List Token → List Token → Nat × Nat
  | c :: cs, t :: ts =>
    let (s, p) := simCounts cs ts
    if c = param then (s, p + 1) else if c = t then (s + 1, p) else (s, p)
  | _, _ => (0, 0)

/-- `sim ≥ config.similarity` at 0.90, exactly. -/
def similarEnough (similar n : Nat) : Bool := 9 * n ≤ 10 * similar

/-- `tree::generalize`: a differing position becomes `param`, and a position
already at `param` stays. -/
def generalize : List Token → List Token → List Token
  | c :: cs, t :: ts => (if c = t then c else param) :: generalize cs ts
  | cs, _ => cs

/-- The best-candidate fold in `Drain::search`: strictly more similar wins,
and on a tie in similarity, more params wins. The first candidate always
beats the Rust's `best_sim = -1.0` sentinel. Similarity is compared on the
`similar` count, since every candidate has the line's length. -/
def pickBest (toks : List Token) (cands : List Cluster) : Option (Nat × Nat × Nat) :=
  cands.foldl (fun acc c =>
    let (s, p) := simCounts c.tokens toks
    match acc with
    | none => some (c.seq, s, p)
    | some (_, bs, bp) => if s > bs ∨ (s = bs ∧ p > bp) then some (c.seq, s, p) else acc)
    none

/-- `Drain::search`: the best candidate of the line's length, if it passes
the threshold. Every length faces the threshold. (The Rust once returned a
one-token line's first cluster unchecked; `plausible` found `[""]` and
`["\x01"]` joining on that path.) -/
def search (st : State) (toks : List Token) : Option Nat :=
  let cands := st.clusters.filter (·.tokens.length == toks.length)
  match pickBest toks cands with
  | some (seq, s, _) => if similarEnough s toks.length then some seq else none
  | none => none

/-- `Drain::train`, returning the assigned sequence number. -/
def train (st : State) (toks : List Token) : State × Nat :=
  match search st toks with
  | some seq =>
    let clusters := st.clusters.map fun c =>
      if c.seq = seq then { c with tokens := generalize c.tokens toks, size := c.size + 1 }
      else c
    ({ st with clusters }, seq)
  | none =>
    ({ clusters := st.clusters ++ [⟨st.nextSeq, toks, 1⟩], nextSeq := st.nextSeq + 1 },
      st.nextSeq)

/-- Train a sequence of lines, returning each line's sequence number. -/
def trainAll (lines : List (List Token)) : State × List Nat :=
  lines.foldl (fun (st, seqs) toks => let (st', s) := train st toks; (st', seqs ++ [s]))
    (State.init, [])

end OarfishDrain
