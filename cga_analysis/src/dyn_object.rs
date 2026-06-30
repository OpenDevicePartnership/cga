use rustc_hir::def_id::DefId;
use rustc_log::tracing::*;
use rustc_middle::ty::{GenericArgs, GenericArgsRef, Instance, TyCtxt, TypingEnv};
use rustc_type_ir::TypeVisitableExt;

pub(crate) fn dyn_object_call_to_impl_methods<'tcx>(
    virtual_def_id: DefId,
    // Not necessary?
    _args: GenericArgsRef<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> Vec<Instance<'tcx>> {
    let trait_def_id = tcx.parent(virtual_def_id);

    debug!("determining implementors {virtual_def_id:?} by checking out {trait_def_id:?}");

    let typing_env = TypingEnv::codegen(tcx, virtual_def_id);

    // When a dyn object vtable call is made, assume all impls of that trait are called.
    let trait_impls = tcx.trait_impls_of(trait_def_id);
    trait_impls
        .blanket_impls()
        .iter()
        .chain(trait_impls.non_blanket_impls().values().flatten())
        .filter_map(|impl_def_id| {
            let Ok(impl_trait) = tcx.try_normalize_erasing_regions(
                typing_env,
                tcx.impl_trait_ref(*impl_def_id).instantiate_identity(),
            ) else {
                info!("Failed to normalize {impl_def_id:?}");
                return None;
            };
            let impl_trait_ty = impl_trait.self_ty();

            // Filter out uninstantiated impls with generic parameters.
            // TODO Unclear why we encounter these in monomorphized code, probably a bug somewhere.
            if impl_trait_ty.has_non_region_infer() || impl_trait_ty.has_non_region_param() {
                return None;
            }

            let impl_args = GenericArgs::identity_for_item(tcx, *impl_def_id);

            // Get the type for thing having an impl for the trait.
            let self_ty = tcx
                .type_of(*impl_def_id)
                .instantiate(tcx, impl_args)
                .skip_norm_wip();

            // Replace dyn binder Self with a concrete Self of the type of self_ty.
            // Without this replacement a &dyn would be passed to the function, but it expects &self.
            let trait_args = tcx.mk_args_from_iter(
                std::iter::once(self_ty.into()).chain(
                    GenericArgs::identity_for_item(tcx, virtual_def_id)
                        .iter()
                        .skip(1),
                ),
            );

            if let Ok(instance_opt) =
                Instance::try_resolve(tcx, typing_env, virtual_def_id, trait_args)
            {
                instance_opt
            } else {
                None
            }
        })
        .collect()
}
