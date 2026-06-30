use cga_data::{
    self as data,
    callgraph::Callgraph,
    cmd::{DriverInput, LintConfig},
};
use lazy_regex::Regex;
use std::{
    env,
    fs::File,
    io::{BufRead, BufReader, BufWriter},
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    str::FromStr,
};

use crate::args::{Opts, OutputFormat, Subcommand, cargo::CargoArgs};

fn toolchain() -> String {
    let toolchain = env!("RUSTUP_TOOLCHAIN");
    let regex = Regex::new("^(nightly-[0-9]{4}-[0-9]{2}-[0-9]{2})-.+").unwrap();
    regex
        .captures(toolchain)
        .expect(&format!(
            "RUSTUP_TOOLCHAIN env not set with expected name, got {toolchain:?}"
        ))
        .get(1)
        .unwrap()
        .as_str()
        .to_owned()
}

fn prepare_cargo(input: &DriverInput, args: CargoArgs) -> Command {
    // Note: use rustup cargo alias instead of a toolchain-specific cargo binary.
    let mut cmd = Command::new("cargo");
    let driver = env::current_exe().unwrap().with_file_name("cga-driver");

    let build_std = if let Some(build_std) = &args.build_std {
        &format!("build-std={build_std}")
    } else {
        "build-std"
    };

    cmd
        // Require nightly **cargo** for "-Z build-std" (our rustc-driver does not care)
        .arg(&format!("+{}", toolchain()))
        // Force std to be freshly built so that we can get MIR.
        .args(["-Z", build_std])
        .arg("check")
        .env("RUSTC", driver)
        .env("CGA_INPUT", serde_json::to_string(&input).unwrap())
        // Disable incremental compilation such that we always get MIR.
        .env("CARGO_INCREMENTAL", "0")
        // Request to always encode mir, and speed up compilation.
        .env("RUSTFLAGS", "-Zalways-encode-mir -Zhint-mostly-unused");

    args.apply(&mut cmd);

    cmd
}

struct ExtractInstructions {
    output_path: PathBuf,
    output_format: OutputFormat,
}

pub fn exec(opts: Opts) -> ExitStatus {
    let mut input = data::cmd::DriverInput::default();

    let (mut cmd, extract_instr) = match opts.subcommand {
        Subcommand::Check {
            cargo,
            allow_assert,
            allow_async_panic,
            config_path,
        } => {
            let mut config = if let Some(config_path) = config_path {
                toml::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap()
            } else {
                LintConfig::default()
            };

            config.options.allow_async_panic |= allow_async_panic;
            config.options.allow_assert |= allow_assert;

            input.lint = Some(config);
            (prepare_cargo(&input, cargo), None)
        }
        Subcommand::Extract {
            output_path,
            output_format,
            cargo,
        } => {
            input.extract = true;

            let output_path = output_path.unwrap_or_else(|| {
                PathBuf::from_str(match output_format {
                    OutputFormat::Json => "./callgraph.json",
                    OutputFormat::Dot => "./callgraph.dot",
                })
                .unwrap()
            });

            (
                prepare_cargo(&input, cargo),
                Some(ExtractInstructions {
                    output_path,
                    output_format,
                }),
            )
        }
        Subcommand::Test {
            config_path,
            cargo,
            output_path,
        } => {
            let config = if let Some(config_path) = config_path {
                toml::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap()
            } else {
                LintConfig::default()
            };

            input.extract = true;
            input.lint = Some(config);

            (
                prepare_cargo(&input, cargo),
                Some(ExtractInstructions {
                    output_path,
                    output_format: OutputFormat::Json,
                }),
            )
        }
    };

    if let Some(ExtractInstructions {
        output_path,
        output_format,
    }) = extract_instr
    {
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn().unwrap();
        let reader = BufReader::new(child.stderr.as_mut().unwrap());

        let mut emissions = vec![];

        let lines = reader.lines();
        for line in lines {
            let line = line.unwrap();
            if let Some((_, suffix)) = line.split_once("cga-emit:") {
                emissions.push(suffix.to_owned());
            } else {
                // Pass the line through our parent process.
                eprintln!("{line}");
            }
        }

        let callgraph = Callgraph::merge(emissions.iter().map(|e| {
            serde_json::from_reader::<_, Callgraph>(BufReader::new(File::open(e).unwrap())).unwrap()
        }));

        let file = BufWriter::new(File::create(output_path).unwrap());

        match output_format {
            OutputFormat::Json => serde_json::to_writer_pretty(file, &callgraph).unwrap(),
            OutputFormat::Dot => crate::callgraph::emit_dot(&callgraph, file).unwrap(),
        };

        child.wait().unwrap()
    } else {
        cmd.status().unwrap()
    }
}
