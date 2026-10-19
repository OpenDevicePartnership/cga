//! A shallow fork of [`rustc_monomorphize`].
//!
//! We need to use [`collect_roots`] for our analysis pass, but this function is not exposed by [`rustc_monomorphize`].
//! Instead we have forked the crate to regain the same functionality.
//!
//! `NoConstOrStatic` has been added, as well as an extra check for the existence of the `no_mangle` and `export_name` attributes.

use rustc_attr_ir::{HasAttrs, InlineAttr, lang_items::LangItem};
use rustc_data_structures::fx::FxIndexMap;
use rustc_hir::{
    ImplItemId, ItemId,
    def::DefKind,
    def_id::{DefId, LocalDefId},
};
use rustc_middle::{
    middle::codegen_fn_attrs::CodegenFnAttrFlags,
    mir::{self},
    mono::MonoItem,
    ty::{self, GenericParamDefKind, Instance, Ty, TyCtxt, Unnormalized, layout::ValidityRequirement},
};
use rustc_middle::{
    mir::interpret::{AllocId, GlobalAlloc, Scalar},
    ty::GenericArgs,
};
use rustc_session::config::EntryFnType;
use rustc_span::{DUMMY_SP, Span, Spanned, bug, dummy_spanned, respan};
use rustc_type_ir::TypeVisitableExt;

use rustc_log::tracing::*;

#[derive(PartialEq)]
pub enum MonoItemCollectionStrategy {
    /// Collect all items, including const items that are not necessarily used.
    Eager,
    /// Collect all items, but only const items that are directly used.
    Lazy,
    /// Only collect extern and normal entrypoints.
    ///
    /// Do not collect const/static.
    NoConstOrStatic,
}

struct MonoItems<'tcx> {
    // We want a set of MonoItem + Span where trying to re-insert a MonoItem with a different Span
    // is ignored. Map does that, but it looks odd.
    items: FxIndexMap<MonoItem<'tcx>, Span>,
}

impl<'tcx> MonoItems<'tcx> {
    fn new() -> Self {
        Self {
            items: FxIndexMap::default(),
        }
    }

    fn push(&mut self, item: Spanned<MonoItem<'tcx>>) {
        // Insert only if the entry does not exist. A normal insert would stomp the first span that
        // got inserted.
        self.items.entry(item.node).or_insert(item.span);
    }
}

impl<'tcx> IntoIterator for MonoItems<'tcx> {
    type Item = Spanned<MonoItem<'tcx>>;
    type IntoIter = impl Iterator<Item = Spanned<MonoItem<'tcx>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter().map(|(item, span)| respan(span, item))
    }
}

impl<'tcx> Extend<Spanned<MonoItem<'tcx>>> for MonoItems<'tcx> {
    fn extend<I>(&mut self, iter: I)
    where
        I: IntoIterator<Item = Spanned<MonoItem<'tcx>>>,
    {
        for item in iter {
            self.push(item)
        }
    }
}

// Find all non-generic items by walking the HIR. These items serve as roots to
// start monomorphizing from.
pub fn collect_roots(tcx: TyCtxt<'_>, mode: MonoItemCollectionStrategy) -> Vec<MonoItem<'_>> {
    debug!("collecting roots");
    let mut roots = MonoItems::new();

    {
        let entry_fn = tcx.entry_fn(());

        debug!("collect_roots: entry_fn = {:?}", entry_fn);

        let mut collector = RootCollector {
            tcx,
            strategy: mode,
            entry_fn,
            output: &mut roots,
        };

        let crate_items = tcx.hir_crate_items(());

        for id in crate_items.free_items() {
            collector.process_item(id);
        }

        for id in crate_items.impl_items() {
            collector.process_impl_item(id);
        }

        for id in crate_items.nested_bodies() {
            collector.process_nested_body(id);
        }

        collector.push_extra_entry_roots();
    }

    // We can only codegen items that are instantiable - items all of
    // whose predicates hold. Luckily, items that aren't instantiable
    // can't actually be used, so we can just skip codegenning them.
    roots
        .into_iter()
        .filter_map(|Spanned { node: mono_item, .. }| mono_item.is_instantiable(tcx).then_some(mono_item))
        .collect()
}

struct RootCollector<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    strategy: MonoItemCollectionStrategy,
    output: &'a mut MonoItems<'tcx>,
    entry_fn: Option<(DefId, EntryFnType)>,
}

