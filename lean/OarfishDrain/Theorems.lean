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

end OarfishDrain

#print axioms OarfishDrain.search_sound
