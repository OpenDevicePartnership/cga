//! Run a post-mono pass on MIR, possibly from other crates.
//! In post-mono MIR, all functions are possible to resolve to an [`Instance`].
//!
//! This traverses the callgraph along function call edges, starting from mono roots.
//!
//! A "mono root" is an externally reachable function, such as `main`, `pub fn`, something with `no_mangle` or `export_name`,
//! or weird things such as `std::rt::lang_start`.
//!
//! There are three functions in play during this pass.
//! 1. The potentially panicking function being called, which we call the 'callee'.
//! 2. The generic function being instantiated, *which may not be in the current crate*, called the
//!    'caller'.
//! 3. The functions that instantiated it (recursively, back to the mono root).
//!
//! ## Recommended reading
//! - [MIR Debugging](https://rustc-dev-guide.rust-lang.org/mir/debugging.html)

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
use crate::{AnalysisState, Callgraph, Use, UseKind};

/// A node in the post-monomorphisation analysis.
struct PostMonoNode<'a, 'tcx> {
    /// The function we are currently traversing.
    instance: Instance<'tcx>,

    /// Its body.
    body: &'tcx Body<'tcx>,

    // The global analysis context.
    state: &'a mut AnalysisState<'tcx>,
}

/// Analyse the MIR items recursively, starting from root nodes collected by `rustc_monomorphize`.
///
/// During the analysis the results are added to a common [Callgraph] struct and finally returned.
pub fn generate_callgraph<'tcx>(tcx: TyCtxt<'tcx>, roots: Vec<MonoItem<'tcx>>) -> Callgraph<'tcx> {
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
        PostMonoNode::visit_instance(&mut state, instance);
    }

    state.callgraph
}

impl<'a, 'tcx> mir::visit::Visitor<'tcx> for PostMonoNode<'a, 'tcx> {
    fn visit_terminator(&mut self, terminator: &Terminator<'tcx>, location: Location) {
        for use_ in self.get_call_def_mir(terminator) {
            self.on_edge(use_);
        }

        if let TerminatorKind::Assert { .. } = terminator.kind {
            let use_ = Use::for_location(UseKind::BuiltinMirAssert, self.body, terminator.source_info);
            self.on_edge(use_);
        }

        self.super_terminator(terminator, location);
    }

    fn visit_rvalue(&mut self, rvalue: &Rvalue<'tcx>, location: Location) {
        let Some((_call_span, use_kind)) = self.find_dynamic_cast(rvalue) else {
            return;
        };

        let use_ = Use::for_location(use_kind, self.body, self.body.source_info(location).clone());
        self.on_edge(use_);
        self.super_rvalue(rvalue, location);
    }
}

impl<'a, 'tcx> PostMonoNode<'a, 'tcx> {
    fn find_dynamic_cast(&self, rval: &Rvalue<'tcx>) -> Option<(Span, UseKind<'tcx>)> {
        match rval {
            Rvalue::Cast(CastKind::PointerCoercion(PointerCoercion::ReifyFnPointer(_), _), operand, _fn_ptr_ty) => {
                let span = operand.span(self.body);
                self.resolve_fn_operand_opt(operand, span)
                    .map(|instance| (span, UseKind::FnPtrCast(instance)))
            }
            _ => None,
        }
    }

    /// On MIR callgraph edge.
    ///
    /// An edge is a [`Use`] in the definition of a monomorphized [`Instance`].
    fn on_edge(&mut self, use_: Use<'tcx>) {
        self.state.callgraph.register_edge(self.instance, use_.clone());

        match use_.kind {
            UseKind::BuiltinMirAssert | UseKind::AsyncPanic => {
                // Non-recursive leaf node
                return;
            }
            // In any other case we should have fully monomorphized the function.
            UseKind::Called(_) | UseKind::VTableCalled(_) | UseKind::FnPtrCast(_) => {
                let callee_instance = use_.opt_instance().unwrap_or_else(|| {
                    span_bug!(
                        use_.span,
                        "called expect_instance on a use_ that does not have an instance"
                    )
                });

                if matches!(callee_instance.def, InstanceKind::Virtual(..)) {
                    self.visit_virtual(*callee_instance, use_.span);
                } else {
                    PostMonoNode::visit_instance(self.state, *callee_instance);
                }
            }
            UseKind::Entrypoint(..) => {
                span_bug!(
                    use_.span,
                    "Entrypoint is a root node of the graph, and cannot be logically generated mid-analysis"
                );
            }
        }
    }

    fn visit_virtual(&mut self, instance: Instance<'tcx>, span: Span) {
        let InstanceKind::Virtual(virtual_def_id, _) = instance.def else {
            span_bug!(self.body.span, "Visiting a non-virtual {instance:?}");
        };

        if !self.state.callgraph.visit(instance) {
            debug!("already linted {instance:?}");
            return;
        }

        info!("visiting virtual {instance:?}");

        let mut this: PostMonoNode<'_, 'tcx> = PostMonoNode {
            instance,
            body: self.body, // Re-use old body
            state: self.state,
        };

        for method in dyn_object_call_to_impl_methods(virtual_def_id, this.instance.args, this.state.tcx) {
            // Add a 'fake' use to each of the VTable entries.
            this.on_edge(Use {
                kind: UseKind::VTableCalled(method),
                hir_id: None,
                span,
            });
        }
    }

    fn visit_instance(state: &mut AnalysisState<'tcx>, mut instance: Instance<'tcx>) {
        if !state.callgraph.visit(instance) {
            return;
        }

        let owner = instance.def_id();

        if state.tcx.intrinsic(owner).is_some() {
            // When it is an intrinsic, build new raw instance for it.
            instance = ty::Instance::new_raw(owner, instance.args);
        }

        if !state.tcx.is_mir_available(owner) {
            // Ignore if MIR is not available for the current process.
            // Possibly other process for dependency crate will generate the callgraph for this call,
            // which will be merged later.
            return;
        }

        let body = state.tcx.instance_mir(instance.def);

        let mut this: PostMonoNode<'_, 'tcx> = PostMonoNode { instance, body, state };

        if let Some(CoroutineKind::Desugared(CoroutineDesugaring::Async, _)) = body.coroutine_kind() {
            // TODO this implementation block is fragile.
            debug!("visiting (async coroutine) {instance:#?} {body:#?}");

            // Visit all BB's associated with Unresumed discriminators.
            // Do not visit "Returned" and "Panicked" discriminator bb's, instead emit a UseKind::AsyncPanic
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

                        this.on_edge(Use::for_location(UseKind::AsyncPanic, body, source_info));
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

    /// For a given terminator, yield all definitions that are called.
    fn get_call_def_mir(&self, terminator: &Terminator<'tcx>) -> Vec<Use<'tcx>> {
        let tcx = self.state.tcx;
        let span = terminator.source_info.span;

        let call_instance = match &terminator.kind {
            TerminatorKind::Call { func, .. } | TerminatorKind::TailCall { func, .. } => {
                if let Some(instance) = self.resolve_fn_operand_opt(func, span) {
                    instance
                } else {
                    return vec![];
                }
            }
            TerminatorKind::Drop { place, .. } => {
                let ty = self.instance.instantiate_mir_and_normalize_erasing_regions(
                    tcx,
                    TypingEnv::codegen(tcx, self.instance.def_id()),
                    EarlyBinder::bind(tcx, place.ty(self.body, tcx)),
                );
                Instance::resolve_drop_glue(tcx, ty.ty)
            }
            _ => return vec![],
        };

        vec![Use::for_location(
            UseKind::Called(call_instance),
            self.body,
            terminator.source_info,
        )]
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
