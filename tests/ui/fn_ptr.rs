fn test() {
    let _ = 2u64.strict_add(500);
    //~^ panics
}

fn main() {
    let _ptr = test as *const fn();

    let _ptr = u64::strict_add as *const fn(u64) -> u64;
    //~^ panics
}
