use clap::{Parser, ValueEnum};
use std::path::PathBuf;

use crate::args::cargo::CargoArgs;

pub(crate) mod cargo;

#[derive(Parser)]
pub struct Opts {
    #[clap(subcommand)]
    pub subcommand: Subcommand,
}

#[derive(Debug, ValueEnum, Default, Clone, Copy)]
#[clap(rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    Json,
    Dot,
}

#[derive(Debug, Parser)]
pub enum Subcommand {
    /// Checks a package to catch whether functions are used that are not allowed, such as panics.
    Check {
        /// Configure the cga::panics lint to allow implicit panics from async generation.
        #[clap(long)]
        allow_async_panic: bool,

        /// Configure the cga::panics lint to allow implicit asserts.
        #[clap(long)]
        allow_assert: bool,

        /// Path to a TOML file containing rules.
        #[clap(long)]
        config_path: Option<PathBuf>,

        #[clap(flatten)]
        cargo: CargoArgs,
    },
    /// Extracts the callgraph for a package and all their dependencies.
    Extract {
        /// Path file will be written to [default: ./callgraph.<ext>]
        #[clap(short, long)]
        output_path: Option<PathBuf>,

        /// Format of the output file
        #[clap(short = 'T', long, default_value = "json")]
        output_format: OutputFormat,

        #[clap(flatten)]
        cargo: CargoArgs,
    },

    /// Perform both checking and extracting at the same time for use in tests.
    ///
    /// Hidden from general help because this is meant for internal use only.
    #[clap(hide = true)]
    Test {
        #[clap(long)]
        config_path: Option<PathBuf>,

        #[clap(flatten)]
        cargo: CargoArgs,

        /// Path file will be written to
        #[clap(short, long)]
        output_path: PathBuf,
    },
}
