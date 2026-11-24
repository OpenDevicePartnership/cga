use indexmap::{
    IndexSet,
    map::{Entry, IndexMap},
};
use rustc_middle::ty::{Instance, TyCtxt};

use crate::{Arrow, ArrowTarget};

mod data;

pub mod bigraph;

/// The callgraph as used in the analysis step.
#[derive(Clone, Debug)]
pub struct Callgraph<'tcx> {
    roots: IndexSet<Arrow<'tcx>>,
    edges: IndexMap<Instance<'tcx>, IndexSet<Arrow<'tcx>>>,
}

impl<'tcx> Callgraph<'tcx> {
    pub fn new() -> Self {
        Self {
            roots: IndexSet::default(),
            edges: IndexMap::default(),
        }
    }

    pub fn register_root(&mut self, instance: Instance<'tcx>, tcx: TyCtxt<'tcx>) {
        // Only collect local root nodes.
        // This excludes tcx.lang_items().start_fn(), which is never local.
        if let Some(root_node) = instance.def_id().as_local() {
            let hir_id = tcx.local_def_id_to_hir_id(root_node);
            let span = tcx.hir_span(hir_id);

            let arrow = Arrow {
                target: ArrowTarget::Entrypoint(instance),
                span,
                hir_id: Some(hir_id),
            };

            self.roots.insert(arrow);
        }
    }

    pub fn register_edge(&mut self, from: Instance<'tcx>, to: Arrow<'tcx>) {
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
    pub fn edges(&self) -> impl Iterator<Item = (Option<&Instance<'tcx>>, &Arrow<'tcx>)> {
        self.roots.iter().map(|arrow| (None, arrow)).chain(
            self.edges
                .iter()
                .flat_map(|(from, tos)| tos.iter().map(move |to| (Some(from), to))),
        )
    }

    pub fn cut_edges(&mut self, f: impl Fn(&Arrow<'tcx>) -> bool) {
        for (_, tos) in self.edges.iter_mut() {
            tos.retain(|arrow| !f(arrow));
        }
    }
}
