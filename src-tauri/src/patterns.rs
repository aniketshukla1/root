//! Log clustering with the Drain algorithm.
//!
//! Reference: He, Zhu, Zheng, Lyu — "Drain: An Online Log Parsing Approach with
//! Fixed Depth Tree" (ICWS 2017). Grafana Loki ships a Drain-based pattern
//! ingester and Coroot does its event clustering the same way; this is the same
//! idea, sized for a desktop tool.
//!
//! The point is that nobody can read a log tail. Ten thousand lines collapse to
//! a few dozen *templates* — the message with its variable parts replaced by
//! `<*>` — which can be ranked, counted, and sparklined. The failing line you
//! actually care about is then one row, not something you scroll past.
//!
//! Two-stage design, and both stages matter:
//!
//! 1. **Masking** (this module's `mask_token`) is a conservative pre-pass that
//!    only rewrites tokens that are unambiguously variable — numbers, IPs,
//!    UUIDs, hashes, timestamps. It deliberately leaves things like `/v0/orders`
//!    and `worker-3` alone, because over-masking destroys the distinctions that
//!    make a pattern worth reading.
//! 2. **Merging** (the tree below) catches the rest. When two lines reach the
//!    same leaf and are similar enough, the positions where they disagree become
//!    wildcards. So `worker-3` and `worker-5` still collapse — via evidence that
//!    they vary, rather than via a guess up front.
//!
//! Clustering is online and single-pass: no training, no corpus, no config.

use std::collections::HashMap;

/// Stands in for a variable token in a template.
pub const WILDCARD: &str = "<*>";

#[derive(Debug, Clone)]
pub struct DrainConfig {
    /// How many leading tokens are used to index the tree. The paper's depth
    /// parameter; 2 keeps the tree shallow enough that similar lines meet.
    pub prefix_depth: usize,
    /// Fraction of positions that must match for a line to join a cluster.
    /// Below this a new cluster is created. The paper's `st`.
    pub sim_threshold: f64,
    /// Distinct children before an internal node collapses into its wildcard
    /// branch. Bounds the tree when a position is effectively unbounded.
    pub max_children: usize,
    /// Hard cap on clusters, so a pathological stream cannot exhaust memory.
    pub max_clusters: usize,
}

impl Default for DrainConfig {
    fn default() -> Self {
        DrainConfig {
            prefix_depth: 2,
            sim_threshold: 0.4,
            max_children: 100,
            max_clusters: 2000,
        }
    }
}

/// One discovered message template and how many lines matched it.
#[derive(Debug, Clone)]
pub struct Cluster {
    pub template: Vec<String>,
    pub count: usize,
}

impl Cluster {
    /// The template as it is shown and as it round-trips through the filter.
    pub fn template_string(&self) -> String {
        self.template.join(" ")
    }
}

enum Node {
    Internal(HashMap<String, usize>),
    Leaf(Vec<usize>),
}

pub struct Drain {
    cfg: DrainConfig,
    nodes: Vec<Node>,
    /// Token count is the first level of the tree: lines of different lengths
    /// are never the same message, so this partition is free accuracy.
    roots: HashMap<usize, usize>,
    clusters: Vec<Cluster>,
}

impl Drain {
    pub fn new(cfg: DrainConfig) -> Self {
        Drain {
            cfg,
            nodes: Vec::new(),
            roots: HashMap::new(),
            clusters: Vec::new(),
        }
    }

    pub fn clusters(&self) -> &[Cluster] {
        &self.clusters
    }

    /// Feed one message; returns the index of the cluster it belongs to.
    ///
    /// `None` only for a message with no tokens at all, which carries no
    /// information and should not create a cluster.
    pub fn add(&mut self, message: &str) -> Option<usize> {
        let tokens = tokenize(message);
        if tokens.is_empty() {
            return None;
        }

        let leaf = self.descend(&tokens);
        let Node::Leaf(candidates) = &self.nodes[leaf] else {
            unreachable!("descend always ends on a leaf");
        };

        if let Some(hit) = self.best_match(candidates, &tokens) {
            // Positions that disagree are evidence of a variable, so widen.
            let cluster = &mut self.clusters[hit];
            for (i, tok) in tokens.iter().enumerate() {
                if cluster.template[i] != *tok && cluster.template[i] != WILDCARD {
                    cluster.template[i] = WILDCARD.to_string();
                }
            }
            cluster.count += 1;
            return Some(hit);
        }

        if self.clusters.len() >= self.cfg.max_clusters {
            // Over the cap we stop splitting and fold into the closest cluster
            // we have, so counts stay honest even though templates stop
            // refining. Preferable to silently dropping lines.
            return self.loosest_match(&tokens);
        }

        let id = self.clusters.len();
        self.clusters.push(Cluster {
            template: tokens,
            count: 1,
        });
        if let Node::Leaf(list) = &mut self.nodes[leaf] {
            list.push(id);
        }
        Some(id)
    }

