trait Test {
    fn test(&self) {
        panic!("Default impl")
        //~^ panics
    }
}

struct StrictAdd;
struct StrictSub;
struct DefaultImpl;

impl Test for StrictAdd {
    fn test(&self) {
        let _ = 2u64.strict_add(500);
        //~^ panics
    }
}

impl Test for StrictSub {
    fn test(&self) {
        let _ = 2u64.strict_sub(500);
        //~^ panics
    }
}

impl Test for DefaultImpl {}

fn main() {
    // Even though only StrictAdd is instantiated here,
    // the dyn object cast makes it unclear which is being called.
    //
    // Hence both `StrictAdd` and `StrictSub` emit a panic.
    let x = StrictAdd;

    let r: &dyn Test = &x;
    r.test();
}
