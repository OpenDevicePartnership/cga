fn test() {
    // Performs overflow add, which can panic when overflowing.
    // This is then a MIR terminator assert, and not an explicit function call to panic.
    1 + 1;
    //~^ panics
}

fn main() {
    test();
}