    /// Walk to the leaf for these tokens, creating nodes on the way.
    fn descend(&mut self, tokens: &[String]) -> usize {
        let depth = self.cfg.prefix_depth.min(tokens.len());
        let mut current = match self.roots.get(&tokens.len()) {
            Some(&idx) => idx,
            None => {
                let idx = self.push_node(depth == 0);
                self.roots.insert(tokens.len(), idx);
                idx
            }
        };

        for (level, token) in tokens.iter().take(depth).enumerate() {
            let last = level + 1 == depth;

            // A token carrying digits is a poor index — it fragments the tree
            // into near-duplicate branches — so it rides the wildcard branch.
            let key = if has_digit(token) {
                WILDCARD.to_string()
            } else {
                token.clone()
            };

            let Node::Internal(children) = &self.nodes[current] else {
                return current;
            };

            let next = match children.get(&key) {
                Some(&idx) => idx,
                None => {
                    // Past the fan-out budget everything shares one branch,
                    // rather than growing without bound.
                    if children.len() >= self.cfg.max_children {
                        match children.get(WILDCARD) {
                            Some(&idx) => idx,
                            None => {
                                let idx = self.push_node(last);
                                self.insert_child(current, WILDCARD.to_string(), idx);
                                idx
                            }
                        }
                    } else {
                        let idx = self.push_node(last);
                        self.insert_child(current, key, idx);
                        idx
                    }
                }
            };
            current = next;
        }

        current
    }

    fn push_node(&mut self, leaf: bool) -> usize {
        self.nodes.push(if leaf {
            Node::Leaf(Vec::new())
        } else {
            Node::Internal(HashMap::new())
        });
        self.nodes.len() - 1
    }

    fn insert_child(&mut self, parent: usize, key: String, child: usize) {
        if let Node::Internal(children) = &mut self.nodes[parent] {
            children.insert(key, child);
        }
    }

    /// Best candidate at or above the similarity threshold.
    ///
    /// Ties break toward the more specific template (fewer wildcards), so a
    /// line prefers an exact home over one that has already been widened.
    fn best_match(&self, candidates: &[usize], tokens: &[String]) -> Option<usize> {
        let mut best: Option<(usize, f64, usize)> = None;
        for &id in candidates {
            let cluster = &self.clusters[id];
            if cluster.template.len() != tokens.len() {
                continue;
            }
            let (sim, wildcards) = similarity(&cluster.template, tokens);
            if sim < self.cfg.sim_threshold {
                continue;
            }
            let better = match best {
                None => true,
                Some((_, best_sim, best_wild)) => {
                    sim > best_sim || (sim == best_sim && wildcards < best_wild)
                }
            };
            if better {
                best = Some((id, sim, wildcards));
            }
        }
        best.map(|(id, _, _)| id)
    }

    /// Fallback once the cluster cap is hit: closest same-length template.
    fn loosest_match(&mut self, tokens: &[String]) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for (id, cluster) in self.clusters.iter().enumerate() {
            if cluster.template.len() != tokens.len() {
                continue;
            }
            let (sim, _) = similarity(&cluster.template, tokens);
            if best.is_none_or(|(_, s)| sim > s) {
                best = Some((id, sim));
            }
        }
        let id = best.map(|(id, _)| id)?;
        self.clusters[id].count += 1;
        Some(id)
    }
}

/// Fraction of positions that agree, plus the template's wildcard count.
///
/// A wildcard position is not counted as agreement — otherwise a fully widened
/// template would absorb every line of its length.
fn similarity(template: &[String], tokens: &[String]) -> (f64, usize) {
    let mut matches = 0usize;
    let mut wildcards = 0usize;
    for (t, tok) in template.iter().zip(tokens) {
        if t == WILDCARD {
            wildcards += 1;
        } else if t == tok {
            matches += 1;
        }
    }
    if template.is_empty() {
        return (0.0, wildcards);
    }
    (matches as f64 / template.len() as f64, wildcards)
}

