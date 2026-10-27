//! Bidirectional Breadth First Search implementation.
//!
//! Copied from the [pathfinding](https://crates.io/crates/pathfinding) crate and
//! adapted for the BiCallgraph for less boilerplate & better performance.
//!
//! Pathfinding copyright Samuel Tardieu <sam@rfc1149.net> and released under Apache-2.0/MIT.

use crate::callgraph::bigraph::{BiCallgraph, Edge, Node};

use indexmap::IndexMap;

impl<'tcx> BiCallgraph<'tcx> {
    /// Bidirectional Breadth First Search
    ///
    /// Copied from the `pathfinding` crate and adapted to this graph structure.
    pub(super) fn bfs_bidirectional(
        &self,
        starts: impl Iterator<Item = Node<'tcx>>,
        ends: impl Iterator<Item = Node<'tcx>>,
    ) -> Option<Vec<Edge<'tcx>>> {
        let mut predecessors: IndexMap<Node<'tcx>, Option<usize>> = IndexMap::default();
        predecessors.extend(starts.map(|n| (n, None)));
        let mut successors: IndexMap<Node<'tcx>, Option<usize>> = IndexMap::default();
        successors.extend(ends.map(|n| (n, None)));

        if predecessors.keys().any(|node| successors.contains_key(node)) {
            return Some(vec![]); // We are already here.
        }

        let mut i_forwards = 0;
        let mut i_backwards = 0;
        let middle = 'l: loop {
            for _ in 0..(predecessors.len() - i_forwards) {
                let node = *predecessors.get_index(i_forwards).unwrap().0;
                for successor_edge in self.successors(node) {
                    let successor_node = successor_edge.to;
                    if !predecessors.contains_key(&successor_node) {
                        predecessors.insert(successor_node, Some(i_forwards));
                    }
                    if successors.contains_key(&successor_node) {
                        break 'l Some(successor_node);
                    }
                }
                i_forwards += 1;
            }

            for _ in 0..(successors.len() - i_backwards) {
                let node = *successors.get_index(i_backwards).unwrap().0;
                for predecessor_edge in self.predecessors(node) {
                    let predecessor_node = predecessor_edge.from;
                    if !successors.contains_key(&predecessor_node) {
                        successors.insert(predecessor_node, Some(i_backwards));
                    }
                    if predecessors.contains_key(&predecessor_node) {
                        break 'l Some(predecessor_node);
                    }
                }
                i_backwards += 1;
            }

            if i_forwards == predecessors.len() && i_backwards == successors.len() {
                break 'l None;
            }
        };

        middle.map(|middle| {
            // Path found!
            // Build the path.
            let mut path: Vec<Edge<'tcx>> = vec![];
            // From middle to the start.
            let mut node = Some(middle.clone());
            while let Some(n) = node {
                node = predecessors[&n].map(|i| {
                    let prev = *predecessors.get_index(i).unwrap().0;
                    path.push(*self.predecessors(n).find(|e| e.from == prev).unwrap());
                    prev
                });
            }
            // Reverse, to put start at the front.
            path.reverse();
            // And from middle to the end.
            let mut node = successors[&middle].map(|i| {
                let next = successors.get_index(i).unwrap().0.clone();
                path.push(*self.successors(middle).find(|e| e.to == next).unwrap());
                next
            });
            while let Some(n) = node {
                node = successors[&n].map(|i| {
                    let next = *successors.get_index(i).unwrap().0;
                    path.push(*self.successors(n).find(|e| e.to == next).unwrap());
                    next
                });
            }

            path
        })
    }
}