impl<'v> RootCollector<'_, 'v> {
    fn process_item(&mut self, id: ItemId) {
        match self.tcx.def_kind(id.owner_id) {
            DefKind::Enum | DefKind::Struct | DefKind::Union => {
                if self.strategy == MonoItemCollectionStrategy::Eager
                    && !self.tcx.generics_of(id.owner_id).requires_monomorphization(self.tcx)
                {
                    debug!("RootCollector: ADT drop-glue for `{id:?}`",);
                    let id_args =
                        ty::GenericArgs::for_item(self.tcx, id.owner_id.to_def_id(), |param, _| match param.kind {
                            GenericParamDefKind::Lifetime => self.tcx.lifetimes.re_erased.into(),
                            GenericParamDefKind::Type { .. } | GenericParamDefKind::Const { .. } => {
                                unreachable!(
                                    "`own_requires_monomorphization` check means that \
                                we should have no type/const params"
                                )
                            }
                        });

                    // This type is impossible to instantiate, so we should not try to
                    // generate a `drop_in_place` instance for it.
                    if self
                        .tcx
                        .instantiate_and_check_impossible_clauses((id.owner_id.to_def_id(), id_args))
                    {
                        return;
                    }

                    let ty = self.tcx.type_of(id.owner_id.to_def_id()).instantiate(self.tcx, id_args);
                    // FIXME CGA rustc #155345
                    let ty = ty.skip_norm_wip();
                    assert!(!ty.has_non_region_param());

                    visit_drop_use(self.tcx, ty, true, DUMMY_SP, self.output);
                }
            }
            DefKind::GlobalAsm => {
                debug!(
                    "RootCollector: ItemKind::GlobalAsm({})",
                    self.tcx.def_path_str(id.owner_id)
                );
                self.output.push(dummy_spanned(MonoItem::GlobalAsm(id)));
            }
            DefKind::Static { .. } => match self.strategy {
                MonoItemCollectionStrategy::NoConstOrStatic => {}
                _ => {
                    let def_id = id.owner_id.to_def_id();
                    debug!("RootCollector: ItemKind::Static({})", self.tcx.def_path_str(def_id));
                    self.output.push(dummy_spanned(MonoItem::Static(def_id)));
                }
            },
            DefKind::Const { .. } => {
                // Const items only generate mono items if they are actually used somewhere.
                // Just declaring them is insufficient.

                // If we're collecting items eagerly, then recurse into all constants.
                // Otherwise the value is only collected when explicitly mentioned in other items.
                if self.strategy == MonoItemCollectionStrategy::Eager {
                    let def_id = id.owner_id.to_def_id();
                    // Type Consts don't have bodies to evaluate
                    // nor do they make sense as a static.
                    if self.tcx.const_of_item(def_id).is_some() {
                        // FIXME(mgca): Is this actually what we want? We may want to
                        // normalize to a ValTree then convert to a const allocation and
                        // collect that?
                        return;
                    }
                    if self.tcx.generics_of(id.owner_id).own_requires_monomorphization() {
                        return;
                    }
                    let Ok(val) = self.tcx.const_eval_poly(def_id) else {
                        return;
                    };
                    collect_const_value(self.tcx, val, self.output);
                }
            }
            DefKind::Impl { of_trait: true } => {
                if self.strategy == MonoItemCollectionStrategy::Eager {
                    create_mono_items_for_default_impls(self.tcx, id, self.output);
                }
            }
            DefKind::Fn => {
                self.push_if_root(id.owner_id.def_id);
            }
            _ => {}
        }
    }

    fn process_impl_item(&mut self, id: ImplItemId) {
        if self.tcx.def_kind(id.owner_id) == DefKind::AssocFn {
            self.push_if_root(id.owner_id.def_id);
        }
    }

    fn process_nested_body(&mut self, def_id: LocalDefId) {
        match self.tcx.def_kind(def_id) {
            DefKind::Closure => {
                // for 'pub async fn foo(..)' also trying to monomorphize foo::{closure}
                let is_pub_fn_coroutine = match *self.tcx.type_of(def_id).instantiate_identity().skip_norm_wip().kind()
                {
                    ty::Coroutine(cor_id, _args) => {
                        let tcx = self.tcx;
                        let parent_id = tcx.parent(cor_id);
                        tcx.def_kind(parent_id) == DefKind::Fn
                            && tcx.asyncness(parent_id).is_async()
                            && tcx.visibility(parent_id).is_public()
                    }
                    ty::Closure(..) | ty::CoroutineClosure(..) => false,
                    _ => unreachable!(),
                };
                if (self.strategy == MonoItemCollectionStrategy::Eager || is_pub_fn_coroutine)
                    && !self
                        .tcx
                        .generics_of(self.tcx.typeck_root_def_id_local(def_id))
                        .requires_monomorphization(self.tcx)
                {
                    let instance = match *self.tcx.type_of(def_id).instantiate_identity().skip_norm_wip().kind() {
                        ty::Closure(def_id, args)
                        | ty::Coroutine(def_id, args)
                        | ty::CoroutineClosure(def_id, args) => {
                            Instance::new_raw(def_id, self.tcx.erase_and_anonymize_regions(args))
                        }
                        _ => unreachable!(),
                    };
                    let Ok(instance) = self.tcx.try_normalize_erasing_regions(
                        ty::TypingEnv::fully_monomorphized(),
                        Unnormalized::new_wip(instance),
                    ) else {
                        // Don't ICE on an impossible-to-normalize closure.
                        return;
                    };
                    let mono_item = create_fn_mono_item(self.tcx, instance, DUMMY_SP);
                    if mono_item.node.is_instantiable(self.tcx) {
                        self.output.push(mono_item);
                    }
                }
            }
            _ => {}
        }
    }

    fn is_root(&self, def_id: LocalDefId) -> bool {
        !self.tcx.generics_of(def_id).requires_monomorphization(self.tcx)
            && (self.is_extern(def_id)
                || match self.strategy {
                    MonoItemCollectionStrategy::Eager => {
                        !matches!(self.tcx.codegen_fn_attrs(def_id).inline, InlineAttr::Force { .. })
                    }
                    MonoItemCollectionStrategy::Lazy => {
                        self.entry_fn.and_then(|(id, _)| id.as_local()) == Some(def_id)
                            || self.tcx.is_reachable_non_generic(def_id)
                            || {
                                let attrs = self.tcx.codegen_fn_attrs(def_id);
                                attrs.flags.intersects(
                                    CodegenFnAttrFlags::RUSTC_STD_INTERNAL_SYMBOL
                                        | CodegenFnAttrFlags::EXTERNALLY_IMPLEMENTABLE_ITEM,
                                )
                            }
                    }
                    MonoItemCollectionStrategy::NoConstOrStatic => false,
                })
    }

    fn is_extern(&self, def_id: LocalDefId) -> bool {
        let attrs = self.tcx.codegen_fn_attrs(def_id);

        if attrs.flags.contains(CodegenFnAttrFlags::FOREIGN_ITEM) {
            return false;
        }

        // If it is a builtin, it is not an extern. (used to remove weakly linked compiler builtins)
        if self.tcx.is_compiler_builtins(def_id.to_def_id().krate) {
            return false;
        }

        let has_export_link_name = HasAttrs::get_attrs(def_id, &self.tcx).iter().any(|attr| match attr {
            rustc_attr_ir::Attribute::Parsed(rustc_attr_ir::AttributeKind::ExportName { .. }) => true,
            _ => false,
        });
        let no_mangle = attrs.flags.contains(CodegenFnAttrFlags::NO_MANGLE);

        has_export_link_name || no_mangle
    }

    /// If `def_id` represents a root, pushes it onto the list of
    /// outputs. (Note that all roots must be monomorphic.)

    fn push_if_root(&mut self, def_id: LocalDefId) {
        if self.is_root(def_id) {
            let instance = Instance::mono(self.tcx, def_id.to_def_id());
            self.output.push(create_fn_mono_item(self.tcx, instance, DUMMY_SP));
        }
    }

    /// As a special case, when/if we encounter the
    /// `main()` function, we also have to generate a
    /// monomorphized copy of the start lang item based on
    /// the return type of `main`. This is not needed when
    /// the user writes their own `start` manually.
    fn push_extra_entry_roots(&mut self) {
        let Some((main_def_id, EntryFnType::Main { .. })) = self.entry_fn else {
            return;
        };

        let main_instance = Instance::mono(self.tcx, main_def_id);
        if self.tcx.should_codegen_locally(main_instance) {
            self.output.push(create_fn_mono_item(
                self.tcx,
                main_instance,
                self.tcx.def_span(main_def_id),
            ));
        }

        let Some(start_def_id) = self.tcx.lang_items().start_fn() else {
            self.tcx.dcx().emit_fatal(crate::errors::StartNotFound);
        };
        let main_ret_ty = self.tcx.fn_sig(main_def_id).no_bound_vars().unwrap().output();

        // Given that `main()` has no arguments,
        // then its return type cannot have
        // late-bound regions, since late-bound
        // regions must appear in the argument
        // listing.
        let main_ret_ty = self.tcx.normalize_erasing_regions(
            ty::TypingEnv::fully_monomorphized(),
            Unnormalized::new_wip(main_ret_ty.no_bound_vars().unwrap()),
        );

        let start_instance = Instance::expect_resolve(
            self.tcx,
            ty::TypingEnv::fully_monomorphized(),
            start_def_id,
            self.tcx.mk_args(&[main_ret_ty.into()]),
            DUMMY_SP,
        );

        self.output
            .push(create_fn_mono_item(self.tcx, start_instance, DUMMY_SP));
    }
}

