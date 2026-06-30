use lazy_regex::regex;
use rustc_hir::def_id::DefId;
use rustc_middle::ty::TyCtxt;

use crate::{Use, UseKind};

/// Determination if a [`Use`] is a kind of panic.
///
/// Derived using [`Use::panic_status`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PanicStatus {
    /// Use is not a call any panic impl.
    NoDirectPanic,
    /// Use is a call to a panic impl.
    PanicImpl,
    /// Use is a MIR assert terminator.
    ///
    /// An example would be the overflow check with a plain addition ex.: `1 + 1`
    ///
    /// These are translated to a panic in a later compiler pass.
    Asserts,
}

impl PanicStatus {
    /// This variant can not be considered to be a panic.
    pub fn none(&self) -> bool {
        matches!(self, PanicStatus::NoDirectPanic)
    }

    /// This variant is some form of a panic.
    pub fn any(&self) -> bool {
        !self.none()
    }
}

/// Check if the item is one of the multiple invocations in core/std that could be called panic.
///
/// Examples would be the panic! or assert! macros. However in the MIR callgraph we could see a call to
/// `core::panicking` or `core::rt::panic_fmt`, depending on the version of core.
///
/// This analysis needs to be conservative. If you don't have enough information to determine the status, assume it panics.
pub fn item_is_panic_impl(tcx: TyCtxt<'_>, def_id: DefId) -> PanicStatus {
    // A closure is panic free if the function it's defined in is panic free.
    let owner = tcx.typeck_root_def_id(def_id);

    // TODO replace with #[rustc_panic_entrypoint]
    let path = tcx.def_path_str(owner);
    let regex = regex!("^(core|std)::(panicking::|rt::panic_fmt)");
    if regex.is_match(&path) {
        return PanicStatus::PanicImpl;
    }

    PanicStatus::NoDirectPanic
}

impl Use<'_> {
    /// Determines whether the [`Use`] panics and how.
    ///
    /// Only checks this specific [`Use`], and does not expand the call tree below.
    pub fn panic_status(&self, tcx: TyCtxt<'_>) -> PanicStatus {
        match &self.kind {
            UseKind::BuiltinMirAssert => PanicStatus::Asserts,
            UseKind::AsyncPanic => PanicStatus::PanicImpl, // Will always panic.
            _ => {
                // Check if callee is an implementation which
                // colloquially would be called a 'panic'.
                if let Some(callee) = self.opt_def_id() {
                    item_is_panic_impl(tcx, callee)
                } else {
                    PanicStatus::NoDirectPanic
                }
            }
        }
    }
}
