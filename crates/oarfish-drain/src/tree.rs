//! The prefix tree: count level, then leading-token levels, then candidates.
//!
//! Ported from `treeSearch` / `addSeqToPrefixTree` in Loki's
//! `pkg/pattern/drain/drain.go`. Children are a scanned `Vec`, not a map:
//! fan-out is capped, most nodes hold a single edge, and Loki benchmarked the
//! scan faster up to about four children.

use crate::Config;

pub(crate) struct Node {
    children: Vec<(String, Node)>,
    pub(crate) cluster_ids: Vec<u64>,
}

impl Node {
    pub(crate) fn new() -> Self {
        Self {
            children: Vec::new(),
            cluster_ids: Vec::new(),
        }
    }

    fn child(&self, key: &str) -> Option<&Node> {
        self.children.iter().find(|(k, _)| k == key).map(|(_, n)| n)
    }

    fn child_mut(&mut self, key: &str) -> Option<&mut Node> {
        self.children
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, n)| n)
    }
}

pub(crate) fn max_node_depth(config: &Config) -> usize {
    config.depth - 2
}

/// Walk to the leaf whose candidates may match, or `None` when no path fits.
pub(crate) fn search<'a>(root: &'a Node, tokens: &[String], config: &Config) -> Option<&'a Node> {
    let count = root.child(&tokens.len().to_string())?;
    if tokens.len() < 2 {
        return Some(count);
    }

    let mut node = count;
    for (node_depth, token) in (1..).zip(tokens.iter()) {
        if node_depth >= max_node_depth(config) || node_depth == tokens.len() {
            break;
        }
        node = node.child(token).or_else(|| node.child(&config.param))?;
    }
    Some(node)
}

/// Equal tokens over total, parameter positions in the cluster skipped rather
/// than counted. Returns the similarity and the parameter count (for
/// tie-breaking); lengths are equal by construction.
pub(crate) fn similarity(
    cluster_tokens: &[String],
    tokens: &[String],
    param: &str,
) -> (f64, usize) {
    debug_assert_eq!(cluster_tokens.len(), tokens.len());
    let mut similar = 0;
    let mut params = 0;
    for (ct, t) in cluster_tokens.iter().zip(tokens) {
        if ct == param {
            params += 1;
        } else if ct == t {
            similar += 1;
        }
    }
    (similar as f64 / cluster_tokens.len() as f64, params)
}

/// Differing positions become the parameter string, in place. Monotonic: a
/// position generalizes at most once, which bounds the id churn in design §5.
pub(crate) fn generalize(cluster_tokens: &mut [String], tokens: &[String], param: &str) {
    debug_assert_eq!(cluster_tokens.len(), tokens.len());
    for (ct, t) in cluster_tokens.iter_mut().zip(tokens) {
        if ct != t {
            *ct = param.to_owned();
        }
    }
}

fn has_numbers(token: &str) -> bool {
    token.chars().any(|c| c.is_numeric())
}