fn visit_drop_use<'tcx>(
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
    is_direct_call: bool,
    source: Span,
    output: &mut MonoItems<'tcx>,
) {
    let instance = Instance::resolve_drop_glue(tcx, ty);
    visit_instance_use(tcx, instance, is_direct_call, source, output);
}

fn collect_const_value<'tcx>(tcx: TyCtxt<'tcx>, value: mir::ConstValue, output: &mut MonoItems<'tcx>) {
    match value {
        mir::ConstValue::Scalar(Scalar::Ptr(ptr, _size)) => collect_alloc(tcx, ptr.provenance.alloc_id(), output),
        mir::ConstValue::Indirect { alloc_id, .. } | mir::ConstValue::Slice { alloc_id, meta: _ } => {
            collect_alloc(tcx, alloc_id, output)
        }
        _ => {}
    }
}

fn visit_instance_use<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
    is_direct_call: bool,
    source: Span,
    output: &mut MonoItems<'tcx>,
) {
    debug!("visit_item_use({:?}, is_direct_call={:?})", instance, is_direct_call);
    if !tcx.should_codegen_locally(instance) {
        return;
    }
    if let Some(intrinsic) = tcx.intrinsic(instance.def_id()) {
        if let Some(_requirement) = ValidityRequirement::from_intrinsic(intrinsic.name) {
            // The intrinsics assert_inhabited, assert_zero_valid, and assert_mem_uninitialized_valid will
            // be lowered in codegen to nothing or a call to panic_nounwind. So if we encounter any
            // of those intrinsics, we need to include a mono item for panic_nounwind, else we may try to
            // codegen a call to that function without generating code for the function itself.
            let def_id = tcx.require_lang_item(LangItem::PanicNounwind, source);
            let panic_instance = Instance::mono(tcx, def_id);
            if tcx.should_codegen_locally(panic_instance) {
                output.push(create_fn_mono_item(tcx, panic_instance, source));
            }
        } else if !intrinsic.must_be_overridden && !tcx.sess.replaced_intrinsics.contains(&intrinsic.name) {
            // Codegen the fallback body of intrinsics with fallback bodies.
            // We have to skip this otherwise as there's no body to codegen.
            // We also skip intrinsics the backend handles, to reduce monomorphizations.
            let instance = ty::Instance::new_raw(instance.def_id(), instance.args);
            if tcx.should_codegen_locally(instance) {
                output.push(create_fn_mono_item(tcx, instance, source));
            }
        }
    }

    match instance.def {
        ty::InstanceKind::Virtual(..) | ty::InstanceKind::LlvmIntrinsic(_) | ty::InstanceKind::Intrinsic(_) => {
            if !is_direct_call {
                bug!("{:?} being reified", instance);
            }
        }
        ty::InstanceKind::Shim(ty::ShimKind::ThreadLocal(..)) => {
            bug!("{:?} being reified", instance);
        }
        ty::InstanceKind::Shim(ty::ShimKind::DropGlue(_, None)) => {
            // Don't need to emit noop drop glue if we are calling directly.
            //
            // Note that we also optimize away the call to visit_instance_use in vtable construction
            // (see create_mono_items_for_vtable_methods).
            if !is_direct_call {
                output.push(create_fn_mono_item(tcx, instance, source));
            }
        }
        ty::InstanceKind::Shim(
            ty::ShimKind::DropGlue(_, Some(_))
            | ty::ShimKind::FutureDropPoll(..)
            | ty::ShimKind::AsyncDropGlue(_, _)
            | ty::ShimKind::AsyncDropGlueCtor(_, _)
            | ty::ShimKind::VTable(..)
            | ty::ShimKind::Reify(..)
            | ty::ShimKind::ClosureOnce { .. }
            | ty::ShimKind::ConstructCoroutineInClosure { .. }
            | ty::ShimKind::FnPtr(..)
            | ty::ShimKind::Clone(..)
            | ty::ShimKind::FnPtrAsPtr(..)
            | ty::ShimKind::FnPtrFromPtr(..),
        )
        | ty::InstanceKind::Item(..) => {
            output.push(create_fn_mono_item(tcx, instance, source));
        }
    }
}

