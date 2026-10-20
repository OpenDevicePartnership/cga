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

use rustc_data_structures::{graph::StartNode, unord::UnordSet};
use rustc_hir::{CoroutineDesugaring, CoroutineKind, def_id::DefId};
use rustc_log::tracing::*;
use rustc_middle::mir::{self, Body, CastKind, Location, Rvalue, Terminator, TerminatorKind, visit::Visitor as _};
use rustc_middle::mono::MonoItem;
use rustc_middle::ty::adjustment::PointerCoercion;
use rustc_middle::ty::{
    self, EarlyBinder, GenericArgsRef, Instance, InstanceKind, ShimKind, TyCtxt, TypeFoldable, TypingEnv,
};
use rustc_span::{Span, span_bug};
use rustc_type_ir::CoroutineArgs;

use crate::dyn_object::dyn_object_call_to_impl_methods;
use crate::{AnalysisState, Callgraph, Use, UseKind};

/// A node in the post-monomorphisation analysis.
struct PostMonoNode<'b, 'a, 'tcx> {
    /// The function we are currently traversing.
    instance: Instance<'tcx>,

    /// Its body.
    body: &'tcx Body<'tcx>,

    // The global analysis context.
    state: &'b mut AnalysisState<'a, 'tcx>,
}

/// Analyse all used items recursively, starting from roots.
pub fn generate_callgraph<'tcx>(tcx: TyCtxt<'tcx>, roots: UnordSet<MonoItem<'tcx>>) -> Callgraph<'tcx> {
    trace!("all roots: {roots:?}");

    let mut callgraph = Callgraph::new();

    // We need to sort these for query stability.
    let roots = tcx.with_stable_hashing_context(move |mut hcx| roots.into_sorted(&mut hcx, true));
    for root in roots {
        let instance = match root {
            // global asm is always an exported constraint
            MonoItem::GlobalAsm(..) => continue,
            // NOTE: `mono` panics if item has generics rather than silently doing the wrong thing
            MonoItem::Static(def_id) => Instance::mono(tcx, def_id),
            MonoItem::Fn(instance) => {
                let def = instance.def_id();

                // Skip std::rt::lang_start. Technically we could lint on it as if it were the span
                // of `main`, but the lint would never be useful.
                // Note that we may not have `lang_start` yet if we're still compiling core.
                if Some(def) == tcx.lang_items().start_fn() {
                    continue;
                }

                instance
            }
        };
        debug!("linting root: {instance:?}");
        let mut state = AnalysisState {
            tcx,
            callgraph: &mut callgraph,
        };

        state.register_root(instance);

        PostMonoNode::visit_instance(&mut state, instance);
    }

    callgraph
}

