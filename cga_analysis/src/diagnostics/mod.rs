use cga_data::cmd::LintConfig;
use indexmap::{IndexMap, IndexSet};
use itertools::Itertools;
use rustc_attr_ir::lang_items::LangItem;
use rustc_errors::{Diag, MultiSpan};
use rustc_lint::Lint;
use rustc_log::tracing::*;
use rustc_middle::ty::TyCtxt;
use rustc_span::{Span, SyntaxContext};

mod rules;

use crate::{
    BLACKLISTED, Callgraph, PANICS, Use, UseKind,
    callgraph::bigraph::{BiCallgraph, Edge, Node},
    canonical_path,
    diagnostics::rules::LintRuleSet,
    panics::PanicStatus,
};

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
struct BoundaryLeaf<'a, 'tcx> {
    boundary: &'a Use<'tcx>,
    leaf: &'a Use<'tcx>,
}

pub fn emit_lints<'tcx>(mut callgraph: Callgraph<'tcx>, config: &LintConfig, tcx: TyCtxt<'tcx>) {
    // TODO better error handling
    let whitelist = LintRuleSet::new(&config.whitelist).unwrap();
    let blacklist = LintRuleSet::new(&config.blacklist).unwrap();

    // Cut away all edges in the callgraph that are whitelisted.
    callgraph.cut_edges(|use_| {
        (config.options.allow_async_panic && matches!(use_.kind, UseKind::AsyncPanic))
            || (config.options.allow_assert && matches!(use_.kind, UseKind::BuiltinMirAssert))
            || whitelist.use_applies(use_, tcx)
    });

    let bigraph = BiCallgraph::from(&callgraph);

    // Compute all boundaries and leafs, and group them per span so that we have one lint per location.
    let mut panic_boundaries: IndexMap<Span, IndexSet<BoundaryLeaf<'_, 'tcx>>> = IndexMap::default();
    let mut blacklist_boundaries: IndexMap<Span, IndexSet<BoundaryLeaf<'_, 'tcx>>> = IndexMap::default();

    for Edge { use_: leaf, .. } in bigraph.edges() {
        if leaf.panic_status(tcx).any() {
            info!("Leaf {leaf:?}");
            bigraph.find_boundaries(leaf, tcx, |boundary| {
                let root_span = rustc_span::hygiene::walk_chain(boundary.span, SyntaxContext::root());
                panic_boundaries
                    .entry(root_span)
                    .or_default()
                    .insert(BoundaryLeaf { boundary, leaf });
            });
        }

        if blacklist.use_applies(leaf, tcx) {
            bigraph.find_boundaries(leaf, tcx, |boundary| {
                let root_span = rustc_span::hygiene::walk_chain(boundary.span, SyntaxContext::root());
                blacklist_boundaries
                    .entry(root_span)
                    .or_default()
                    .insert(BoundaryLeaf { boundary, leaf });
            });
        }
    }

    process_boundaries::<Panics>(&bigraph, panic_boundaries, tcx);
    process_boundaries::<Blacklisted>(&bigraph, blacklist_boundaries, tcx);
}

trait LintEngine {
    const LINT: &'static Lint;
    const INDIRECT_ISSUE: &'static str;
    const DIRECT_ISSUE: &'static str;
    const PLURAL_NOUN: &'static str;

    fn select<'a, 'tcx: 'a>(
        boundary_leafs: IndexSet<BoundaryLeaf<'a, 'tcx>>,
        _tcx: TyCtxt<'tcx>,
    ) -> IndexSet<BoundaryLeaf<'a, 'tcx>> {
        boundary_leafs // Do nothing but pass it.
    }

    fn is_direct(data: &LintData<'_, '_>, tcx: TyCtxt<'_>) -> bool;
}

struct Panics;
struct Blacklisted;

impl LintEngine for Panics {
    const LINT: &'static Lint = PANICS;
    const INDIRECT_ISSUE: &'static str = "can panic";
    const DIRECT_ISSUE: &'static str = "is a panic";
    const PLURAL_NOUN: &'static str = "panics";

    fn select<'a, 'tcx: 'a>(
        mut boundary_leafs: IndexSet<BoundaryLeaf<'a, 'tcx>>,
        tcx: TyCtxt<'tcx>,
    ) -> IndexSet<BoundaryLeaf<'a, 'tcx>> {
        let direct_panics: IndexSet<_> = boundary_leafs
            .iter()
            .filter(|bl| matches!(bl.leaf.panic_status(tcx), PanicStatus::PanicImpl))
            .copied()
            .collect();

        // If there are any direct panics, they take precedence over any other panics.
        if !direct_panics.is_empty() {
            boundary_leafs = direct_panics;
        }

        boundary_leafs
    }

    fn is_direct(data: &LintData<'_, '_>, tcx: TyCtxt<'_>) -> bool {
        // Note(unwrap): trace will have at least one element.
        &data.boundary == data.trace.last().unwrap()
            && matches!(data.boundary.panic_status(tcx), PanicStatus::PanicImpl)
    }
}

