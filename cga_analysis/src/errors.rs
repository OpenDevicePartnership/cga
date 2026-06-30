use rustc_macros::Diagnostic;

#[derive(Diagnostic)]
#[diag("using `fn main` requires the standard library")]
#[help(
    "use `#![no_main]` to bypass the Rust generated entrypoint and declare a platform specific entrypoint yourself, usually with `#[no_mangle]`"
)]
pub(crate) struct StartNotFound;
