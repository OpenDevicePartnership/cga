use rustc_data_structures::graph::StartNode;
use rustc_hir::{CoroutineDesugaring, CoroutineKind};
use rustc_log::tracing::*;
use rustc_middle::mir::Operand;
use rustc_middle::mir::{self, Body, CastKind, Location, Rvalue, Terminator, TerminatorKind, visit::Visitor as _};
use rustc_middle::mono::MonoItem;
use rustc_middle::ty::adjustment::PointerCoercion;
use rustc_middle::ty::{self, EarlyBinder, Instance, InstanceKind, TyCtxt, TypingEnv};
use rustc_span::{Span, span_bug};
use rustc_type_ir::CoroutineArgs;

use crate::dyn_object::dyn_object_call_to_impl_methods;
use crate::{Arrow, ArrowTarget, Callgraph};

/// Context used for the entire analysis.
struct AnalysisState<'tcx> {
    /// Type context.
    tcx: TyCtxt<'tcx>,

    /// The callgraph currently being constructed.
    callgraph: Callgraph<'tcx>,
}

/// A node in the MIR analysis.
///
/// The MIR is being visited using the Visitor pattern.
/// Roots and edges (and implicitly nodes) in the callgraph
/// are being registered during this analysis.
///
/// Edges are captured as [Arrow], and can be
/// * entrypoints
/// * explicit functions calls
/// * implicit possible function calls
/// * implicit panics like asserts and those found in async
///
/// For each call-like structure, the callee [Instance] and its [Body] are resolved to a Node,
/// and analysis proceeds recursively on the MIR of that [Body] using the [mir::visit::Visitor].
struct AnalysisNode<'a, 'tcx> {
    /// The [Instance] currently being analysed.
    instance: Instance<'tcx>,

    /// The [Body] belonging to that [Instance].
    body: &'tcx Body<'tcx>,

    /// A reference to the global [AnalysisState].
    state: &'a mut AnalysisState<'tcx>,
}

/// Extract the Callgraph from what should be considered 'root' items in the current crate.
///
/// These include:
/// * entrypoints like fn main() when this crate is a binary
/// * extern "C" fn's or those that are no_mangle
///
/// Walks over the MIR tree from these root nodes and collects any function calls and assertions.
///
/// Can only be run once 'collect_and_partition_mono_items' pass has run.
pub fn extract_from_roots<'tcx>(tcx: TyCtxt<'tcx>) -> Callgraph<'tcx> {
    let roots =
        crate::monomorphize::collect_roots(tcx, crate::monomorphize::MonoItemCollectionStrategy::NoConstOrStatic);
    extract(tcx, roots)
}

/// Extract the Callgraph with a series of [MonoItem]s as root nodes.
///
/// Walks over the MIR tree from these root nodes and collects any function calls and assertions.
///
/// Can only be run once 'collect_and_partition_mono_items' pass has run.
pub fn extract<'tcx>(tcx: TyCtxt<'tcx>, roots: Vec<MonoItem<'tcx>>) -> Callgraph<'tcx> {
    let mut state = AnalysisState {
        tcx,
        callgraph: Callgraph::new(),
    };
    for root in roots {
        let instance = match root {
            MonoItem::Static(def_id) => Instance::mono(tcx, def_id),
            MonoItem::Fn(instance) => instance,
            _ => continue,
        };
        state.callgraph.register_root(instance, state.tcx);
        state.analyse_instance(instance);
    }

    state.callgraph
}

