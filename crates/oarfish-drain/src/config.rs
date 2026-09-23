//! What the clusterer is allowed to assume, in one place.
//!
//! Every field documents why it holds the value it holds, because the next
//! person to touch these will be tuning recall against snapshots at midnight.

/// Similarity, tree shape and table bounds. See the design §4 for the full
/// rationale; the one-line version is next to each field.
#[derive(Debug, Clone)]
pub struct Config {
    /// Minimum equal-tokens-over-total to join a cluster. 0.90, from the
    /// parent spec: over-split rather than merge `succeeded` into `failed`.
    pub similarity: f64,
    /// How many leading tokens form the search path (Loki's LogClusterDepth).
    /// 8: masked leading tokens are stable constants, so prefixes can be
    /// trusted further than drain3's 4, without Loki's 30.
    pub depth: usize,
    /// Fan-out cap per tree level before collapsing to the parameter node.
    /// 100, drain3's default; hostile input cannot blow up the tree.
    pub max_children: usize,
    /// Hard cap on clusters, LRU-evicted past it (Task 2). 65_536: above the
    /// few-thousand-template steady state, bounded under explosion.
    pub max_clusters: usize,
    /// What a generalized position renders as. `<*>`: drain3's convention,
    /// deliberately distinct from typed `<VAR:NAME>` slots.
    pub param: String,
    /// Lines past this many tokens cluster on their prefix. 128: bounds
    /// per-cluster memory while keeping the function total.
    pub max_tokens: usize,
    /// The floor of merge referral in [`Drain::neighbours`](crate::Drain::neighbours).
    /// 0.65: pairs scoring below this are different events, not close ones.
    /// There is no ceiling: a high Jaccard does not mean Drain merged the
    /// pair, since Drain never compares across token counts or tree paths.
    pub referral_floor: f64,
    /// Most clusters one tree leaf may hold; past it, the leaf's least
    /// recently used cluster is evicted. 0 means uncapped. Search scores
    /// every cluster in the line's leaf, so this bounds one line's work at
    /// `max_leaf_clusters × max_tokens` token comparisons. Without it the
    /// bound is `max_clusters × max_tokens` (`line_cost_bounded` in the Lean
    /// model), and one source sharing a prefix can reach it.
    pub max_leaf_clusters: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            similarity: 0.90,
            depth: 8,
            max_children: 100,
            max_clusters: 65_536,
            param: "<*>".to_owned(),
            max_tokens: 128,
            referral_floor: 0.65,
            max_leaf_clusters: 128,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Drain;
    use crate::DrainError;

    use super::*;

    #[test]
    fn defaults_match_the_design() {
        let config = Config::default();
        assert_eq!(config.similarity, 0.90);
        assert_eq!(config.depth, 8);
        assert_eq!(config.max_children, 100);
        assert_eq!(config.max_clusters, 65_536);
        assert_eq!(config.param, "<*>");
        assert_eq!(config.max_tokens, 128);
        assert_eq!(config.referral_floor, 0.65);
        assert_eq!(config.max_leaf_clusters, 128);
    }

    #[test]
    fn depth_below_three_is_rejected() {
        assert!(matches!(
            Drain::new(Config {
                depth: 2,
                ..Config::default()
            }),
            Err(DrainError::DepthBelowMinimum(2))
        ));
    }
}
