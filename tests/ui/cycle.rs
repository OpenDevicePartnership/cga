fn test(i: u32) {
    if i > 100 {
        return;
    }

    test(i.saturating_add(1));
}

pub fn main() {
    // Test if MIR reachability expansion does not infinite loop when recursing.
    test(0);

    let _ = 2u64.strict_add(500);
    //~^ panics
}