impl LintEngine for Blacklisted {
    const LINT: &'static Lint = BLACKLISTED;
    const INDIRECT_ISSUE: &'static str = "can call blacklisted functions";
    const DIRECT_ISSUE: &'static str = "is blacklisted";
    const PLURAL_NOUN: &'static str = "blacklisted functions";

    fn is_direct(data: &LintData<'_, '_>, _tcx: TyCtxt<'_>) -> bool {
        // Note(unwrap): trace will have at least one element.
        &data.boundary == data.trace.last().unwrap()
    }
}

fn process_boundaries<'tcx, Engine: LintEngine>(
    bigraph: &BiCallgraph<'tcx>,
    boundaries: IndexMap<Span, IndexSet<BoundaryLeaf<'_, 'tcx>>>,
    tcx: TyCtxt<'tcx>,
) {
    for (span, boundary_leafs) in boundaries {
        info!("Boundary {span:?} {boundary_leafs:?}");

        let all_leafs: IndexSet<_> = boundary_leafs.iter().map(|bl| Node::from(bl.leaf)).collect();

        let boundary_leafs = Engine::select(boundary_leafs, tcx);
        let boundaries: IndexSet<_> = boundary_leafs.iter().map(|bl| *bl.boundary).collect();

        // Path of trace root to boundary.
        let Some(path_from_root) = bigraph.find_shortest_from_roots(&boundaries) else {
            info!("No path to root?!");
            continue;
        };

        // Note(unwrap): `find_shortest_from_roots` generates traces with at least 1 edge.
        let boundary = path_from_root.last().unwrap().use_;

        // Note(unwrap): we got here, hence there should be a path.
        let path_to_panics = bigraph
            .find_shortest_to_leafs(
                std::iter::once(Node::from(&boundary)),
                boundary_leafs.iter().map(|bl| Node::from(bl.leaf)),
            )
            .unwrap();

        if let (Some(root_last), Some(panic_first)) = (path_from_root.last(), path_to_panics.first()) {
            assert_eq!(root_last.to, panic_first.from);
            info!("Path consecutive");
        } else {
            info!("Path not consecutive");
        }

        let trace: Vec<_> = path_from_root.iter().chain(path_to_panics.iter()).collect();

        {
            // Double-check if the result is a consistent trace.
            let mut iter = trace.iter();
            let mut prev = iter.next().unwrap().to;
            for e in iter {
                assert_eq!(prev, e.from);
                prev = e.to;
            }
        }

        let trace = trace.iter().map(|e| e.use_).collect();

        let data = LintData {
            boundary,
            above_paths: bigraph.count_paths_from_roots(Node::from(&boundary)),
            below_paths: bigraph.count_paths_to_leafs(Node::from(&boundary), &all_leafs),
            below_leafs: all_leafs.len(),
            trace: &trace,
        };

        lint_use::<Engine>(data, tcx);
    }
}

#[derive(Debug)]
struct LintData<'a, 'tcx> {
    boundary: Use<'tcx>,

    /// Number of call paths to end up at the boundary.
    above_paths: usize,

    /// Number of call paths from boundary to panic locations.
    below_paths: usize,

    /// Number of reachable panic locations.
    below_leafs: usize,

    /// The trace picked as the example to illustrate the panic path.
    trace: &'a Vec<Use<'tcx>>,
}

impl<'tcx> Use<'tcx> {
    fn descriptor(&self) -> &'static str {
        match self.kind {
            UseKind::Entrypoint(..) => "entrypoint",
            UseKind::Called(..) => "called",
            UseKind::VTableCalled(..) => "possible vtable resolution",
            UseKind::FnPtrCast(..) => "fn-ptr cast",
            UseKind::BuiltinMirAssert => "assert",
            UseKind::AsyncPanic => "implicit async panic",
        }
    }

    fn noun(&self) -> &'static str {
        match self.kind {
            UseKind::Entrypoint(..) => "start to",
            UseKind::Called(..) => "call to",
            UseKind::VTableCalled(..) => "possible dyn object call to",
            UseKind::FnPtrCast(..) => "possible later call to",
            UseKind::BuiltinMirAssert => "implicit assert",
            UseKind::AsyncPanic => "can be poll'd after Ready",
        }
    }
}

