//~v panics
async fn test() {
    // Do nothing.
}

//~v panics
async fn test_with_panic() {
    let x = 5u32;
    let x = x.saturating_add(10);
    let (x, _) = x.overflowing_add(5);
    let _ = x.strict_add(3);
    //~^ panics
}

fn main() {
    let waker = core::task::Waker::noop();
    let mut ctx = core::task::Context::from_waker(waker);

    let mut fut = test();

    core::pin::pin!(fut).poll(&mut ctx);

    let mut fut = test_with_panic();

    core::pin::pin!(fut).poll(&mut ctx);
}