/// Split on whitespace and mask the tokens that are obviously variable.
pub fn tokenize(message: &str) -> Vec<String> {
    message
        .split_whitespace()
        .map(mask_token)
        .collect::<Vec<_>>()
}

/// Whether a whole token should collapse to a wildcard before clustering.
///
/// Conservative by design — see the module docs. Anything ambiguous is left for
/// the merge step to decide from evidence.
fn mask_token(token: &str) -> String {
    let core = token.trim_matches(|c: char| {
        matches!(c, ',' | ';' | '.' | ':' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}')
    });
    if core.is_empty() {
        return token.to_string();
    }

    if is_uuid(core) || is_ipv4ish(core) || is_number_like(core) || is_hash(core) || is_timestamp(core)
    {
        return WILDCARD.to_string();
    }
    token.to_string()
}

fn has_digit(token: &str) -> bool {
    token.bytes().any(|b| b.is_ascii_digit())
}

/// 8-4-4-4-12 hex.
fn is_uuid(token: &str) -> bool {
    let groups: Vec<&str> = token.split('-').collect();
    groups.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(len, g)| g.len() == *len && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Dotted quad, optionally with a port. Also catches version-like quads, which
/// vary just as often and are just as uninteresting inside a template.
fn is_ipv4ish(token: &str) -> bool {
    let host = token.split(':').next().unwrap_or(token);
    let octets: Vec<&str> = host.split('.').collect();
    octets.len() == 4
        && octets
            .iter()
            .all(|o| !o.is_empty() && o.bytes().all(|b| b.is_ascii_digit()))
}

/// A number, with an optional sign and an optional short unit: `137`, `-4`,
/// `1.8s`, `93%`, `512MB`.
fn is_number_like(token: &str) -> bool {
    let body = token.strip_prefix(['-', '+']).unwrap_or(token);
    let digits_end = body
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '_')
        .unwrap_or(body.len());
    let (num, unit) = body.split_at(digits_end);

    if num.is_empty() || !num.bytes().any(|b| b.is_ascii_digit()) {
        return false;
    }
    if num.matches('.').count() > 1 {
        return false;
    }
    unit.is_empty() || (unit.len() <= 3 && unit.chars().all(|c| c.is_ascii_alphabetic()))
        || unit == "%"
}

/// Long hex runs: container ids, commit shas, request ids.
fn is_hash(token: &str) -> bool {
    let body = token.strip_prefix("0x").unwrap_or(token);
    body.len() >= 7 && body.bytes().all(|b| b.is_ascii_hexdigit()) && has_digit(body)
}

/// A leading `YYYY-MM-DD`, or a bare `HH:MM:SS`.
fn is_timestamp(token: &str) -> bool {
    let bytes = token.as_bytes();
    let ymd = bytes.len() >= 10
        && bytes[..10]
            .iter()
            .enumerate()
            .all(|(i, b)| if i == 4 || i == 7 { *b == b'-' } else { b.is_ascii_digit() });

    let hms = bytes.len() >= 8
        && bytes[..8]
            .iter()
            .enumerate()
            .all(|(i, b)| if i == 2 || i == 5 { *b == b':' } else { b.is_ascii_digit() });

    ymd || hms
}