impl<'a, 'tcx> mir::visit::Visitor<'tcx> for AnalysisNode<'a, 'tcx> {
    fn visit_terminator(&mut self, terminator: &Terminator<'tcx>, location: Location) {
        let source = terminator.source_info;
        let span = source.span;
        let tcx = self.state.tcx;

        // Assert that can result in a panic.
        if let TerminatorKind::Assert { .. } = terminator.kind {
            self.register_arrow(Arrow::for_location(ArrowTarget::BuiltinMirAssert, self.body, source));
        }

        // Call-like terminators.
        if let Some(instance) = match &terminator.kind {
            TerminatorKind::Call { func, .. } | TerminatorKind::TailCall { func, .. } => {
                self.resolve_fn_operand_opt(func, span)
            }
            TerminatorKind::Drop { place, .. } => {
                let ty = self.instance.instantiate_mir_and_normalize_erasing_regions(
                    tcx,
                    TypingEnv::codegen(tcx, self.instance.def_id()),
                    EarlyBinder::bind(tcx, place.ty(self.body, tcx)),
                );
                Some(Instance::resolve_drop_glue(tcx, ty.ty))
            }
            _ => None,
        } {
            self.register_arrow(Arrow::for_location(ArrowTarget::Called(instance), self.body, source));
        }

        self.super_terminator(terminator, location);
    }

    fn visit_rvalue(&mut self, rvalue: &Rvalue<'tcx>, location: Location) {
        // We are only interested in rvalue's when a fn is cast to a fnptr.
        // Reasoning what happens to this ptr value is out of scope and hence we assume
        // it will be used (derefenced & called) at some later point.
        if let Rvalue::Cast(CastKind::PointerCoercion(PointerCoercion::ReifyFnPointer(_), _), operand, _) = rvalue
            && let span = operand.span(self.body)
            && let Some(instance) = self.resolve_fn_operand_opt(operand, span)
        {
            let arrow = Arrow::for_location(
                ArrowTarget::FnPtrCast(instance),
                self.body,
                *self.body.source_info(location),
            );
            self.register_arrow(arrow);
        }
        self.super_rvalue(rvalue, location);
    }
}

impl<'a, 'tcx> AnalysisNode<'a, 'tcx> {
    /// On callgraph edge when we have an arrow from our [`Instance`] to something else.
    ///
    /// Do not call this for root nodes, as we should not have an [`Instance`] yet.
    ///
    /// Will recurse the analysis if the [`Arrow`] is a call-like operation.
    fn register_arrow(&mut self, arrow: Arrow<'tcx>) {
        self.state.callgraph.register_edge(self.instance, arrow);

        match arrow.target {
            // Dead ends with no further arrows.
            ArrowTarget::BuiltinMirAssert | ArrowTarget::AsyncPanic => (),
            // In any other case we should have fully monomorphized the function.
            ArrowTarget::Called(_) | ArrowTarget::VTableCalled(_) | ArrowTarget::FnPtrCast(_) => {
                let callee_instance = arrow.opt_instance().unwrap_or_else(|| {
                    span_bug!(
                        arrow.span,
                        "called expect_instance on an arrow that does not have an instance"
                    )
                });

                if let InstanceKind::Virtual(..) = callee_instance.def {
                    self.state.analyse_virtual(*callee_instance, arrow.span);
                } else {
                    self.state.analyse_instance(*callee_instance);
                }
            }
            ArrowTarget::Entrypoint(..) => {
                span_bug!(
                    arrow.span,
                    "Entrypoint is a root node of the graph, and cannot be logically generated mid-analysis"
                );
            }
        }
    }