impl<'b, 'a, 'tcx> mir::visit::Visitor<'tcx> for PostMonoNode<'b, 'a, 'tcx> {
    fn visit_terminator(&mut self, terminator: &Terminator<'tcx>, location: Location) {
        debug!("visit_terminator");
        for use_ in self.get_call_def_mir(terminator) {
            debug!("visit_terminator def mir {use_:#?}");
            self.on_edge(use_);
        }

        if let TerminatorKind::Assert { .. } = terminator.kind {
            debug!("visit_terminator assert");
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

impl<'b, 'a, 'tcx> PostMonoNode<'b, 'a, 'tcx> {
    fn find_dynamic_cast(&self, rval: &Rvalue<'tcx>) -> Option<(Span, UseKind<'tcx>)> {
        match rval {
            Rvalue::Cast(CastKind::PointerCoercion(PointerCoercion::ReifyFnPointer(_), _), operand, _fn_ptr_ty) => {
                let call_span = operand.span(self.body);
                let Some((pre_mono_callee, generic_args)) = operand.const_fn_def() else {
                    span_bug!(
                        call_span,
                        "don't know how to handle ReifyFnPointer cast of non-constant fn {operand:?}"
                    );
                };

                let callee_instance = self.monomorphize_instance(pre_mono_callee, generic_args, call_span);

                Some((call_span, UseKind::FnPtrCast(callee_instance)))
            }
            _ => None,
        }
    }

    /// On MIR callgraph edge.
    ///
    /// An edge is a [`Use`] in the definition of a monomorphized [`Instance`].
    fn on_edge(&mut self, use_: Use<'tcx>) {
        info!("edge {:#?} {use_:#?}", self.instance);

        self.state.register_edge(self.instance, use_.clone());

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

        let mut this: PostMonoNode<'_, 'a, 'tcx> = PostMonoNode {
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

    fn visit_instance(state: &mut AnalysisState<'a, 'tcx>, mut instance: Instance<'tcx>) {
        if !state.callgraph.visit(instance) {
            debug!("already linted {instance:?}");
            return;
        }

        info!("visiting {instance:?}");

        let owner = instance.def_id();

        if let Some(intrinsic) = state.tcx.intrinsic(owner) {
            if intrinsic.must_be_overridden {
                // Instrinsics with no fallback body are qualified as part of the compiler,
                // and will panic in `instance_mir`.
                debug!("ignoring intrinsic {owner:?}");
                return;
            }

            // See equivalent code in `rustc_monomorphize::collector::visit_instance_use`.
            if state.tcx.sess.replaced_intrinsics.contains(&intrinsic.name) {
                // This is normal: LLVM in particular has specialized overrides for many
                // integer operations. But that means the fallback body won't actually be
                // used, so don't try to check it.
                debug!("ignoring overridden intrinsic body for {owner:?}");
                return;
            }

            debug!("using fallback body for {owner:?}");
            instance = ty::Instance::new_raw(owner, instance.args);
        } else if !state.tcx.is_mir_available(owner) {
            info!("no MIR for {owner:?}");
            return;
        }

        let body = state.tcx.instance_mir(instance.def);
        info!("instanced {instance:?}");

        let mut this: PostMonoNode<'_, 'a, 'tcx> = PostMonoNode { instance, body, state };

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
                let Some((pre_mono_call, generic_args)) = func.const_fn_def() else {
                    match func.ty(self.body, tcx).kind() {
                        kind @ ty::FnDef(..) => {
                            span_bug!(span, "{kind:?} should have been a const_fn_def?")
                        }
                        ty::FnPtr(..) => {}
                        _ => {
                            // If this is anything other than a function item, it can't have generics and
                            // therefore must have been checked by the THIR pass.
                            // FIXME: are we sure is this true when we're passed an `impl Fn`?
                            tcx.dcx()
                                .span_delayed_bug(span, format!("called a non-function? {func:?}"));
                        }
                    }
                    debug!("ignoring call to non-constant function {func:?}");
                    return vec![];
                };
                self.monomorphize_instance(pre_mono_call, generic_args, span)
            }
            TerminatorKind::Drop { place, .. } => {
                let (ty, _) = self.monomorphize_args(place.ty(self.body, tcx));
                let instance = Instance::resolve_drop_glue(tcx, ty.ty);
                if matches!(instance.def, InstanceKind::Shim(ShimKind::DropGlue(_, None))) {
                    debug!("ty `{ty:?}` does not need to be dropped");
                    return vec![];
                }
                debug!("resolve drop glue => instance={instance:?}, ty={ty:?}");
                instance
            }
            _ => return vec![],
        };

        vec![Use::for_location(
            UseKind::Called(call_instance),
            self.body,
            terminator.source_info,
        )]
    }

    fn monomorphize_instance(&self, def_id: DefId, generic_args: GenericArgsRef<'tcx>, span: Span) -> Instance<'tcx> {
        info!(
            "monomorphize_instance {def_id:?} {generic_args:?} for {:?}",
            self.instance
        );
        let (mono_args, typing_env) = self.monomorphize_args(generic_args);
        Instance::expect_resolve(self.state.tcx, typing_env, def_id, mono_args, span)
    }

    fn monomorphize_args<T>(&self, generic_args: T) -> (T, TypingEnv<'tcx>)
    where
        T: TypeFoldable<TyCtxt<'tcx>>,
    {
        let tcx = self.state.tcx;

        let env = TypingEnv::codegen(tcx, self.instance.def_id());
        let args =
            self.instance
                .instantiate_mir_and_normalize_erasing_regions(tcx, env, EarlyBinder::bind(tcx, generic_args));

        info!("monomorphize_args OK");
        (args, env)
    }
}
