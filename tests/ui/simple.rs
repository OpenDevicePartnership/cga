fn test() {
    let _ = 2u64.strict_add(500);
    //~^ panics
}

fn main() {
    // Do not emit three diagnostics, but instead note that the site is called 3 different ways.
    test();
    test();
    test();
}
