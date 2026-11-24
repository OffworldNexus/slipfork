//! The pure `BuildGraph`: unique keys, present parents, waves, head, closures (§6.1).

use std::collections::{BTreeMap, BTreeSet};

use super::{NodeKey, NodeKind, SnapshotError, SnapshotResult};

/// One desired node; `line` is the `Forkfile`/`Dockerfile` line that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Content key.
    pub key: NodeKey,
    /// What the node stands for.
    pub kind: NodeKind,
    /// Parents in declared order.
    pub parents: Vec<NodeKey>,
    /// Canonical text this key was derived from.
    pub canon: String,
    /// Source line.
    pub line: u32,
}

/// A disk-free DAG with exactly one head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildGraph {
    head: NodeKey,
    nodes: BTreeMap<NodeKey, Node>,
    waves: Vec<Vec<NodeKey>>,
}

impl BuildGraph {
    /// Validates unique keys, present parents and acyclicity, then freezes the wave schedule.
    pub fn new(head: NodeKey, nodes: Vec<Node>) -> SnapshotResult<Self> {
        let mut map = BTreeMap::new();
        for node in nodes {
            if map.insert(node.key, node.clone()).is_some() {
                return Err(SnapshotError::Graph(format!(
                    "node {} appears more than once\nnext: keep one node per content key and re-run `slipfork up`",
                    node.key
                )));
            }
        }
        if !map.contains_key(&head) {
            return Err(SnapshotError::Graph(format!(
                "head {head} is not a node\nnext: rebuild the plan from the Forkfile instead of editing keys by hand"
            )));
        }
        for node in map.values() {
            for parent in &node.parents {
                if !map.contains_key(parent) {
                    return Err(SnapshotError::Graph(format!(
                        "node {} names missing parent {parent}\nnext: include every parent node in the graph and re-run `slipfork up`",
                        node.key
                    )));
                }
            }
        }
        let waves = waves(&map)?;
        Ok(Self {
            head,
            nodes: map,
            waves,
        })
    }

    /// The graph head (the `world` key for a world, the `image` key for an image).
    #[must_use]
    pub const fn head(&self) -> NodeKey {
        self.head
    }

    /// One node by key.
    #[must_use]
    pub fn node(&self, key: &NodeKey) -> Option<&Node> {
        self.nodes.get(key)
    }

    /// Every node, key-ordered.
    #[must_use]
    pub const fn nodes(&self) -> &BTreeMap<NodeKey, Node> {
        &self.nodes
    }

    /// Topological waves; siblings are key-ordered.
    #[must_use]
    pub fn waves(&self) -> &[Vec<NodeKey>] {
        &self.waves
    }

    /// Whether a kind owns a sealed subvolume.
    #[must_use]
    pub const fn is_materialized(kind: NodeKind) -> bool {
        kind.is_materialized()
    }

    /// Every materialized key in wave order (base/external/step/seed only).
    #[must_use]
    pub fn materialized(&self) -> Vec<NodeKey> {
        self.waves
            .iter()
            .flatten()
            .filter(|key| {
                self.nodes
                    .get(key)
                    .is_some_and(|node| node.kind.is_materialized())
            })
            .copied()
            .collect()
    }

    /// The parent-transitive closure of `root`, including `root` itself.
    #[must_use]
    pub fn closure(&self, root: &NodeKey) -> BTreeSet<NodeKey> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![*root];
        while let Some(key) = stack.pop() {
            if !seen.insert(key) {
                continue;
            }
            if let Some(node) = self.nodes.get(&key) {
                stack.extend(node.parents.iter().copied());
            }
        }
        seen
    }
}

fn waves(nodes: &BTreeMap<NodeKey, Node>) -> SnapshotResult<Vec<Vec<NodeKey>>> {
    let mut indegree: BTreeMap<NodeKey, usize> = nodes.keys().map(|key| (*key, 0)).collect();
    let mut children: BTreeMap<NodeKey, Vec<NodeKey>> = BTreeMap::new();
    for node in nodes.values() {
        for parent in &node.parents {
            if let Some(degree) = indegree.get_mut(&node.key) {
                *degree += 1;
            }
            children.entry(*parent).or_default().push(node.key);
        }
    }
    let mut ready: BTreeSet<NodeKey> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(key, _)| *key)
        .collect();
    let mut waves = Vec::new();
    let mut emitted = 0usize;
    while !ready.is_empty() {
        let wave: Vec<NodeKey> = ready.iter().copied().collect();
        emitted += wave.len();
        let mut next = BTreeSet::new();
        for key in &wave {
            if let Some(kids) = children.get(key) {
                for kid in kids {
                    if let Some(degree) = indegree.get_mut(kid) {
                        *degree -= 1;
                        if *degree == 0 {
                            next.insert(*kid);
                        }
                    }
                }
            }
        }
        ready = next;
        waves.push(wave);
    }
    if emitted != nodes.len() {
        return Err(SnapshotError::Graph(
            "the node graph contains a cycle\nnext: remove the circular `COPY --from`/step dependency and re-run `slipfork up`".to_owned(),
        ));
    }
    Ok(waves)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> NodeKey {
        NodeKey::from_digest([byte; 32])
    }

    fn node(key: NodeKey, kind: NodeKind, parents: Vec<NodeKey>, canon: &str, line: u32) -> Node {
        Node {
            key,
            kind,
            parents,
            canon: canon.to_owned(),
            line,
        }
    }

    #[test]
    fn diamond_waves_materialized_and_closure() {
        let (a, b, c, d) = (key(1), key(2), key(3), key(4));
        let graph = BuildGraph::new(
            d,
            vec![
                node(a, NodeKind::Base, vec![], "FROM a", 1),
                node(b, NodeKind::Step, vec![a], "RUN b", 2),
                node(c, NodeKind::Step, vec![a], "RUN c", 3),
                node(d, NodeKind::Image, vec![b, c], "image d", 4),
            ],
        )
        .expect("diamond is a DAG");
        assert_eq!(graph.head(), d);
        let waves = graph.waves();
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0], vec![a]);
        assert_eq!(
            waves[1].iter().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([b, c])
        );
        assert_eq!(waves[2], vec![d]);
        let materialized = graph.materialized();
        assert_eq!(materialized.first(), Some(&a));
        assert_eq!(
            materialized.iter().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([a, b, c])
        );
        assert!(!BuildGraph::is_materialized(NodeKind::Image));
        assert_eq!(graph.closure(&d), BTreeSet::from([a, b, c, d]));
    }

    #[test]
    fn cycles_and_missing_parents_are_refused() {
        let (a, b) = (key(1), key(2));
        let cycle = BuildGraph::new(
            a,
            vec![
                node(a, NodeKind::Step, vec![b], "RUN a", 1),
                node(b, NodeKind::Step, vec![a], "RUN b", 2),
            ],
        )
        .expect_err("a cycle must be refused");
        assert!(matches!(cycle, SnapshotError::Graph(_)));
        let missing = BuildGraph::new(a, vec![node(a, NodeKind::Step, vec![b], "RUN a", 1)])
            .expect_err("a missing parent must be refused");
        assert!(matches!(missing, SnapshotError::Graph(_)));
        let duplicate = BuildGraph::new(
            a,
            vec![
                node(a, NodeKind::Base, vec![], "FROM a", 1),
                node(a, NodeKind::Base, vec![], "FROM a", 1),
            ],
        )
        .expect_err("duplicate keys must be refused");
        assert!(matches!(duplicate, SnapshotError::Graph(_)));
    }
}
