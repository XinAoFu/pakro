//! Bounded, iterative reachability and SCC planning over descriptive facts.
//! No filesystem handle or removal capability can enter this module.
use eyre::{Result, ensure};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(in crate::programs::uninstall) struct Graph<N> {
    edges: BTreeMap<N, Vec<N>>,
}

impl<N: Ord + Clone> Graph<N> {
    pub(in crate::programs::uninstall) fn new(
        mut edges: BTreeMap<N, Vec<N>>,
        max_nodes: usize,
        max_edges: usize,
    ) -> Result<Self> {
        ensure!(
            edges.len() <= max_nodes,
            "reclamation graph node budget exceeded"
        );
        let mut total = 0usize;
        for adjacent in edges.values_mut() {
            total = total
                .checked_add(adjacent.len())
                .filter(|value| *value <= max_edges)
                .ok_or_else(|| eyre::eyre!("reclamation graph edge budget exceeded"))?;
            adjacent.sort();
            adjacent.dedup();
        }
        ensure!(
            edges
                .values()
                .flatten()
                .all(|node| edges.contains_key(node)),
            "reclamation dependency is missing; outgoing references are unknown"
        );
        Ok(Self { edges })
    }

    pub(in crate::programs::uninstall) fn closure(
        &self,
        roots: impl IntoIterator<Item = N>,
    ) -> Result<BTreeSet<N>> {
        let mut reached = BTreeSet::new();
        let mut pending = VecDeque::new();
        for root in roots {
            ensure!(
                self.edges.contains_key(&root),
                "reclamation protection root is missing"
            );
            if reached.insert(root.clone()) {
                pending.push_back(root);
            }
        }
        while let Some(node) = pending.pop_front() {
            for dependency in &self.edges[&node] {
                if reached.insert(dependency.clone()) {
                    pending.push_back(dependency.clone());
                }
            }
        }
        Ok(reached)
    }

    /// Explain one retained root with an actual dependency path. A reverse
    /// BFS visits each edge once, including cycles, then selects the first
    /// reachable root in the caller's deterministic order.
    pub(in crate::programs::uninstall) fn reference_path(
        &self,
        roots: impl IntoIterator<Item = N>,
        target: &N,
    ) -> Result<Option<Vec<N>>> {
        ensure!(
            self.edges.contains_key(target),
            "reference target is missing"
        );
        let mut reverse: BTreeMap<N, Vec<N>> = self
            .edges
            .keys()
            .cloned()
            .map(|node| (node, Vec::new()))
            .collect();
        for (node, dependencies) in &self.edges {
            for dependency in dependencies {
                reverse
                    .get_mut(dependency)
                    .expect("validated dependency")
                    .push(node.clone());
            }
        }
        let mut reached = BTreeSet::from([target.clone()]);
        let mut pending = VecDeque::from([target.clone()]);
        let mut next = BTreeMap::new();
        while let Some(node) = pending.pop_front() {
            for parent in &reverse[&node] {
                if reached.insert(parent.clone()) {
                    next.insert(parent.clone(), node.clone());
                    pending.push_back(parent.clone());
                }
            }
        }
        for root in roots {
            ensure!(self.edges.contains_key(&root), "reference root is missing");
            if !reached.contains(&root) {
                continue;
            }
            let mut path = vec![root.clone()];
            let mut current = root;
            while &current != target {
                current = next
                    .get(&current)
                    .ok_or_else(|| eyre::eyre!("retained reference path is incomplete"))?
                    .clone();
                path.push(current.clone());
                ensure!(
                    path.len() <= self.edges.len(),
                    "retained reference path exceeds graph bounds"
                );
            }
            return Ok(Some(path));
        }
        Ok(None)
    }

