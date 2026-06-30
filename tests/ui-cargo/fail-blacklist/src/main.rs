#![feature(register_tool)]
#![register_tool(cga)]
#![forbid(cga::blacklisted)]
#![allow(unused)]

fn test() {
    let _ = std::process::Command::new("evil");
}

fn main() {
    test();
}