/// Whether a message belongs to a template, used to filter the stream down to
/// one pattern's instances.
///
/// Matching against the template directly — rather than re-running the
/// clustering and hoping it lands identically — means the selection stays
/// stable while new lines arrive.
pub fn template_matches(template: &str, message: &str) -> bool {
    let want: Vec<&str> = template.split_whitespace().collect();
    let got = tokenize(message);
    if want.len() != got.len() {
        return false;
    }
    want.iter()
        .zip(&got)
        .all(|(w, g)| *w == WILDCARD || *w == g.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster_all(lines: &[&str]) -> Drain {
        let mut drain = Drain::new(DrainConfig::default());
        for line in lines {
            drain.add(line);
        }
        drain
    }

    #[test]
    fn collapses_lines_that_differ_only_in_variables() {
        let drain = cluster_all(&[
            "ERROR: connection refused upstream 10.0.4.19:8080",
            "ERROR: connection refused upstream 10.0.7.31:9090",
            "ERROR: connection refused upstream 10.0.1.02:8080",
        ]);
        assert_eq!(drain.clusters().len(), 1);
        assert_eq!(drain.clusters()[0].count, 3);
        assert_eq!(
            drain.clusters()[0].template_string(),
            "ERROR: connection refused upstream <*>"
        );
    }

    #[test]
    fn keeps_genuinely_different_messages_apart() {
        let drain = cluster_all(&[
            "PANIC: out of memory, killing worker",
            "cache hit ratio 0.994",
            "slow query detected: 1.8s",
        ]);
        assert_eq!(drain.clusters().len(), 3);
    }

    #[test]
    fn merge_step_widens_positions_that_vary() {
        // `worker-3` survives masking on purpose; only repeated evidence that
        // the position varies should turn it into a wildcard.
        let drain = cluster_all(&[
            "restarting service worker-3 after failure",
            "restarting service worker-5 after failure",
        ]);
        assert_eq!(drain.clusters().len(), 1);
        assert_eq!(
            drain.clusters()[0].template_string(),
            "restarting service <*> after failure"
        );
    }

    #[test]
    fn masking_leaves_low_digit_identifiers_alone() {
        // Over-masking these would erase the only interesting part of the line.
        assert_eq!(mask_token("/v0/orders"), "/v0/orders");
        assert_eq!(mask_token("worker-3"), "worker-3");
        assert_eq!(mask_token("checkout-api"), "checkout-api");
    }

    #[test]
    fn masking_catches_the_unambiguous_variables() {
        assert_eq!(mask_token("137"), WILDCARD);
        assert_eq!(mask_token("1.8s"), WILDCARD);
        assert_eq!(mask_token("93%"), WILDCARD);
        assert_eq!(mask_token("512MB"), WILDCARD);
        assert_eq!(mask_token("10.0.4.19:8080"), WILDCARD);
        assert_eq!(mask_token("f47ac10b-58cc-4372-a567-0e02b2c3d479"), WILDCARD);
        assert_eq!(mask_token("a3f9c2e1b7d4"), WILDCARD);
        assert_eq!(mask_token("2026-08-08"), WILDCARD);
        assert_eq!(mask_token("19:41:09"), WILDCARD);
    }

    #[test]
    fn different_lengths_never_merge() {
        let drain = cluster_all(&["disk full", "disk full on device sda1"]);
        assert_eq!(drain.clusters().len(), 2);
    }

    #[test]
    fn template_matching_round_trips() {
        let drain = cluster_all(&[
            "ERROR: connection refused upstream 10.0.4.19:8080",
            "ERROR: connection refused upstream 10.0.7.31:9090",
        ]);
        let template = drain.clusters()[0].template_string();

        assert!(template_matches(
            &template,
            "ERROR: connection refused upstream 10.9.9.9:1234"
        ));
        assert!(!template_matches(&template, "PANIC: out of memory"));
        // Same shape, different literal — must not match.
        assert!(!template_matches(
            &template,
            "ERROR: connection accepted upstream 10.9.9.9:1234"
        ));
    }

    #[test]
    fn empty_and_whitespace_messages_are_ignored() {
        let mut drain = Drain::new(DrainConfig::default());
        assert!(drain.add("").is_none());
        assert!(drain.add("   ").is_none());
        assert!(drain.clusters().is_empty());
    }

    #[test]
    fn respects_the_cluster_cap() {
        let cfg = DrainConfig {
            max_clusters: 5,
            // Force every line into its own cluster up to the cap.
            sim_threshold: 0.99,
            ..Default::default()
        };
        let mut drain = Drain::new(cfg);
        for i in 0..50 {
            drain.add(&format!("unique message alpha{i} beta{i} gamma{i}"));
        }
        assert!(drain.clusters().len() <= 5);
        // Nothing was dropped: every line landed in some cluster.
        let total: usize = drain.clusters().iter().map(|c| c.count).sum();
        assert_eq!(total, 50);
    }

    #[test]
    fn handles_a_realistic_mixed_stream() {
        let mut lines = Vec::new();
        for i in 0..200 {
            lines.push(format!("ERROR: connection refused upstream 10.0.4.{i}:8080"));
        }
        for i in 0..40 {
            lines.push(format!("slow query detected: {i}.8s"));
        }
        lines.push("PANIC: out of memory, killing worker".to_string());

        let mut drain = Drain::new(DrainConfig::default());
        for line in &lines {
            drain.add(line);
        }

        // 241 lines, a handful of shapes — the whole point of the exercise.
        assert!(
            drain.clusters().len() <= 5,
            "expected a handful of templates, got {}",
            drain.clusters().len()
        );
        let total: usize = drain.clusters().iter().map(|c| c.count).sum();
        assert_eq!(total, 241);

        let biggest = drain.clusters().iter().map(|c| c.count).max().unwrap();
        assert_eq!(biggest, 200);
    }
}
