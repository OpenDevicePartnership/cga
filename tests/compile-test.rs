#![feature(rustc_private)]
#![warn(rust_2018_idioms, unused_lifetimes)]
#![allow(unused_extern_crates)]

use ui_test::custom_flags::edition::Edition;
use ui_test::dependencies::DependencyBuilder;
use ui_test::spanned::Spanned;
use ui_test::status_emitter::StatusEmitter;
use ui_test::{Args, CommandBuilder, Config, Match, error_on_output_conflict};

use std::env::{self, set_var, var_os};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use cga_data::cmd::{DriverInput, LintConfig};

struct TestContext {
    args: Args,
}

impl TestContext {
    fn new() -> Self {
        let mut args = Args::test().unwrap();
        args.bless |= var_os("RUSTC_BLESS").is_some_and(|v| v != "0");
        Self { args }
    }

    fn base_config(&self, test_dir: &str, mandatory_annotations: bool) -> Config {
        let target_dir = PathBuf::from(var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()));
        let mut config = Config {
            output_conflict_handling: error_on_output_conflict,
            // Pre-fill filters with TESTNAME; will be later extended with `self.args`.
            filter_files: env::var("TESTNAME")
                .map(|filters| {
                    filters
                        .split(',')
                        // Make sure that if TESTNAME is empty we produce the empty list here,
                        // not a list containing an empty string.
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            target: None,
            bless_command: Some("cargo uibless".into()),
            out_dir: target_dir.join("ui_test"),
            ..Config::rustc(Path::new("tests").join(test_dir))
        };

        let defaults = config.comment_defaults.base();
        defaults.set_custom("edition", Edition("2024".into()));
        defaults.set_custom(
            "dependencies",
            DependencyBuilder {
                program: {
                    let mut p = CommandBuilder::cargo();

                    // Override to check instead of build.
                    p.args = vec![
                        "check".into(),
                        "--color=never".into(),
                        "--quiet".into(),
                        "--jobs".into(),
                        "1".into(),
                    ];

                    p.envs.push((
                        "RUSTFLAGS".into(),
                        Some("-Zalways-encode-mir -Zhint-mostly-unused".into()),
                    ));
                    p
                },
                crate_manifest_path: Path::new("cga_test_deps").join("Cargo.toml"),
                build_std: Some("".into()),
                bless_lockfile: self.args.bless,
            },
        );
        defaults.exit_status = None.into();
        if mandatory_annotations {
            defaults.require_annotations = Some(Spanned::dummy(true)).into();
        } else {
            defaults.require_annotations = None.into();
        }
        defaults.diagnostic_code_prefix = Some(Spanned::dummy("cga::".into())).into();
        config.with_args(&self.args);

        config.filter(&get_rustc_sysroot().to_string_lossy(), "rustc-sysroot");
        config.filter("cga-emit:.*\n", "");

        config.program.args.extend(
            [
                "--emit=metadata",
                "-Aunused",
                "-Ainternal_features",
                "-Zui-testing",
                "-Zdeduplicate-diagnostics=no",
                "-Zalways-encode-mir",
                "-Zhint-mostly-unused",
                "-Dwarnings",
            ]
            .map(OsString::from),
        );

        let cga_input = serde_json::to_string(&DriverInput {
            lint: Some(LintConfig::default()),
            extract: true,
        })
        .unwrap();

        config.program.envs.extend(
            [
                // Prevent rustc from creating `rustc-ice-*` files the console output is enough.
                ("RUSTC_ICE".into(), Some("0".into())),
                ("CGA_INPUT".into(), Some(cga_input.into())),
            ]
            .into_iter(),
        );

        if let Some(host_libs) = option_env!("HOST_LIBS") {
            let deps_dir = Path::new(host_libs).join("deps");

            if deps_dir.exists() {
                let dep = format!("-Ldependency={}", deps_dir.display());
                config.program.args.push(dep.into());
            } else {
                // If `/deps` does not exist, assume Cargo v2 build-dir layout
                let build_dir = Path::new(host_libs).join("build");
                let dependencies = discover_out_dirs(&build_dir);

                for dep in dependencies {
                    let dep = format!("-Ldependency={}", dep.display());
                    config.program.args.push(dep.into());
                }
            }
        }

        config.program.program = PathBuf::from(env!("CARGO_BIN_EXE_cga-driver"));

        config
    }
}

fn run_ui(cx: &TestContext) {
    let mut config = cx.base_config("ui", true);
    config.program.envs.push(("CGA_CONF_DIR".into(), Some("tests".into())));

    ui_test::run_tests_generic(
        vec![config],
        ui_test::default_file_filter,
        ui_test::default_per_file_config,
        Box::<dyn StatusEmitter>::from(cx.args.format),
    )
    .unwrap();
}

fn run_ui_cargo(cx: &TestContext) {
    let mut config = cx.base_config("ui-cargo", false);
    config.program.input_file_flag = CommandBuilder::cargo().input_file_flag;
    config.program.out_dir_flag = CommandBuilder::cargo().out_dir_flag;
    config.program.args = vec![
        "cga".into(),
        "test".into(),
        "--quiet".into(),
        "--color=never".into(),
        "--locked".into(),
    ];
    config.program.envs.extend([
        ("RUSTFLAGS".into(), Some("-Dwarnings".into())),
        ("CARGO_INCREMENTAL".into(), Some("0".into())),
    ]);
    // We need to do this while we still have a rustc in the `program` field.
    config.fill_host_and_target().unwrap();
    config
        .program
        .program
        .set_file_name(if cfg!(windows) { "cargo-cga.exe" } else { "cargo-cga" });
    config.comment_defaults.base().custom.clear();

    config
        .comment_defaults
        .base()
        .normalize_stderr
        .push((Match::from(env::current_dir().unwrap().as_path()), b"$DIR".into()));

    ui_test::run_tests_generic(
        vec![config],
        |path, config| {
            path.ends_with("Cargo.toml")
                .then(|| ui_test::default_any_file_filter(path, config))
        },
        |config, file_contents| {
            let mut path = file_contents.span().file;
            path.set_file_name("callgraph.json");
            config
                .program
                .args
                .extend_from_slice(&["--output-path".into(), (&path).into()]);

            path.set_file_name("cga.toml");
            if std::fs::exists(&path).unwrap() {
                config
                    .program
                    .args
                    .extend_from_slice(&["--config-path".into(), (&path).into()]);
            }

            path.set_file_name("target-triple");
            if std::fs::exists(&path).unwrap() {
                config.target = Some(std::fs::read_to_string(path).unwrap());
            }
        },
        Box::<dyn StatusEmitter>::from(cx.args.format),
    )
    .unwrap();
}

fn main() {
    unsafe {
        set_var("CGA_DISABLE_DOCS_LINKS", "true");
    }

    let cx = TestContext::new();

    run_ui(&cx);
    run_ui_cargo(&cx);
}

/// Gets all of the `out` dirs in a given Cargo `build-dir/<profile>/build` dir.
fn discover_out_dirs(dir: &Path) -> Vec<PathBuf> {
    if !dir.exists() {
        return Vec::new();
    }

    let read_dir = |path: &Path| path.read_dir().ok().into_iter().flatten().filter_map(Result::ok);
    dir.read_dir()
        .unwrap_or_else(|e| panic!("Couldn't read {}: {}", dir.display(), e))
        .map(|e| e.unwrap())
        .flat_map(|e| read_dir(&e.path()))
        .flat_map(|e| read_dir(&e.path()))
        .map(|e| e.path())
        .filter(|path| path.ends_with("out"))
        .collect::<Vec<_>>()
}

fn get_rustc_sysroot() -> PathBuf {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .expect("Failed to run rustc");

    let sysroot_str = String::from_utf8(output.stdout).expect("Invalid UTF-8 output from rustc");

    PathBuf::from(sysroot_str.trim())
}
