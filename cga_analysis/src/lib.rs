#![feature(rustc_private)]
#![feature(impl_trait_in_assoc_type)]

extern crate rustc_abi;
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_index;
extern crate rustc_infer;
extern crate rustc_interface;
extern crate rustc_lint;
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

mod callgraph;
mod dyn_object;
mod errors;
mod panics;

pub mod diagnostics;
pub mod monomorphize;
pub mod post_mono;

use rustc_hir::{HirId, def_id::DefId};
use rustc_middle::{
    mir::{Body, SourceInfo},
    ty::{Instance, TyCtxt, print::with_resolve_crate_name},
};
use rustc_session::declare_tool_lint;
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

// for intra-doc links
#[rustfmt::skip]
#[allow(unused_imports)]
use rustc_middle::{
    mir::TerminatorKind,
    ty::{GenericArgsRef, ParamEnv},
};

use crate::callgraph::Callgraph;

/// Context used for analysis for a given root node.
struct AnalysisState<'a, 'tcx> {
    /// Type context.
    tcx: TyCtxt<'tcx>,

    /// The callgraph currently being constructed.
    callgraph: &'a mut Callgraph<'tcx>,
}

impl<'tcx> AnalysisState<'_, 'tcx> {
    pub fn register_root(&mut self, instance: Instance<'tcx>) {
        self.callgraph.register_root(instance, self.tcx);
    }

    pub fn register_edge(&mut self, from: Instance<'tcx>, to: Use<'tcx>) {
        self.callgraph.register_edge(from, to);
    }
}

/// A Use of a function or a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Use<'tcx> {
    /// The kind associated with the [`Use`].
    ///
    /// When calling a function, refers to the monomorphized [`Instance`] of the function called.
    kind: UseKind<'tcx>,

    /// The calling location of the [`Use`].
    span: Span,

    /// The HIR node associated with the calling location, necessary for emitting diagnostics.
    ///
    /// Only applicable for nodes within our local crate.
    hir_id: Option<HirId>,
}

/// Nodes in a call graph.
///
/// Each node is a *possible* call to monomorphized functions (possibly), or panic.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum UseKind<'tcx> {
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
    /// For example OverflowAdd will generate a terminator with [TerminatorKind::Assert].
    BuiltinMirAssert,

    /// The implicit panic(s) associated with any async coroutine.
    ///
    /// All async coroutines have a "poll after Ready" state, which always panics.
    /// These panics are (as of yet) unavoidable when using async.
    ///
    /// Because there is no mitigation for this, we put these panics into a separate category.
    AsyncPanic,
}

impl<'tcx> Use<'tcx> {
    fn for_location(kind: UseKind<'tcx>, body: &Body<'tcx>, source: SourceInfo) -> Use<'tcx> {
        Use {
            kind,
            span: source.span,
            hir_id: source.scope.lint_root(&body.source_scopes),
        }
    }

    pub fn opt_def_id(&self) -> Option<DefId> {
        match self.kind {
            UseKind::Called(instance)
            | UseKind::VTableCalled(instance)
            | UseKind::FnPtrCast(instance)
            | UseKind::Entrypoint(instance) => Some(instance.def_id()),
            UseKind::BuiltinMirAssert => None,
            UseKind::AsyncPanic => None,
        }
    }

    pub fn opt_instance(&self) -> Option<&Instance<'tcx>> {
        match &self.kind {
            UseKind::FnPtrCast(instance)
            | UseKind::Called(instance)
            | UseKind::VTableCalled(instance)
            | UseKind::Entrypoint(instance) => Some(instance),
            UseKind::BuiltinMirAssert | UseKind::AsyncPanic => None,
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