    /// Assuming an [Operand] is a constant function item, resolve it into a monomorphized [Instance].
    ///
    /// This is relevant in a Call terminator, or when casting a fn to a ptr.
    fn resolve_fn_operand_opt(&self, operand: &Operand<'tcx>, span: Span) -> Option<Instance<'tcx>> {
        if let Some((callee, args)) = operand.const_fn_def() {
            let ty_env = TypingEnv::codegen(self.state.tcx, self.instance.def_id());
            Some(Instance::expect_resolve(
                self.state.tcx,
                ty_env,
                callee,
                self.instance.instantiate_mir_and_normalize_erasing_regions(
                    self.state.tcx,
                    ty_env,
                    EarlyBinder::bind(self.state.tcx, args),
                ),
                span,
            ))
        } else {
            None
        }
    }
}

impl<'tcx> AnalysisState<'tcx> {
    /// When a MIR Call (or call-like operation like casting as a fnptr) is performed, visit the [Instance].
    ///
    /// Visiting involves fetching the `body` associated with the instance,
    /// and checking if it requires special consideration as an async block.
    /// Finally a new [AnalysisNode] is created, and the [mir::visit::Visitor] is called using the preorder.
    fn analyse_instance(&mut self, mut instance: Instance<'tcx>) {
        if !self.callgraph.visit(instance) {
            return;
        }

        let owner = instance.def_id();

        if self.tcx.intrinsic(owner).is_some() {
            // When it is an intrinsic, build new raw instance for it.
            instance = ty::Instance::new_raw(owner, instance.args);
        }

        if !self.tcx.is_mir_available(owner) {
            // Ignore if MIR is not available for the current process.
            // Possibly other process for dependency crate will generate the callgraph for this call,
            // which will be merged later.
            return;
        }

        let body = self.tcx.instance_mir(instance.def);

        let mut this: AnalysisNode<'_, 'tcx> = AnalysisNode {
            instance,
            body,
            state: self,
        };

        if let Some(CoroutineKind::Desugared(CoroutineDesugaring::Async, _)) = body.coroutine_kind() {
            // TODO this implementation block is fragile.
            debug!("visiting (async coroutine) {instance:#?} {body:#?}");

            // Visit all BB's associated with Unresumed discriminators.
            // Do not visit "Returned" and "Panicked" discriminator bb's, instead emit a ArrowKind::AsyncPanic
            // BB0 is the switch statement

            let switch_bb = &body[body.basic_blocks.start_node()];
            let TerminatorKind::SwitchInt { targets, .. } = &switch_bb.terminator().kind else {
                span_bug!(
                    body.span,
                    "Expected async coroutine with a switch for the discriminator in the first basic block"
                );
            };

            for (value, bb) in targets.iter() {
                const RETURNED: u128 = <CoroutineArgs<TyCtxt<'_>> as ty::CoroutineArgsExt>::RETURNED as u128;
                const POISONED: u128 = <CoroutineArgs<TyCtxt<'_>> as ty::CoroutineArgsExt>::POISONED as u128;

                match value {
                    RETURNED => {
                        let data = &body[bb];
                        let source_info = data.terminator().source_info;

                        this.register_arrow(Arrow::for_location(ArrowTarget::AsyncPanic, body, source_info));
                    }
                    POISONED => {} // Ignore
                    _ => {
                        for (bb, data) in mir::traversal::Preorder::new(body, bb) {
                            this.visit_basic_block_data(bb, data);
                        }
                    }
                }
            }

            // TODO coroutine drop?
        } else {
            // TODO instead of directly traversing the MIR, try to const_eval any calls to eliminate unreachable SwitchInt branches.
            debug!("visiting (normal preorder) {instance:#?}");
            for (bb, data) in mir::traversal::preorder(body) {
                this.visit_basic_block_data(bb, data);
            }
        }
    }

    /// When a MIR Call (or call-like operation like casting as a fnptr) to a virtual fn is performed,
    /// visit all the [Instance]s that possibly implement that trait fn.
    fn analyse_virtual(&mut self, instance: Instance<'tcx>, span: Span) {
        let InstanceKind::Virtual(virtual_def_id, _) = instance.def else {
            span_bug!(span, "Visiting a non-virtual {instance:?}");
        };

        if !self.callgraph.visit(instance) {
            return;
        }

        for method in dyn_object_call_to_impl_methods(virtual_def_id, instance.args, self.tcx) {
            // Add a 'fake' use to each of the VTable entries.
            self.callgraph.register_edge(
                instance,
                Arrow {
                    target: ArrowTarget::VTableCalled(method),
                    hir_id: None,
                    span,
                },
            );
            self.analyse_instance(method);
        }
    }
}
