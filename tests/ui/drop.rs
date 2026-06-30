struct Foo;

impl Drop for Foo {
    fn drop(&mut self) {
        panic!("Oh no!");
        //~^ panics
    }
}

fn main() {
    let foo = Foo;
    core::mem::drop(foo);
}