    /// Kosaraju with explicit stacks. The condensation DAG is emitted with
    /// referrers first, so a failed/retained group can retain its dependencies.
    pub(in crate::programs::uninstall) fn groups(&self, retained: &BTreeSet<N>) -> Vec<Vec<N>> {
        let nodes: Vec<_> = self
            .edges
            .keys()
            .filter(|node| !retained.contains(*node))
            .cloned()
            .collect();
        let indexes: BTreeMap<_, _> = nodes
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, node)| (node, index))
            .collect();
        let mut adjacent = vec![Vec::new(); nodes.len()];
        let mut reverse = vec![Vec::new(); nodes.len()];
        for (index, node) in nodes.iter().enumerate() {
            for dependency in &self.edges[node] {
                if let Some(&other) = indexes.get(dependency) {
                    adjacent[index].push(other);
                    reverse[other].push(index);
                }
            }
        }
        let mut visited = vec![false; nodes.len()];
        let mut finish = Vec::with_capacity(nodes.len());
        for start in 0..nodes.len() {
            if visited[start] {
                continue;
            }
            visited[start] = true;
            let mut stack = vec![(start, 0usize)];
            while let Some((node, next)) = stack.last_mut() {
                if *next < adjacent[*node].len() {
                    let child = adjacent[*node][*next];
                    *next += 1;
                    if !visited[child] {
                        visited[child] = true;
                        stack.push((child, 0));
                    }
                } else {
                    let (node, _) = stack.pop().expect("nonempty stack");
                    finish.push(node);
                }
            }
        }
        let mut components = vec![usize::MAX; nodes.len()];
        let mut groups: Vec<Vec<N>> = Vec::new();
        for start in finish.into_iter().rev() {
            if components[start] != usize::MAX {
                continue;
            }
            let component = groups.len();
            let mut group = Vec::new();
            let mut stack = vec![start];
            components[start] = component;
            while let Some(node) = stack.pop() {
                group.push(nodes[node].clone());
                for &other in &reverse[node] {
                    if components[other] == usize::MAX {
                        components[other] = component;
                        stack.push(other);
                    }
                }
            }
            group.sort();
            groups.push(group);
        }
        let mut dag = vec![BTreeSet::new(); groups.len()];
        let mut incoming = vec![0usize; groups.len()];
        for (node, edges) in adjacent.iter().enumerate() {
            for &other in edges {
                let (from, to) = (components[node], components[other]);
                if from != to && dag[from].insert(to) {
                    incoming[to] += 1;
                }
            }
        }
        let mut ready: BTreeSet<_> = (0..groups.len())
            .filter(|&index| incoming[index] == 0)
            .map(|index| (groups[index][0].clone(), index))
            .collect();
        let mut result = Vec::with_capacity(groups.len());
        while let Some((_, index)) = ready.pop_first() {
            result.push(groups[index].clone());
            for &other in &dag[index] {
                incoming[other] -= 1;
                if incoming[other] == 0 {
                    ready.insert((groups[other][0].clone(), other));
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn graph(edges: &[(u8, &[u8])]) -> Graph<u8> {
        Graph::new(
            edges
                .iter()
                .map(|(node, edges)| (*node, edges.to_vec()))
                .collect(),
            64,
            128,
        )
        .unwrap()
    }
    #[test]
    fn p4_rooted_cycle_is_kept_and_unrooted_cycle_is_one_group() {
        let graph = graph(&[(1, &[2]), (2, &[1, 3]), (3, &[]), (4, &[5]), (5, &[4])]);
        let retained = graph.closure([1]).unwrap();
        assert_eq!(retained, BTreeSet::from([1, 2, 3]));
        assert_eq!(graph.groups(&retained), vec![vec![4, 5]]);
    }
    #[test]
    fn p4_referrers_precede_dependencies_and_busy_closure_keeps_them() {
        let graph = graph(&[(1, &[2]), (2, &[3]), (3, &[]), (4, &[2])]);
        let groups = graph.groups(&BTreeSet::new());
        for (before, after) in [(1, 2), (4, 2), (2, 3)] {
            assert!(
                groups.iter().position(|g| g.contains(&before)).unwrap()
                    < groups.iter().position(|g| g.contains(&after)).unwrap()
            );
        }
        assert_eq!(graph.closure([4]).unwrap(), BTreeSet::from([2, 3, 4]));
        assert_eq!(graph.groups(&graph.closure([4]).unwrap()), vec![vec![1]]);
    }
    #[test]
    fn p4_dangling_or_capacity_is_an_error_not_an_empty_graph() {
        assert!(Graph::new(BTreeMap::from([(1u8, vec![2])]), 8, 8).is_err());
        assert!(Graph::new(BTreeMap::from([(1u8, vec![])]), 0, 8).is_err());
        assert!(Graph::new(BTreeMap::from([(1u8, vec![1, 1])]), 8, 1).is_err());
        assert!(graph(&[(1, &[])]).closure([2]).is_err());
    }
    #[test]
    fn p4_oversized_cycle_is_never_split_into_permitted_subgroups() {
        let count = crate::libraries::reclaim::MAX_GROUP_OBJECTS + 1;
        let edges = (0..count)
            .map(|node| (node, vec![(node + 1) % count]))
            .collect();
        let graph = Graph::new(edges, count, count).unwrap();
        let groups = graph.groups(&BTreeSet::new());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), count);
        assert!(groups[0].len() > crate::libraries::reclaim::MAX_GROUP_OBJECTS);
    }
}
