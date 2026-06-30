fn main() {
    let rustc = std::env::var("RUSTC").expect("RUSTC env var must be set for build script");
    let output = std::process::Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .expect("failed to run rustc --print sysroot");
    let stdout = String::from_utf8(output.stdout).expect("rustc --print sysroot output was not UTF-8");
    let sysroot = stdout.trim_end();
    println!("cargo:rustc-link-arg=-Wl,-rpath={sysroot}/lib")
}
