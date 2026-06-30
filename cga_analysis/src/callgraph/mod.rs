use indexmap::{
    IndexSet,
    map::{Entry, IndexMap},
};
use rustc_middle::ty::{Instance, TyCtxt};

use crate::{Use, UseKind};

mod data;

pub mod bigraph;

/// The callgraph as used in the analysis step.
///
/// Each [`Use`] is annotated with the [`NodeId`] to give a deterministic ordering to the callgraph.
#[derive(Clone, Debug)]
pub struct Callgraph<'tcx> {
    roots: IndexSet<Use<'tcx>>,
    edges: IndexMap<Instance<'tcx>, IndexSet<Use<'tcx>>>,
}

impl<'tcx> Callgraph<'tcx> {
    pub fn new() -> Self {
        Self {
            roots: IndexSet::default(),
            edges: IndexMap::default(),
        }
    }

    pub fn register_root(&mut self, instance: Instance<'tcx>, tcx: TyCtxt<'tcx>) {
        let root_node = instance.def_id().expect_local();
        let hir_id = tcx.local_def_id_to_hir_id(root_node);
        let span = tcx.hir_span(hir_id);

        let use_ = Use {
            kind: UseKind::Entrypoint(instance),
            span,
            hir_id: Some(hir_id),
        };

        self.roots.insert(use_);
    }

    pub fn register_edge(&mut self, from: Instance<'tcx>, to: Use<'tcx>) {
        self.edges.entry(from).or_default().insert(to);
    }

    /// Insert a node into the callgraph if it did not exist already.
    ///
    /// Does not touch the edges if the node did exist previously.
    ///
    /// Returns `true` if the node was not yet visited (and we just inserted it).
    pub fn visit(&mut self, instance: Instance<'tcx>) -> bool {
        let entry = self.edges.entry(instance);
        let was_vacant = matches!(entry, Entry::<_, _>::Vacant(..));
        entry.or_default();
        was_vacant
    }

    /// Quickly iterates over all edges.
    pub fn edges(&self) -> impl Iterator<Item = (Option<&Instance<'tcx>>, &Use<'tcx>)> {
        self.roots.iter().map(|use_| (None, use_)).chain(
            self.edges
                .iter()
                .flat_map(|(from, tos)| tos.iter().map(move |to| (Some(from), to))),
        )
    }

    pub fn cut_edges(&mut self, f: impl Fn(&Use<'tcx>) -> bool) {
        for (_, tos) in self.edges.iter_mut() {
            tos.retain(|use_| !f(use_));
        }
    }
}
