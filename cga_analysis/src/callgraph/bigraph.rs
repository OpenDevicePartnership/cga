use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};
use rustc_middle::ty::Instance;

use crate::{Arrow, callgraph::Callgraph};

mod bfs;

#[derive(Clone, Debug)]
pub struct BiCallgraph<'tcx> {
    edges: IndexMap<Node<'tcx>, IndexSet<Edge<'tcx>>>,
    rev_edges: IndexMap<Node<'tcx>, IndexSet<Edge<'tcx>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Node<'tcx> {
    Entrypoint,
    Assert,
    AsyncPanic,
    Item(Instance<'tcx>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Edge<'tcx> {
    pub from: Node<'tcx>,
    pub to: Node<'tcx>,
    pub arrow: Arrow<'tcx>,
}

impl<'tcx> From<&Arrow<'tcx>> for Node<'tcx> {
    fn from(value: &Arrow<'tcx>) -> Self {
        match value.target {
            crate::ArrowTarget::Entrypoint(instance)
            | crate::ArrowTarget::Called(instance)
            | crate::ArrowTarget::VTableCalled(instance)
            | crate::ArrowTarget::FnPtrCast(instance) => Node::Item(instance),
            crate::ArrowTarget::BuiltinMirAssert => Node::Assert,
            crate::ArrowTarget::AsyncPanic => Node::AsyncPanic,
        }
    }
}

impl<'tcx> From<&Callgraph<'tcx>> for BiCallgraph<'tcx> {
    fn from(value: &Callgraph<'tcx>) -> Self {
        let mut edges: IndexMap<Node<'tcx>, IndexSet<Edge<'tcx>>> = IndexMap::default();
        let mut rev_edges: IndexMap<Node<'tcx>, IndexSet<Edge<'tcx>>> = IndexMap::default();

        for (from, tos) in value.edges.iter() {
            let from = Node::Item(*from);
            let from_edges = edges.entry(from).or_default();

            for arrow in tos.iter() {
                let to = Node::from(arrow);
                let edge = Edge {
                    from,
                    to,
                    arrow: *arrow,
                };

                from_edges.insert(edge);
                rev_edges.entry(to).or_default().insert(edge);
            }
        }

        let entrypoint = Node::Entrypoint;
        let entrypoints = edges.entry(entrypoint).or_default();
        for arrow in value.roots.iter() {
            let to = Node::from(arrow);
            let edge = Edge {
                from: entrypoint,
                to,
                arrow: *arrow,
            };
            entrypoints.insert(edge);
            rev_edges.entry(to).or_default().insert(edge);
        }

        Self { edges, rev_edges }
    }
}

