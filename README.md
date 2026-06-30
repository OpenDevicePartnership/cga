# Call Graph Analyzer (CGA)
Rust tool to perform call graph analysis on the monomorphized MIR.

Explicit goal is to be able to recursively check for an application to panic.
Only works on crates with 'entrypoints', so binary crates with a `fn main` and library crates containing symbols with exported names. (`no_mangle` and `export_name`)

As a rustc-driver, it uses a specific nightly version to be built, installed and run. Your project does not gain the dependency on nightly though, unless you wish to finely grained configure (i.e. silence) diagnostic lint warnings emitted by this tool. In that case it requires nightly rust to register as a rustc tool (but you can conditionally enable this with `cfg_attr`).

This tool also supports a separate configuration file in which specific functions/paths/modules/crates can be whitelisted or blacklisted. This feature does not require nightly for your project.

⚠️ **Warning**: CGA is inherently a bit fragile as it is not maintained in lockstep with rustc (like clippy is for example), and it is not exhaustively tested on various crates (like rustc and clippy are). Please create an issue if your project can not be analysed using this tool, or if the callgraph exported is incorrect.

## Attribution
Based on a blog by [Jyn](https://jyn.dev/rustc-driver/) and completely forked parts of [rustc](https://github.com/rust-lang/rust) and [ferrocene](https://github.com/ferrocene/ferrocene/tree/main/compiler).

## Alternatives
CGA extracts the callgraph directly from MIR as a rustc-driver. That approach can work well to detect panics, but it is not the only way to achieve it. All approaches come with varying up- and down-sides.

Other ways to extract callgraphs or detect panics include:
* [rustig](https://github.com/Technolution/rustig) (ELF debug info analysis)
* [findpanics](https://github.com/philipc/findpanics) (ELF debug info analysis)
* [cargo-callgraph](https://github.com/robinmoussu/cargo-callgraph) (Rust HIR analyzer)
* [cargo-callgraph](https://crates.io/crates/cargo-callgraph) (Rust-analyzer based exporter)
* [charon](https://github.com/AeneasVerif/charon) (Rust MIR exporter)
* [no-panic](https://crates.io/crates/no-panic) (Linker-based panic detection)
* [dont_panic](https://github.com/Kixunil/dont_panic) (Linker-based panic detection)

Please let us know if a project is missing from this list.

## Examples

### Panic
```
error: call to method `core::num::<impl u64>::strict_add` which can panic
  --> tests/ui/dyn_trait.rs:18:17
   |
LL |         let _ = 2u64.strict_add(500);
   |                 ^^^^^^^^^^^^^^^^^^^^
   |
   = note: 3 different panics from this location
   = note: backtrace (1 of 2):
           0: std::rt::panic_fmt (called)
           1: core::num::imp::overflow_panic::add (called)
           2: core::num::<impl u64>::strict_add (called) <--
           3: <dyn_trait::StrictAdd as dyn_trait::Test>::test (possible vtable resolution)
           4: <dyn dyn_trait::Test as dyn_trait::Test>::test - virtual#3 (called)
           5: dyn_trait::main (entrypoint)
```

### Blacklist
```
error: call to drop of `std::process::Command` which can call blacklisted functions
 --> src/main.rs:7:47
  |
7 |     let _ = std::process::Command::new("evil");
  |                                               ^
  |
  = note: 105 different blacklisted functions from this location
  = note: backtrace (1 of 42169425):
          0: std::sys::fs::unix::debug_assert_fd_is_open (called)
          1: <std::os::fd::OwnedFd as std::ops::Drop>::drop (called)
          2: drop of `std::os::fd::OwnedFd` (called)
          3: drop of `std::sys::fd::unix::FileDesc` (called)
          4: drop of `std::sys::process::unix::common::Stdio` (called)
          5: drop of `std::option::Option<std::sys::process::unix::common::Stdio>` (called)
          6: drop of `std::sys::process::unix::common::Command` (called)
          7: drop of `std::process::Command` (called) <--
          8: fail_blacklist::test (called)
          9: fail_blacklist::main (entrypoint)
```

## How to use
To install CGA you must check out this repository and run:
```bash
cargo install --path .
```

The tool then works like a cargo subcommand, which you can inspect using
```
$ cargo cga -h
Usage: cga <COMMAND>

Commands:
  check    Checks a package to catch whether functions are used that are not allowed, such as panics
  extract  Extracts the callgraph for a package and all their dependencies
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### Checking
```
$ cargo cga check -h
Checks a package to catch whether functions are used that are not allowed, such as panics

Usage: cga check [OPTIONS]

Options:
      --allow-async-panic          Configure the cga::panics lint to allow implicit panics from async generation
      --allow-assert               Configure the cga::panics lint to allow implicit asserts
      --config-path <CONFIG_PATH>  Path to a TOML file containing rules
  -h, --help
```

When checking options can also be passed:
```
cargo cga check --config-path ./cga.toml --allow-assert --allow-async-panic
```

With the following example configuration file:
```toml
whitelist = [
    { crate = "core" },
    { canonical = "^std::ptr::drop_glue::<totallysafe" },
    { path = "^totallysafe::Generic::test$" },
]

blacklist = [
    { path = "^std::process::" },
]

[options]
allow_async_panic = true
allow_assert = false
```

### Extraction
A callgraph can be exported using:
```
cargo cga extract -Tdot -o callgraph.dot
```

And a graphical representation can be generated using GraphViz (only for small graphs though):
```
dot -x -Tsvg callgraph.dot -o callgraph.svg
```

The format of these extracted callgraphs are still subject to change.

## Development
To run tests on this repository:
```
cargo uitest
```

To regenerate test artifacts:
```
cargo uibless
```

To only regenerate for a single test:
```
TESTNAME=simple cargo uibless
```

Diagnostic information can be gathered by setting env `RUSTC_LOG=cga_analysis=info`.

## Notes

### Optimization levels
CGA operates on MIR in the last pass before codegen is actually run (after analysis with monomorphized, optimized MIR).
Hence the optimization used by `rustc` directly impacts what optimized MIR is generated, which in turn impacts the extracted callgraph.

Generally it is recommended to run using the same compiler flags as used when building your binary, to ensure (as best as possible) that the callgraph corresponds to the one that ends up in your binary.

However the MIR optimization is not the only optimization that is relevant to your binary. The LLVM codegen pass, as well as the linker, play an important role in this. This becomes very relevant for `opt-level=z`, when the MIR inliner does not behave as aggressively as `opt-level=2`. Instead inlining is left mostly to LLVM codegen.

As a consequence of this when using `opt-level=z`, the checks for the compiler option `debug_assertions` and flag `ub_checks` are not eliminated from the MIR (even though they could be `false` and thus be deleted). This results in a lot of instances of `assert` and a call to `panic` ending up in your callgraph, even though they are not applicable in the final binary. Ideally these would not be present in the callgraph.

For now it is recommended to run CGA with a profile specific to it using `opt-level=2`, and to not use `opt-level=z` unless you do not mind a lot of false-positives. The downside of this is that a lot more functions will be inlined, making the callgraphs potentially less usable.

At a later date a custom optimization pass might be added to CGA to eliminate these specific checks leveraging the compilers `const_eval` pass.