/// Insert a cluster id under its template's path. Tokens without digits earn
/// specific nodes; tokens with digits (including `<VAR:IP4>`) descend the
/// parameter path when one exists. (Loki's source comments label these two
/// branches backwards; the conditions are what is ported.)
pub(crate) fn insert(root: &mut Node, cluster_id: u64, template: &[String], config: &Config) {
    let count_key = template.len().to_string();
    if root.child(&count_key).is_none() {
        root.children.push((count_key.clone(), Node::new()));
    }
    let mut node = root.child_mut(&count_key).expect("just inserted");
    if template.is_empty() {
        node.cluster_ids.push(cluster_id);
        return;
    }

    for (node_depth, token) in (1..).zip(template.iter()) {
        if node_depth >= max_node_depth(config) || node_depth >= template.len() {
            node.cluster_ids.push(cluster_id);
            break;
        }

        if node.child(token).is_some() {
            node = node.child_mut(token).expect("just checked");
        } else if !has_numbers(token) {
            let has_param = node.child(&config.param).is_some();
            if has_param {
                if node.children.len() < config.max_children {
                    node.children.push((token.clone(), Node::new()));
                    node = node.child_mut(token).expect("just inserted");
                } else {
                    let param = config.param.clone();
                    node = node.child_mut(&param).expect("checked");
                }
            } else if node.children.len() + 1 < config.max_children {
                node.children.push((token.clone(), Node::new()));
                node = node.child_mut(token).expect("just inserted");
            } else if node.children.len() + 1 == config.max_children {
                node.children.push((config.param.clone(), Node::new()));
                let param = config.param.clone();
                node = node.child_mut(&param).expect("just inserted");
            } else {
                let param = config.param.clone();
                node = node
                    .child_mut(&param)
                    .expect("exists: else-branch means over cap");
            }
        } else if node.child(&config.param).is_none() {
            node.children.push((config.param.clone(), Node::new()));
            let param = config.param.clone();
            node = node.child_mut(&param).expect("just inserted");
        } else {
            let param = config.param.clone();
            node = node.child_mut(&param).expect("checked");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Config, Drain};

    fn drain() -> Drain {
        Drain::new(Config::default()).expect("default config is valid")
    }

    #[test]
    fn identical_lines_share_a_cluster() {
        let mut drain = drain();
        let line = "sshd<VAR:PID>: Failed password for root from <VAR:IP4> port <VAR:NUM> ssh2";
        let first = drain.train(line);
        let second = drain.train(line);
        assert_eq!(first.seq, second.seq);
        assert_eq!(first.template, second.template);
        assert_eq!(second.size, 2);
    }

    /// The parent spec's own example, and the reason the threshold is 0.90.
    /// If this ever merges, an alarm has been silently deleted.
    #[test]
    fn succeeded_and_failed_never_share_a_cluster() {
        let mut drain = drain();
        let ok = drain.train("task <VAR:NUM> succeeded");
        let failed = drain.train("task <VAR:NUM> failed");
        assert_ne!(ok.seq, failed.seq);
        assert_ne!(ok.template, failed.template);
    }

    #[test]
    fn different_token_counts_never_share_a_cluster() {
        let mut drain = drain();
        let short = drain.train("a b c d");
        let long = drain.train("a b c d e");
        assert_ne!(short.seq, long.seq);
    }

    #[test]
    fn one_differing_token_generalizes_to_a_parameter() {
        // Ten tokens differing only at the last: same tree path, 9/10
        // similarity, so the last position generalizes.
        let mut drain = drain();
        drain.train("user alice logged in from <VAR:IP4> at dawn today ok");
        drain.train("user alice logged in from <VAR:IP4> at dawn today fine");
        let cluster = drain.get(1).expect("cluster 1");
        assert_eq!(
            cluster.template,
            "user alice logged in from <VAR:IP4> at dawn today <*>"
        );
        assert_eq!(cluster.size, 2);
    }

    /// 9/10 equal tokens is exactly 0.90: merges. 8/10 is 0.80: splits.
    /// This pins the threshold arithmetic, not just its direction.
    #[test]
    fn the_threshold_is_inclusive_at_exactly_ninety_percent() {
        let mut drain1 = drain();
        let base = "t1 t2 t3 t4 t5 t6 t7 t8 t9 t10";
        let nine = "t1 t2 t3 t4 t5 t6 t7 t8 t9 CHANGED";
        let eight = "t1 t2 t3 t4 t5 t6 t7 t8 CHANGED CHANGED";

        let a = drain1.train(base);
        let b = drain1.train(nine);
        assert_eq!(a.seq, b.seq, "9/10 must reach the 0.90 threshold");

        let mut drain2 = drain();
        let a = drain2.train(base);
        let c = drain2.train(eight);
        assert_ne!(a.seq, c.seq, "8/10 must not reach the 0.90 threshold");
    }

    #[test]
    fn an_empty_line_gets_the_empty_cluster() {
        let mut drain = drain();
        let assignment = drain.train("");
        let cluster = drain.get(assignment.seq).expect("empty cluster");
        assert_eq!(cluster.template, "");
        // And it is stable: a second empty line joins it.
        assert_eq!(drain.train("").seq, assignment.seq);
    }

    proptest::proptest! {
        /// The same sequence of lines always builds the same table. The cost
        /// model rests on a template judged once staying judged.
        #[test]
        fn training_is_deterministic(
            lines in proptest::collection::vec("([a-z]{1,8} ){1,6}[a-z]{1,8}", 1..20),
        ) {
            let run = |lines: &[String]| {
                let mut drain = drain();
                lines.iter().map(|l| drain.train(l).template).collect::<Vec<_>>()
            };
            proptest::prop_assert_eq!(run(&lines), run(&lines));
        }

        /// A line a cluster already matches never moves its id. This is the
        /// §5 promise that matters: the judged past does not churn.
        #[test]
        fn matched_lines_never_move_a_settled_id(
            prefix in "[a-z]{1,8}",
            var in "[a-z]{1,8}",
        ) {
            let mut drain = drain();
            let first = format!("{prefix} alpha {var} gamma");
            let id = drain.train(&first).template;
            for word in ["alpha", "beta", "gamma", "delta", "epsilon"] {
                let line = format!("{prefix} {word} {var} gamma");
                let _ = drain.train(&line);
            }
            // The original line still resolves to a cluster whose template is
            // stable for it: retraining yields the same template id.
            let again = drain.train(&first).template;
            proptest::prop_assert_eq!(again, id);
        }
    }
}
