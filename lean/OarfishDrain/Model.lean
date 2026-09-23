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
- **Eviction, and the tree as (leaf path, id) entries.** `train` takes the
  `max_clusters` and `max_leaf_clusters` caps and evicts as the Rust does.
  The tree appears as `tree`, one entry per leaf id, tagged with its leaf's
  path. **Which path a new cluster gets is a parameter, `pathOf`**, and every
  theorem holds for all of them. So the proofs cover the Rust's routing,
  whatever it is, given two facts the Rust has by construction: eviction
  finds an id by the path recorded at insert (`Entry::path`), and a search
  scans one leaf. The pruning of emptied nodes is left to the Rust proptest
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
  /-- Every id any tree leaf holds, with its leaf's path. -/
  tree : List (List Token × Nat)
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

/-- `Drain::forget`: drop cluster `v` from the table and from its leaf. -/
def forget (v : Nat) (st : State) : State :=
  { st with clusters := st.clusters.filter (·.seq ≠ v), tree := st.tree.filter (·.2 ≠ v) }

/-- The ids in the leaf at `path`: `tree::leaf_ids`. -/
def idsAt (st : State) (path : List Token) : List Nat :=
  (st.tree.filter (·.1 = path)).map (·.2)

/-- `Drain::evict`: past the cap (0 means unbounded), forget the least
recently used cluster. One insert per `train` means at most one eviction. -/
def evict (cap : Nat) (st : State) : State :=
  if cap ≠ 0 ∧ cap < st.clusters.length then
    match oldest st.clusters with
    | some v => forget v st
    | none => st
  else st

/-- `Drain::evict_from_leaf`: past `leafCap` (0 means unbounded), forget the
least recently used cluster in the leaf at `path`, never `newest`. One insert
per `train` means at most one. -/
def evictFromLeaf (leafCap : Nat) (path : List Token) (newest : Nat) (st : State) : State :=
  if leafCap ≠ 0 ∧ leafCap < (idsAt st path).length then
    match oldest (st.clusters.filter fun c => c.seq ∈ idsAt st path ∧ c.seq ≠ newest) with
    | some v => forget v st
    | none => st
  else st

/-- `Drain::train` under `max_clusters = cap` and `max_leaf_clusters =
leafCap`, returning the assigned seq. `pathOf` is where the tree files a new
cluster; it is left open, so theorems hold for any routing. -/
def train (cap leafCap : Nat) (pathOf : State → List Token → List Token)
    (st : State) (toks : List Token) : State × Nat :=
  let tick := st.tick + 1
  match search st toks with
  | some seq =>
    let clusters := st.clusters.map fun c =>
      if c.seq = seq then
        { c with tokens := generalize c.tokens toks, size := c.size + 1, tick }
      else c
    ({ st with clusters, tick }, seq)
  | none =>
    let path := pathOf st toks
    let st' : State :=
      { clusters := st.clusters ++ [⟨st.nextSeq, toks, 1, tick⟩],
        nextSeq := st.nextSeq + 1, tick, tree := st.tree ++ [(path, st.nextSeq)] }
    (evict cap (evictFromLeaf leafCap path st.nextSeq st'), st.nextSeq)

/-! ## Cost

Work is counted in token comparisons: the inner loop of `tree::similarity`,
one step per position of the zipped token lists. `search` runs it once per
candidate. The Rust scans a tree leaf, whose ids are live clusters of the
line's length (`trainAll_inv`), so it compares against a subset of the
candidates counted here: this cost is an upper bound on the Rust's.
-/

/-- Steps `simCounts` takes: one per zipped position. -/
def simSteps : List Token → List Token → Nat
  | _ :: cs, _ :: ts => simSteps cs ts + 1
  | _, _ => 0

/-- Token comparisons one `search` makes. -/
def searchCost (st : State) (toks : List Token) : Nat :=
  ((st.clusters.filter (·.tokens.length == toks.length)).map
    (fun c => simSteps c.tokens toks)).sum

/-- Token comparisons scanning the leaf at `path`: each id's cluster
against the line. -/
def leafSearchCost (st : State) (path : List Token) (toks : List Token) : Nat :=
  ((idsAt st path).map fun id =>
    match st.clusters.find? (·.seq = id) with
    | some c => simSteps c.tokens toks
    | none => 0).sum

/-- A stand-in for the tree's routing, for `#eval`: the first six tokens.
Theorems never assume it. -/
def prefixPath (_ : State) (toks : List Token) : List Token := toks.take 6

/-- Train a sequence of lines, returning each line's seq. -/
def trainAll (lines : List (List Token)) (cap : Nat := 0) (leafCap : Nat := 0)
    (pathOf : State → List Token → List Token := prefixPath) : State × List Nat :=
  lines.foldl
    (fun (st, seqs) toks => let (st', s) := train cap leafCap pathOf st toks; (st', seqs ++ [s]))
    (State.init, [])

end OarfishDrain
