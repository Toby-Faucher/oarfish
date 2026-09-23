import OarfishDrain.Model

/-!
# Theorems about the model

Statements here are reviewed by a human; proofs may be written by anyone.
Nothing in this file may use `sorry`, `admit` or a new `axiom`
(`#print axioms` at the bottom lists what each theorem rests on).
-/

namespace OarfishDrain

/-- The step function `pickBest` folds with. -/
private def pickStep (toks : List Token) (acc : Option (Nat × Nat × Nat)) (c : Cluster) :
    Option (Nat × Nat × Nat) :=
  let (s, p) := simCounts c.tokens toks
  match acc with
  | none => some (c.seq, s, p)
  | some (_, bs, bp) => if s > bs ∨ (s = bs ∧ p > bp) then some (c.seq, s, p) else acc

/-- One step either keeps the accumulator or takes the current cluster. -/
private theorem pickStep_cases (toks : List Token) (acc : Option (Nat × Nat × Nat))
    (c : Cluster) {r : Nat × Nat × Nat} (h : pickStep toks acc c = some r) :
    acc = some r ∨ (c.seq, simCounts c.tokens toks) = (r.1, r.2.1, r.2.2) := by
  unfold pickStep at h
  cases acc with
  | none => simp at h; subst h; simp
  | some b =>
    simp only at h
    split at h
    · right; simp at h; subst h; simp
    · left; exact h

/-- Whatever the fold returns came from the accumulator or from a cluster in
the list, carrying that cluster's own similarity counts. -/
private theorem foldl_pickStep_mem (toks : List Token) :
    ∀ (cands : List Cluster) (acc : Option (Nat × Nat × Nat)) {r : Nat × Nat × Nat},
      cands.foldl (pickStep toks) acc = some r →
        acc = some r ∨ ∃ c ∈ cands, c.seq = r.1 ∧ simCounts c.tokens toks = (r.2.1, r.2.2)
  | [], acc, r, h => by simp at h; exact Or.inl h
  | c :: cs, acc, r, h => by
    simp only [List.foldl_cons] at h
    rcases foldl_pickStep_mem toks cs (pickStep toks acc c) h with h' | ⟨c', hc', hs, hsc⟩
    · rcases pickStep_cases toks acc c h' with h'' | h''
      · exact Or.inl h''
      · right
        refine ⟨c, List.mem_cons_self .., ?_, ?_⟩
        · simp [Prod.ext_iff] at h''; exact h''.1
        · simp [Prod.ext_iff] at h''; exact Prod.ext h''.2.1 h''.2.2
    · exact Or.inr ⟨c', List.mem_cons_of_mem _ hc', hs, hsc⟩

theorem pickBest_mem (toks : List Token) (cands : List Cluster) {seq s p : Nat}
    (h : pickBest toks cands = some (seq, s, p)) :
    ∃ c ∈ cands, c.seq = seq ∧ simCounts c.tokens toks = (s, p) := by
  have h' : cands.foldl (pickStep toks) none = some (seq, s, p) := h
  rcases foldl_pickStep_mem toks cands none h' with h'' | h''
  · simp at h''
  · exact h''

/-- **Join soundness.** Whenever `search` sends a line to a cluster, that
cluster is live, has the line's token count, and passes the 0.90 threshold
against the line. At every length: this is the statement the one-token
shortcut broke. -/
theorem search_sound (st : State) (toks : List Token) {seq : Nat}
    (h : search st toks = some seq) :
    ∃ c ∈ st.clusters, c.seq = seq ∧ c.tokens.length = toks.length ∧
      similarEnough (simCounts c.tokens toks).1 toks.length = true := by
  unfold search at h
  simp only at h
  split at h
  · rename_i seq' s p hbest
    split at h
    · rename_i henough
      simp at h; subst h
      obtain ⟨c, hc, hseq, hsim⟩ := pickBest_mem toks _ hbest
      obtain ⟨hmem, hl⟩ := List.mem_filter.mp hc
      refine ⟨c, hmem, hseq, by simpa using hl, ?_⟩
      rw [hsim]; exact henough
    · simp at h
  · simp at h

/-! ## Eviction keeps the tree and the table in step -/

/-- What every reachable state satisfies: seqs are distinct, the tree holds
exactly the live seqs, and every live seq was issued. -/
def Inv (st : State) : Prop :=
  (st.clusters.map (·.seq)).Nodup ∧ st.tree.Perm (st.clusters.map (·.seq)) ∧
    ∀ c ∈ st.clusters, c.seq < st.nextSeq

theorem oldest_mem : ∀ (l : List Cluster) {v : Nat}, oldest l = some v → ∃ c ∈ l, c.seq = v
  | [], v, h => by simp [oldest] at h
  | c :: cs, v, h => by
    simp only [oldest] at h
    split at h
    · simp at h; exact ⟨c, List.mem_cons_self .., h⟩
    · rename_i s hs
      split at h
      · split at h
        · simp at h; exact ⟨c, List.mem_cons_self .., h⟩
        · simp at h; subst h
          obtain ⟨c', hc', hseq⟩ := oldest_mem cs hs
          exact ⟨c', List.mem_cons_of_mem _ hc', hseq⟩
      · simp at h; exact ⟨c, List.mem_cons_self .., h⟩

theorem oldest_isSome : ∀ (l : List Cluster), l ≠ [] → (oldest l).isSome
  | [], h => absurd rfl h
  | c :: cs, _ => by
    simp only [oldest]
    split
    · rfl
    · split
      · split <;> rfl
      · rfl

