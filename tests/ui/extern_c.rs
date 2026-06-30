#[unsafe(no_mangle)]
pub unsafe extern "C" fn also_entrypoint() {
    let _ = 2u64.strict_add(500);
    //~^ panics
}

#[unsafe(export_name = "rust_fn_that_panics")]
pub unsafe extern "C" fn call_me_outside() {
    let _ = 2u64.strict_add(500);
    //~^ panics
}

pub unsafe extern "C" fn uncallable_so_no_panic() {
    let _ = 2u64.strict_add(500);
}
