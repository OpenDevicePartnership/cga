#![feature(register_tool)]
#![register_tool(cga)]
#![forbid(cga::panics)]
#![allow(unused)]

fn test() {
    let _ = 2u64.strict_add(500);
}

async fn bar() {}

struct Foo;

impl Drop for Foo {
    fn drop(&mut self) {
        panic!("Oh no!");
    }
}

fn main() {
    test();

    let foo = Foo;
    core::mem::drop(foo);
    embassy_futures::block_on(bar());
}
