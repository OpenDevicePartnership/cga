#![feature(register_tool)]
#![register_tool(cga)]
#![warn(cga::panics)]

fn test() {
    assert!(false);
    //~^ panics
}

fn main() {
    test();
}