fn create_fn_mono_item<'tcx>(_tcx: TyCtxt<'tcx>, instance: Instance<'tcx>, source: Span) -> Spanned<MonoItem<'tcx>> {
    respan(source, MonoItem::Fn(instance))
}

/// Scans the CTFE alloc in order to find function pointers and statics that must be monomorphized.
fn collect_alloc<'tcx>(tcx: TyCtxt<'tcx>, alloc_id: AllocId, output: &mut MonoItems<'tcx>) {
    match tcx.global_alloc(alloc_id) {
        GlobalAlloc::Static(def_id) => {
            assert!(!tcx.is_thread_local_static(def_id));
            let instance = Instance::mono(tcx, def_id);
            if tcx.should_codegen_locally(instance) {
                trace!("collecting static {:?}", def_id);
                output.push(dummy_spanned(MonoItem::Static(def_id)));
            }
        }
        GlobalAlloc::Memory(alloc) => {
            trace!("collecting {:?} with {:#?}", alloc_id, alloc);
            let ptrs = alloc.inner().provenance().ptrs();
            // avoid `ensure_sufficient_stack` in the common case of "no pointers"
            if !ptrs.is_empty() {
                for &prov in ptrs.values() {
                    collect_alloc(tcx, prov.alloc_id(), output);
                }
            }
        }
        GlobalAlloc::Function { instance, .. } => {
            if tcx.should_codegen_locally(instance) {
                trace!("collecting {:?} with {:#?}", alloc_id, instance);
                output.push(create_fn_mono_item(tcx, instance, DUMMY_SP));
            }
        }
        GlobalAlloc::VTable(ty, dyn_ty) => {
            let alloc_id = tcx.vtable_allocation((
                ty,
                dyn_ty
                    .principal()
                    .map(|principal| tcx.instantiate_bound_regions_with_erased(principal)),
            ));
            collect_alloc(tcx, alloc_id, output)
        }
        GlobalAlloc::TypeId { .. } => {}
    }
}

