use std::collections::{BTreeMap, BTreeSet};

use cga_data::callgraph as data;
use rustc_data_structures::fx::FxHashMap;
use rustc_middle::ty::{Instance, TyCtxt};
use rustc_span::Span;

use crate::{Use, callgraph::Callgraph, canonical_path};

struct Assembler<'tcx> {
    /// Final output of the assembler.
    output: data::Callgraph,

    /// Mapping fom Instance to a NodeId.
    instance_map: FxHashMap<Instance<'tcx>, data::NodeId>,

    /// Rustc context.
    tcx: TyCtxt<'tcx>,
}

impl<'tcx> Assembler<'tcx> {
    fn new(tcx: TyCtxt<'tcx>) -> Self {
        Self {
            output: data::Callgraph {
                roots: BTreeSet::default(),
                nodes: Vec::new(),
                edges: BTreeMap::default(),
            },
            instance_map: FxHashMap::default(),
            tcx,
        }
    }

    fn span_location(&self, span: Span) -> data::Location {
        // TODO figure out if we want to do macro expansions.

        let (file, sl, sc, el, ec) = self.tcx.sess.source_map().span_to_location_info(span);

        let path = file
            .map(|file| {
                file.name
                    .clone()
                    .into_local_path()
                    .map(|path| path.to_string_lossy().to_string())
            })
            .flatten()
            .unwrap_or_else(|| "{unknown}".to_string());

        let span = data::Span(
            data::FilePosition {
                line: sl as u32,
                column: sc as u32,
            }..=data::FilePosition {
                line: el as u32,
                column: ec as u32,
            },
        );

        data::Location { path, span }
    }

    fn notify_node(&mut self, instance: Instance<'tcx>, root: bool) -> data::NodeId {
        // TODO move span_location into lamda (but self is borrowed, which makes it complicated).
        let location = self.span_location(self.tcx.def_span(instance.def_id()));

        *self.instance_map.entry(instance).or_insert_with(|| {
            let node = data::Node {
                name: canonical_path(&instance),
                location,
            };

            let node_id = (self.output.nodes.len() as u32).into();
            self.output.nodes.push(node);

            if root {
                self.output.roots.insert(node_id);
            }

            node_id
        })
    }

    fn notify_edge(&mut self, from: data::NodeId, use_: Use<'tcx>) {
        let location = self.span_location(use_.span);
        let kind = match use_.kind {
            crate::UseKind::Entrypoint(..) => panic!("Entrypoints should not be an edge."),
            crate::UseKind::Called(instance) => {
                data::EdgeKind::Called(self.notify_node(instance, false))
            }
            crate::UseKind::VTableCalled(instance) => {
                data::EdgeKind::VTableCalled(self.notify_node(instance, false))
            }
            crate::UseKind::FnPtrCast(instance) => {
                data::EdgeKind::FnPtrCast(self.notify_node(instance, false))
            }
            crate::UseKind::BuiltinMirAssert => data::EdgeKind::BuiltinAssert,
            crate::UseKind::AsyncPanic => data::EdgeKind::AsyncPanic,
        };

        self.output
            .edges
            .entry(from)
            .or_default()
            .push(data::Edge { kind, location });
    }
}

impl<'tcx> Callgraph<'tcx> {
    pub fn export(&self, tcx: TyCtxt<'tcx>) -> data::Callgraph {
        let mut edges: Vec<(Option<Instance<'tcx>>, Use<'tcx>)> = vec![];
        edges.reserve_exact(self.roots.len() + self.edges.len());

        for root in self.roots.iter() {
            edges.push((None, root.clone()));
        }

        for (from, tos) in self.edges.iter() {
            for to in tos.iter() {
                edges.push((Some(*from), to.clone()));
            }
        }

        let mut assembler = Assembler::new(tcx);
        for (from, to) in edges {
            if let Some(from) = from {
                let node_id = assembler.notify_node(from, false);
                assembler.notify_edge(node_id, to);
            } else {
                // From is empty, meaning it must be a root node.
                let instance = to
                    .opt_instance()
                    .expect("Root use_ nodes need to be Entrypoints");

                assembler.notify_node(*instance, true);
            }
        }

        assembler.output
    }
}
