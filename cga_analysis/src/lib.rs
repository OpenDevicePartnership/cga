#![feature(rustc_private)]
#![feature(impl_trait_in_assoc_type)]

extern crate rustc_abi;
extern crate rustc_attr_ir;
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_index;
extern crate rustc_infer;
extern crate rustc_interface;
extern crate rustc_lint;
extern crate rustc_lint_defs;
extern crate rustc_log;
extern crate rustc_macros;
extern crate rustc_middle;
extern crate rustc_mir_transform;
extern crate rustc_monomorphize;
extern crate rustc_public;
extern crate rustc_serialize;
extern crate rustc_session;
extern crate rustc_span;
extern crate rustc_trait_selection;
extern crate rustc_type_ir;

mod dyn_object;
mod errors;
mod panics;

pub mod analysis;
pub mod callgraph;
pub mod diagnostics;
pub mod monomorphize;

use rustc_hir::{HirId, def_id::DefId};
use rustc_lint_defs::declare_tool_lint;
use rustc_middle::{
    mir::{Body, SourceInfo},
    ty::{Instance, print::with_resolve_crate_name},
};
use rustc_span::Span;

declare_tool_lint! {
    /// The `cga::panics` lint detects code that calls panicking functions.
    pub cga::PANICS,
    Deny,
    "a function (indirectly) panics",
    report_in_external_macro: true
}

declare_tool_lint! {
    /// The `cga::blacklisted` lint detects code that was blacklisted.
    pub cga::BLACKLISTED,
    Deny,
    "a function (indirectly) is blacklisted",
    report_in_external_macro: true
}

use crate::callgraph::Callgraph;

/// The [Arrow] from a site in the MIR tree, to something denoted by [ArrowTarget].
///
/// The origin of the [Arrow] is loosely denoted by its span and possibly the local [HirId].
/// Thus getting the origin of an [Arrow] is not possible without the context of an entire [Callgraph].
///
/// Note that for [ArrowTarget::Entrypoint] a pseudo-Arrow is created that points from itself to itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Arrow<'tcx> {
    /// The kind associated with the [`Arrow`].
    ///
    /// When calling a function, refers to the monomorphized [`Instance`] of the function called.
    target: ArrowTarget<'tcx>,

    /// The origin location of the [`Arrow`].
    span: Span,

    /// The HIR node associated with the origin location, necessary for emitting diagnostics.
    ///
    /// Only applicable for nodes within our local crate.
    hir_id: Option<HirId>,
}

/// Pointy end of an [Arrow], denoting what is reachable.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ArrowTarget<'tcx> {
    /// The instance is an entrypoint for the application.
    ///
    /// And entrypoint could be an extern "c" function, an ISR, main, etc.
    Entrypoint(Instance<'tcx>),

    /// An [`Instance`] function is called directly.
    Called(Instance<'tcx>),

    /// An [`Instance`] function is possibly called via a dyn object vtable.
    VTableCalled(Instance<'tcx>),

    /// An [`Instance`] function pointer is created here, and might possibly be called later.
    FnPtrCast(Instance<'tcx>),

    /// A builtin assertion in MIR which can panic.
    ///
    /// For example OverflowAdd will generate a terminator with [rustc_middle::mir::TerminatorKind::Assert].
    BuiltinMirAssert,

    /// The implicit panic(s) associated with any async coroutine.
    ///
    /// All async coroutines have a "poll after Ready" state, which always panics.
    /// These panics are (as of yet) unavoidable when using async.
    ///
    /// Because there is no mitigation for this, we put these panics into a separate category.
    AsyncPanic,
}

impl<'tcx> Arrow<'tcx> {
    fn for_location(target: ArrowTarget<'tcx>, body: &Body<'tcx>, source: SourceInfo) -> Arrow<'tcx> {
        Arrow {
            target,
            span: source.span,
            hir_id: source.scope.lint_root(&body.source_scopes),
        }
    }

    pub fn opt_def_id(&self) -> Option<DefId> {
        match self.target {
            ArrowTarget::Called(instance)
            | ArrowTarget::VTableCalled(instance)
            | ArrowTarget::FnPtrCast(instance)
            | ArrowTarget::Entrypoint(instance) => Some(instance.def_id()),
            ArrowTarget::BuiltinMirAssert => None,
            ArrowTarget::AsyncPanic => None,
        }
    }

    pub fn opt_instance(&self) -> Option<&Instance<'tcx>> {
        match &self.target {
            ArrowTarget::FnPtrCast(instance)
            | ArrowTarget::Called(instance)
            | ArrowTarget::VTableCalled(instance)
            | ArrowTarget::Entrypoint(instance) => Some(instance),
            ArrowTarget::BuiltinMirAssert | ArrowTarget::AsyncPanic => None,
        }
    }
}

/// The canonical path (or name) of an instance used throughout this crate.
///
/// Used as the qualifiers in the exported callgraph, as well as in the lint backtrace and to which rules are applied.
pub(crate) fn canonical_path(instance: &Instance<'_>) -> String {
    with_resolve_crate_name!(format!("{instance}"))
}

pub(crate) fn simplified_path(instance: &Instance<'_>) -> String {
    with_resolve_crate_name!(rustc_middle::ty::tls::with(|tcx| tcx.def_path_str(instance.def_id())))
}