fn create_mono_items_for_default_impls<'tcx>(tcx: TyCtxt<'tcx>, item: ItemId, output: &mut MonoItems<'tcx>) {
    let impl_ = tcx.impl_trait_header(item.owner_id);

    if impl_.polarity == ty::ImplPolarity::Negative {
        return;
    }

    if tcx.generics_of(item.owner_id).own_requires_monomorphization() {
        return;
    }

    // Lifetimes never affect trait selection, so we are allowed to eagerly
    // instantiate an instance of an impl method if the impl (and method,
    // which we check below) is only parameterized over lifetime. In that case,
    // we use the ReErased, which has no lifetime information associated with
    // it, to validate whether or not the impl is legal to instantiate at all.
    let only_region_params = |param: &ty::GenericParamDef, _: &_| match param.kind {
        GenericParamDefKind::Lifetime => tcx.lifetimes.re_erased.into(),
        GenericParamDefKind::Type { .. } | GenericParamDefKind::Const { .. } => {
            unreachable!(
                "`own_requires_monomorphization` check means that \
                we should have no type/const params"
            )
        }
    };
    let impl_args = GenericArgs::for_item(tcx, item.owner_id.to_def_id(), only_region_params);
    let trait_ref = impl_.trait_ref.instantiate(tcx, impl_args);

    // Unlike 'lazy' monomorphization that begins by collecting items transitively
    // called by `main` or other global items, when eagerly monomorphizing impl
    // items, we never actually check that the predicates of this impl are satisfied
    // in a empty param env (i.e. with no assumptions).
    //
    // Even though this impl has no type or const generic parameters, because we don't
    // consider higher-ranked predicates such as `for<'a> &'a mut [u8]: Copy` to
    // be trivially false. We must now check that the impl has no impossible-to-satisfy
    // predicates.
    if tcx.instantiate_and_check_impossible_clauses((item.owner_id.to_def_id(), impl_args)) {
        return;
    }

    let typing_env = ty::TypingEnv::fully_monomorphized();
    let trait_ref = tcx.normalize_erasing_regions(typing_env, trait_ref);
    let overridden_methods = tcx.impl_item_implementor_ids(item.owner_id);
    for method in tcx.provided_trait_methods(trait_ref.def_id) {
        if overridden_methods.contains_key(&method.def_id) {
            continue;
        }

        if tcx.generics_of(method.def_id).own_requires_monomorphization() {
            continue;
        }

        // As mentioned above, the method is legal to eagerly instantiate if it
        // only has lifetime generic parameters. This is validated by calling
        // `own_requires_monomorphization` on both the impl and method.
        let args = trait_ref.args.extend_to(tcx, method.def_id, only_region_params);
        let instance = ty::Instance::expect_resolve(tcx, typing_env, method.def_id, args, DUMMY_SP);

        let mono_item = create_fn_mono_item(tcx, instance, DUMMY_SP);
        if mono_item.node.is_instantiable(tcx) && tcx.should_codegen_locally(instance) {
            output.push(mono_item);
        }
    }
}
