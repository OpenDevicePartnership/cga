fn test() {
    let _ = std::process::Command::new("evil");
    //~^ panics
    //~^^ panics
}

fn main() {
    test();
}