/-- The seqs of the clusters `filter` keeps are the seqs with `v` dropped. -/
private theorem map_seq_filter (l : List Cluster) (v : Nat) :
    (l.filter (·.seq ≠ v)).map (·.seq) = (l.map (·.seq)).filter (· ≠ v) := by
  rw [List.filter_map]; rfl

theorem evict_inv (cap : Nat) (st : State) (h : Inv st) : Inv (evict cap st) := by
  obtain ⟨hnd, hperm, hlt⟩ := h
  unfold evict
  split
  · split
    · rename_i v _
      refine ⟨?_, ?_, ?_⟩
      · rw [map_seq_filter]; exact hnd.filter _
      · have hf : (st.clusters.map (·.seq)).filter (fun x => decide (x ≠ v)) =
            (st.clusters.map (·.seq)).erase v := by
          rw [hnd.erase_eq_filter]
          apply List.filter_congr
          intro x _
          by_cases hx : x = v <;> simp [hx]
        rw [map_seq_filter, hf]; exact hperm.erase v
      · intro c hc; exact hlt c (List.mem_filter.mp hc).1
    · exact ⟨hnd, hperm, hlt⟩
  · exact ⟨hnd, hperm, hlt⟩

/-- A join rewrites a cluster in place: its seq, and so every seq, is unchanged. -/
private theorem map_seq_join (l : List Cluster) (seq : Nat) (f : Cluster → Cluster)
    (hf : ∀ c, (f c).seq = c.seq) :
    (l.map fun c => if c.seq = seq then f c else c).map (·.seq) = l.map (·.seq) := by
  induction l with
  | nil => rfl
  | cons c cs ih => by_cases h : c.seq = seq <;> simp [h, hf, ih]

/-- **The tree holds exactly the live clusters**, after every `train`. The
old `evict` forgot the tree, and `plausible` refuted this with two distinct
lines under `max_clusters = 1`. -/
theorem train_inv (cap : Nat) (st : State) (toks : List Token) (h : Inv st) :
    Inv (train cap st toks).1 := by
  obtain ⟨hnd, hperm, hlt⟩ := h
  unfold train
  split
  · rename_i seq _
    simp only
    have hmap := map_seq_join st.clusters seq
      (fun c => { c with tokens := generalize c.tokens toks, size := c.size + 1, tick := st.tick + 1 })
      (fun _ => rfl)
    refine ⟨?_, ?_, ?_⟩
    · rw [hmap]; exact hnd
    · rw [hmap]; exact hperm
    · intro c hc
      obtain ⟨c', hc', rfl⟩ := List.mem_map.mp hc
      have hc'lt := hlt c' hc'
      by_cases hs : c'.seq = seq
      · simp [hs]; omega
      · simp [hs]; exact hc'lt
  · apply evict_inv
    refine ⟨?_, ?_, ?_⟩
    · simp only [List.map_append, List.map_cons, List.map_nil]
      refine List.nodup_append.mpr ⟨hnd, by simp, ?_⟩
      intro a ha b hb
      simp at hb; subst hb
      obtain ⟨c, hc, rfl⟩ := List.mem_map.mp ha
      exact Nat.ne_of_lt (hlt c hc)
    · simp only [List.map_append, List.map_cons, List.map_nil]
      exact hperm.append_right _
    · intro c hc
      rcases List.mem_append.mp hc with hc | hc
      · exact Nat.lt_succ_of_lt (hlt c hc)
      · simp at hc; subst hc; exact Nat.lt_succ_self _

theorem init_inv : Inv State.init := ⟨List.nodup_nil, List.Perm.refl _, by simp [State.init]⟩

/-- Every state `trainAll` reaches satisfies `Inv`: from an empty table,
after any lines, under any cap, the tree holds exactly the live clusters. -/
theorem trainAll_inv (lines : List (List Token)) (cap : Nat) : Inv (trainAll lines cap).1 := by
  unfold trainAll
  suffices ∀ (acc : State × List Nat), Inv acc.1 →
      Inv (lines.foldl (fun (st, seqs) toks =>
        let (st', s) := train cap st toks; (st', seqs ++ [s])) acc).1 from
    this _ init_inv
  induction lines with
  | nil => intro acc h; exact h
  | cons l ls ih => intro acc h; exact ih _ (train_inv cap acc.1 l h)

/-- **The table never exceeds `max_clusters`** (for a nonzero cap). -/
theorem train_bound (cap : Nat) (st : State) (toks : List Token) (hcap : cap ≠ 0)
    (h : st.clusters.length ≤ cap) : (train cap st toks).1.clusters.length ≤ cap := by
  unfold train
  split
  · simpa using h
  · simp only
    unfold evict
    split
    · rename_i hover
      split
      · rename_i v hv
        simp only
        obtain ⟨c, hc, hseq⟩ := oldest_mem _ hv
        have hlt : ((st.clusters ++ [(⟨st.nextSeq, toks, 1, st.tick + 1⟩ : Cluster)]).filter
            (·.seq ≠ v)).length <
            (st.clusters ++ [(⟨st.nextSeq, toks, 1, st.tick + 1⟩ : Cluster)]).length :=
          List.length_filter_lt_length_iff_exists.mpr ⟨c, hc, by simp [hseq]⟩
        simp only [List.length_append, List.length_singleton] at hlt
        omega
      · rename_i hv
        have := oldest_isSome (st.clusters ++ [(⟨st.nextSeq, toks, 1, st.tick + 1⟩ : Cluster)])
          (by simp)
        rw [hv] at this; simp at this
    · rename_i hnot
      simp only [List.length_append, List.length_singleton] at hnot ⊢
      omega

end OarfishDrain

#print axioms OarfishDrain.search_sound
#print axioms OarfishDrain.train_inv
#print axioms OarfishDrain.trainAll_inv
#print axioms OarfishDrain.train_bound