fn decorate_expansion(use_: &Use<'_>, multi_span: &mut MultiSpan) {
    if use_.span.from_expansion() {
        let root_span = rustc_span::hygiene::walk_chain(use_.span, SyntaxContext::root());
        if use_.span != root_span {
            multi_span.push_span_label(root_span, "expanded from here");
        }
    }
}

/// Decorate with extra notes if the leaf node is a panic associated with an async coroutine
fn decorate_leaf(use_: &Use<'_>, diag: &mut Diag<'_>) {
    match use_.kind {
        UseKind::AsyncPanic => {
            diag.note("an async block can always panic when poll'd after Ready");
        }
        _ => {}
    }
}

/// Decorate with extra notes if the boundary is not a straightforward function call
fn decorate_cast(use_: &Use<'_>, diag: &mut Diag<'_>) {
    if matches!(use_.kind, UseKind::FnPtrCast(..)) {
        diag.note("once a function is cast to a function pointer, cga can no longer tell how it is used");
        diag.note("as a precaution, it must assume you will eventually call the function");
    }
}

fn lint_use<Engine: LintEngine>(data: LintData<'_, '_>, tcx: TyCtxt<'_>) {
    let boundary_use = data.boundary;
    let drop_glue_fn = tcx.require_lang_item(LangItem::DropGlue, boundary_use.span);

    // Note(unwrap): hir_id is Some because TraceBoundary guarantees that it is local.
    let lint_node = boundary_use
        .hir_id
        .expect("Local boundary should have an associated HirId");

    info!("linting {boundary_use:?}");

    let mut multi_span = MultiSpan::from_span(boundary_use.span);

    decorate_expansion(&boundary_use, &mut multi_span);

    tcx.emit_node_span_lint(
        Engine::LINT,
        lint_node,
        multi_span,
        rustc_errors::DiagDecorator(|diag| {
            let issue = if Engine::is_direct(&data, tcx) {
                Engine::DIRECT_ISSUE
            } else {
                Engine::INDIRECT_ISSUE
            };

            if let Some(callee) = boundary_use.opt_def_id() {
                if callee == drop_glue_fn {
                    // Note(unwrap): we are in drop glue, hence there must be an instance.
                    let boundary_instance = boundary_use.opt_instance().unwrap();

                    diag.primary_message(format!(
                        "{} drop of `{}` which {}",
                        boundary_use.noun(),
                        boundary_instance.args.type_at(0),
                        issue
                    ));
                } else {
                    diag.primary_message(format!(
                        "{} {} `{}` which {}",
                        boundary_use.noun(),
                        tcx.def_descr(callee),
                        tcx.def_path_str(callee),
                        issue
                    ));
                }
            } else {
                diag.primary_message(format!("{} which {}", boundary_use.noun(), issue));
            }

            if data.above_paths > 1 {
                diag.note(format!(
                    "this location is called in {} different traces",
                    data.above_paths
                ));
            }

            if data.below_leafs > 1 {
                diag.note(format!(
                    "{} different {} from this location",
                    data.below_leafs,
                    Engine::PLURAL_NOUN,
                ));
            }

            if let Some(leaf) = data.trace.last() {
                decorate_leaf(&leaf, diag);
            }

            decorate_cast(&boundary_use, diag);

            {
                let backtrace = data
                    .trace
                    .iter()
                    .rev()
                    .enumerate()
                    .map(|(i, use_)| {
                        let use_str = match use_.kind {
                            UseKind::Entrypoint(instance)
                            | UseKind::Called(instance)
                            | UseKind::VTableCalled(instance)
                            | UseKind::FnPtrCast(instance) => {
                                let instance_path = if instance.def_id() == drop_glue_fn {
                                    format!("drop of `{}`", instance.args.type_at(0))
                                } else {
                                    canonical_path(&instance)
                                };

                                format!("{instance_path} ({})", use_.descriptor())
                            }
                            UseKind::BuiltinMirAssert | UseKind::AsyncPanic => use_.descriptor().to_string(),
                        };
                        let is_boundary = use_ == &boundary_use;

                        format!("{i}: {use_str}{}", if is_boundary { " <--" } else { "" })
                    })
                    .join("\n");

                let number_of_paths = data.above_paths * data.below_paths;
                if number_of_paths == 1 {
                    diag.note(format!("backtrace:\n{backtrace}"));
                } else {
                    diag.note(format!("backtrace (1 of {number_of_paths}):\n{backtrace}"));
                }
            }
        }),
    );
}
