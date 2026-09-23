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
- **Eviction, and the tree as a multiset.** `train` takes the `max_clusters`
  cap and evicts the least recently used cluster past it, as the Rust does.
  The tree appears only as `tree`, the ids its leaves hold, which is all the
  eviction bookkeeping touches. Which leaf an id sits in, and the pruning of
  emptied nodes, are left to the Rust proptest
  `the_tree_and_indexes_track_the_live_table`.
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
  /-- `Entry::tick`: when this cluster last took a line. -/
  tick : Nat
  deriving Repr, DecidableEq

/-- The table. `clusters` is in creation order, which is also the order a
tree leaf's `cluster_ids` lists them in (`tree::insert` pushes). -/
structure State where
  clusters : List Cluster
  nextSeq : Nat
  /-- `Drain::tick`, one per `train`. -/
  tick : Nat
  /-- Every id held by any tree leaf, with multiplicity. -/
  tree : List Nat
  deriving Repr

/-- `Drain::new`: an empty table, sequence numbers from 1. -/
def State.init : State := ⟨[], 1, 0, []⟩

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

/-- The least recently used cluster's seq: `stamps.pop_first()`. Ticks are
unique, so the first minimum is the minimum. -/
def oldest : List Cluster → Option Nat
  | [] => none
  | c :: cs =>
    match oldest cs with
    | none => some c.seq
    | some s =>
      match cs.find? (·.seq = s) with
      | some o => if c.tick ≤ o.tick then some c.seq else some s
      | none => some c.seq

/-- `Drain::evict`: past the cap (0 means unbounded), forget the least
recently used cluster, from the table **and from its tree leaf**. One insert
per `train` means at most one eviction. -/
def evict (cap : Nat) (st : State) : State :=
  if cap ≠ 0 ∧ cap < st.clusters.length then
    match oldest st.clusters with
    | some v => { st with clusters := st.clusters.filter (·.seq ≠ v), tree := st.tree.erase v }
    | none => st
  else st

/-- `Drain::train` under `max_clusters = cap`, returning the assigned seq. -/
def train (cap : Nat) (st : State) (toks : List Token) : State × Nat :=
  let tick := st.tick + 1
  match search st toks with
  | some seq =>
    let clusters := st.clusters.map fun c =>
      if c.seq = seq then
        { c with tokens := generalize c.tokens toks, size := c.size + 1, tick }
      else c
    ({ st with clusters, tick }, seq)
  | none =>
    let st' : State :=
      { clusters := st.clusters ++ [⟨st.nextSeq, toks, 1, tick⟩],
        nextSeq := st.nextSeq + 1, tick, tree := st.tree ++ [st.nextSeq] }
    (evict cap st', st.nextSeq)

/-- Train a sequence of lines under `cap`, returning each line's seq. -/
def trainAll (lines : List (List Token)) (cap : Nat := 0) : State × List Nat :=
  lines.foldl (fun (st, seqs) toks => let (st', s) := train cap st toks; (st', seqs ++ [s]))
    (State.init, [])

end OarfishDrain
