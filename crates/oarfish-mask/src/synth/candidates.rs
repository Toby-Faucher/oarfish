//! Finding what the bundle doesn't cover: mask the corpus, cluster it with a
//! throwaway Drain table, and find token positions Drain generalized to
//! `<*>` that aren't already a `<VAR:NAME>` placeholder. That is mechanical
//! evidence of a variable the bundle doesn't know about — not a guess from
//! sampling raw lines.

use std::collections::HashMap;

use oarfish_drain::{Config, Drain};

use crate::Bundle;

/// How many tokens of constant context to keep on each side of a candidate
/// position.
const CONTEXT_WIDTH: usize = 1;
/// At most this many distinct example values are kept per candidate.
const MAX_EXAMPLES: usize = 5;

/// One position in a cluster's template the bundle doesn't already mask: the
/// constant tokens around it, a handful of the real values seen there, and
/// how many corpus lines contributed to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Tokens immediately before the position.
    pub before: Vec<String>,
    /// Tokens immediately after the position.
    pub after: Vec<String>,
    /// Distinct real values seen at this position, first-seen order.
    pub examples: Vec<String>,
    /// How many corpus lines matched the cluster this position came from.
    pub occurrences: u64,
}

/// Mask every corpus line with `bundle`, cluster the result with a throwaway
/// Drain table at the daemon's own default config, and find every token
/// position Drain generalized to `<*>` that isn't already a `<VAR:NAME>`
/// placeholder. Kept above `min_occurrences`, highest-occurrence first.
pub fn find_candidates(bundle: &Bundle, corpus: &[String], min_occurrences: u64) -> Vec<Candidate> {
    let mut drain = Drain::new(Config::default()).expect("default config is valid");
    let mut lines_by_cluster: HashMap<u64, Vec<Vec<String>>> = HashMap::new();

    for line in corpus {
        let masked = bundle.mask(line);
        let tokens: Vec<String> = masked
            .template()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let assignment = drain.train(masked.template());
        lines_by_cluster
            .entry(assignment.seq)
            .or_default()
            .push(tokens);
    }

    let mut candidates = Vec::new();
    for cluster in drain.clusters() {
        if cluster.size < min_occurrences {
            continue;
        }
        let Some(lines) = lines_by_cluster.get(&cluster.seq) else {
            continue;
        };
        for (position, token) in cluster.tokens.iter().enumerate() {
            if token != "<*>" {
                continue;
            }
            let mut examples: Vec<String> = Vec::new();
            for line in lines {
                let Some(value) = line.get(position) else {
                    continue;
                };
                if value.starts_with("<VAR:") && value.ends_with('>') {
                    // Two different already-masked slot types happened to
                    // land at the same position across a close cluster;
                    // that is not a gap the bundle needs filling.
                    continue;
                }
                if !examples.contains(value) {
                    examples.push(value.clone());
                }
                if examples.len() >= MAX_EXAMPLES {
                    break;
                }
            }
            if examples.is_empty() {
                continue;
            }
            let before_start = position.saturating_sub(CONTEXT_WIDTH);
            let before = cluster.tokens[before_start..position].to_vec();
            let after_end = (position + 1 + CONTEXT_WIDTH).min(cluster.tokens.len());
            let after = cluster.tokens[position + 1..after_end].to_vec();
            candidates.push(Candidate {
                before,
                after,
                examples,
                occurrences: cluster.size,
            });
        }
    }
    candidates.sort_by_key(|a| std::cmp::Reverse(a.occurrences));
    candidates
}

#[cfg(test)]
mod tests {
    use crate::curated;

    use super::*;

    fn corpus() -> Vec<String> {
        // Ten tokens: a single varying position is 9/10 similarity, exactly
        // the daemon's 0.90 threshold, so Drain merges these. Three-token
        // lines (`batch xk92 completed`) only reach 2/3 and would never
        // cluster — see tree.rs `the_threshold_is_inclusive_at_exactly_ninety_percent`.
        vec![
            "batch xk92 completed extra alpha beta gamma delta epsilon zeta".to_owned(),
            "batch qm14 completed extra alpha beta gamma delta epsilon zeta".to_owned(),
            "batch zt77 completed extra alpha beta gamma delta epsilon zeta".to_owned(),
            "batch xk92 completed extra alpha beta gamma delta epsilon zeta".to_owned(),
        ]
    }

    #[test]
    fn an_unmasked_position_becomes_one_candidate_with_its_examples() {
        let candidates = find_candidates(curated(), &corpus(), 3);
        assert_eq!(candidates.len(), 1, "got {candidates:?}");
        let candidate = &candidates[0];
        assert_eq!(candidate.before, vec!["batch".to_owned()]);
        assert_eq!(candidate.after, vec!["completed".to_owned()]);
        assert_eq!(candidate.occurrences, 4);
        for value in ["xk92", "qm14", "zt77"] {
            assert!(
                candidate.examples.contains(&value.to_owned()),
                "missing {value} in {:?}",
                candidate.examples
            );
        }
    }

    #[test]
    fn below_min_occurrences_yields_nothing() {
        let candidates = find_candidates(curated(), &corpus(), 5);
        assert!(candidates.is_empty(), "got {candidates:?}");
    }

    #[test]
    fn a_position_the_bundle_already_covers_is_never_a_candidate() {
        // IP4 already masks this uniformly; Drain never generalizes it,
        // since every line gets the identical `<VAR:IP4>` text.
        let corpus = vec![
            "connect from 10.0.0.1 refused extra alpha beta gamma delta".to_owned(),
            "connect from 10.0.0.2 refused extra alpha beta gamma delta".to_owned(),
            "connect from 10.0.0.3 refused extra alpha beta gamma delta".to_owned(),
        ];
        let candidates = find_candidates(curated(), &corpus, 3);
        assert!(candidates.is_empty(), "got {candidates:?}");
    }
}