impl<'tcx> BiCallgraph<'tcx> {
    pub fn edges(&self) -> impl Iterator<Item = &Edge<'tcx>> {
        self.edges.iter().flat_map(|(_, edges)| edges.iter())
    }

    /// Find all the crate boundaries from a leaf node.
    pub fn find_boundaries<'a>(&'a self, leaf: &'a Arrow<'tcx>, mut f: impl FnMut(&'a Arrow<'tcx>)) {
        fn visit_node<'a, 'tcx>(
            this: &'a BiCallgraph<'tcx>,
            to: Node<'tcx>,
            visited: &mut IndexSet<Node<'tcx>>,
            f: &mut impl FnMut(&'a Arrow<'tcx>),
        ) {
            if !visited.insert(to) {
                return;
            }

            if let Some(edges) = this.rev_edges.get(&to) {
                for edge in edges {
                    if edge.arrow.hir_id.is_some() {
                        f(&edge.arrow);
                        continue;
                    }

                    visit_node(this, edge.from, visited, f);
                }
            }
        }

        if leaf.hir_id.is_some() {
            f(leaf);
            return;
        }

        let callee = Node::from(leaf);
        let mut visited = IndexSet::default();

        if let Some(edges) = self.rev_edges.get(&callee) {
            for edge in edges {
                if edge.arrow != *leaf {
                    continue;
                }

                visit_node(self, edge.from, &mut visited, &mut f);
            }
        }
    }

    pub fn find_shortest_from_roots(&self, boundaries: &IndexSet<Arrow<'tcx>>) -> Option<Vec<Edge<'tcx>>> {
        for e in self.edges() {
            if boundaries.contains(&e.arrow)
                && let Some(mut trace) =
                    self.bfs_bidirectional(std::iter::once(Node::Entrypoint), std::iter::once(e.from))
            {
                trace.push(*e);
                return Some(trace);
            }
        }
        None
    }

    pub fn find_shortest_to_leafs(
        &self,
        boundaries: impl Iterator<Item = Node<'tcx>>,
        leafs: impl Iterator<Item = Node<'tcx>>,
    ) -> Option<Vec<Edge<'tcx>>> {
        self.bfs_bidirectional(boundaries, leafs)
    }

    pub fn count_paths_from_roots(&self, dest: Node<'tcx>) -> usize {
        cached_count_paths(
            dest,
            &std::iter::once(Node::Entrypoint).collect(),
            &|node| self.predecessors(node).map(|edge: &Edge<'tcx>| edge.from),
            &mut IndexMap::default(),
            &Trace::new(),
        )
    }

    pub fn count_paths_to_leafs(&self, boundary: Node<'tcx>, leafs: &IndexSet<Node<'tcx>>) -> usize {
        cached_count_paths(
            boundary,
            leafs,
            &|node| self.successors(node).map(|edge| edge.to),
            &mut IndexMap::default(),
            &Trace::new(),
        )
    }

    fn successors(&self, node: Node<'tcx>) -> impl Iterator<Item = &Edge<'tcx>> {
        self.edges.get(&node).into_iter().flat_map(|edges| edges.iter())
    }

    fn predecessors(&self, node: Node<'tcx>) -> impl Iterator<Item = &Edge<'tcx>> {
        self.rev_edges.get(&node).into_iter().flat_map(|edges| edges.iter())
    }
}

fn cached_count_paths<'tcx, NI>(
    start: Node<'tcx>,
    dest: &IndexSet<Node<'tcx>>,
    successors: &impl Fn(Node<'tcx>) -> NI,
    cache: &mut IndexMap<Node<'tcx>, usize>,
    trace: &Trace<'tcx>,
) -> usize
where
    NI: Iterator<Item = Node<'tcx>>,
{
    if let Some(&n) = cache.get(&start) {
        return n;
    }

    if dest.contains(&start) {
        return 1;
    }

    // Prevent paths with cycles from expanding a node twice.
    if trace.contains(start) {
        return 0;
    }

    let trace = trace.extend(start);
    let count = successors(start)
        .map(|node| cached_count_paths(node, dest, successors, cache, &trace))
        .sum();

    if let Some(old) = cache.insert(start, count) {
        assert_eq!(old, count);
    }

    count
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct TraceNode<'tcx> {
    parent: Option<Rc<TraceNode<'tcx>>>,
    current: Node<'tcx>,
}

#[derive(Clone, Hash, PartialEq, Eq)]
pub struct Trace<'tcx> {
    inner: Option<Rc<TraceNode<'tcx>>>,
}

pub struct TraceIterator<'tcx> {
    inner: Option<Rc<TraceNode<'tcx>>>,
}

impl<'tcx> Trace<'tcx> {
    pub fn new() -> Self {
        Self { inner: None }
    }

    pub fn extend(&self, current: Node<'tcx>) -> Self {
        Self {
            inner: Some(Rc::new(TraceNode {
                parent: self.inner.clone(),
                current,
            })),
        }
    }

    pub fn contains(&self, needle: Node<'tcx>) -> bool {
        self.iter().find(|node| node == &needle).is_some()
    }

    pub fn iter(&self) -> TraceIterator<'tcx> {
        // Note(clone): it is relatively cheap, this is a linked list.
        self.clone().into_iter()
    }
}

impl<'tcx> IntoIterator for Trace<'tcx> {
    type Item = Node<'tcx>;
    type IntoIter = TraceIterator<'tcx>;

    fn into_iter(self) -> Self::IntoIter {
        TraceIterator { inner: self.inner }
    }
}

impl<'tcx> Iterator for TraceIterator<'tcx> {
    type Item = Node<'tcx>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(inner) = self.inner.take() {
            self.inner = inner.parent.clone();
            Some(inner.current)
        } else {
            None
        }
    }
}
