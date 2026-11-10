fn test() {
    let _ = std::process::Command::new("evil");
    //~^ panics
    //~^^ panics
}

fn main() {
    test();
}

// The standard library's process implementation depends on the target OS.
//@revisions: host_linux host_windows host_macos
//@[host_linux]only-target: linux
//@[host_windows]only-target: windows
//@[host_macos]only-target: darwin
