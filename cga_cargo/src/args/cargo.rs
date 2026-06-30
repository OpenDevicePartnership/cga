use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Parser)]
#[clap(next_help_heading = "Package Selection")]
pub struct PackageArgs {
    /// Package(s) to process
    #[clap(long, short = 'p')]
    pub package: Option<String>,

    /// Check all packages in the workspace
    #[clap(long)]
    pub workspace: bool,
}

#[derive(Debug, Parser)]
#[clap(next_help_heading = "Feature Selection")]
pub struct FeatureArgs {
    /// Space or comma separated list of features to activate
    #[clap(long, short = 'F')]
    pub features: Option<String>,

    /// Activate all available features
    #[clap(long)]
    pub all_features: bool,

    /// Do not activate the `default` feature
    #[clap(long)]
    pub no_default_features: bool,
}

#[derive(Debug, Parser)]
#[clap(next_help_heading = "Compilation Options")]
pub struct CompilationArgs {
    /// Number of parallel jobs, defaults to # of CPUs
    #[clap(long, short = 'j')]
    pub jobs: Option<u32>,

    /// Check artifacts in release mode, with optimizations
    #[clap(long, short)]
    pub release: bool,

    /// Check artifacts with the specified profile
    #[clap(long)]
    pub profile: Option<String>,

    /// Check for the target triple
    #[clap(long)]
    pub target: Option<String>,

    /// Directory for all generated artifacts
    #[clap(long, default_value = "./target/cga")]
    pub target_dir: PathBuf,
}

#[derive(Debug, Parser)]
#[clap(next_help_heading = "Manifest Options")]
pub struct ManifestArgs {
    /// Path to Cargo.toml
    #[clap(long, short)]
    pub manifest_path: Option<PathBuf>,

    /// Assert that `Cargo.lock` will remain unchanged
    #[clap(long)]
    pub locked: bool,

    /// Run without accessing the network
    #[clap(long)]
    pub offline: bool,

    /// Equivalent to specifying both --locked and --offline
    #[clap(long)]
    pub frozen: bool,
}

#[derive(Debug, ValueEnum, Default, Clone, Copy)]
#[clap(rename_all = "snake_case")]
pub enum Color {
    #[default]
    Auto,
    Always,
    Never,
}

impl core::fmt::Display for Color {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Color::Auto => "auto",
            Color::Always => "always",
            Color::Never => "never",
        })
    }
}

/// Set of Cargo arguments that CGA also supports (for some commands).
#[derive(Debug, Parser)]
#[clap(next_help_heading = "Cargo Options")]
pub struct CargoArgs {
    /// Do not print cargo log messages
    #[clap(long, short)]
    pub quiet: bool,

    /// Use verbose output
    #[clap(long, short)]
    pub verbose: bool,

    /// Coloring
    #[clap(long)]
    pub color: Option<Color>,

    /// Pass extra arguments to '-Z build-std' when cargo is invoked
    #[clap(long = "build-std")]
    pub build_std: Option<String>,

    #[clap(flatten)]
    pub package: PackageArgs,

    #[clap(flatten)]
    pub feature: FeatureArgs,

    #[clap(flatten)]
    pub compilation: CompilationArgs,

    #[clap(flatten)]
    pub manifest: ManifestArgs,
}

impl PackageArgs {
    pub fn apply(&self, command: &mut Command) {
        if let Some(package) = self.package.as_ref() {
            command.args(&["--package", package]);
        }

        if self.workspace {
            command.arg("--workspace");
        }
    }
}

impl FeatureArgs {
    pub fn apply(&self, command: &mut Command) {
        if let Some(features) = self.features.as_ref() {
            command.args(&["--features", features]);
        }

        if self.all_features {
            command.arg("--all-features");
        }

        if self.no_default_features {
            command.arg("--no-default-features");
        }
    }
}

impl CompilationArgs {
    pub fn apply(&self, command: &mut Command) {
        if let Some(jobs) = self.jobs.as_ref() {
            command.args(&["--jobs", &format!("{jobs}")]);
        }

        if self.release {
            command.arg("--release");
        }

        if let Some(profile) = self.profile.as_ref() {
            command.args(&["--profile", profile]);
        }

        if let Some(target) = self.target.as_ref() {
            command.args(&["--target", target]);
        }

        command.args(&["--target-dir", &format!("{}", self.target_dir.display())]);
    }
}

impl ManifestArgs {
    pub fn apply(&self, command: &mut Command) {
        if let Some(manifest_path) = self.manifest_path.as_ref() {
            command.args(&["--manifest-path", &format!("{}", manifest_path.display())]);
        }

        if self.locked {
            command.arg("--locked");
        }

        if self.offline {
            command.arg("--offline");
        }

        if self.frozen {
            command.arg("--frozen");
        }
    }
}

impl CargoArgs {
    pub fn apply(&self, command: &mut Command) {
        if self.quiet {
            command.arg("--quiet");
        }

        if self.verbose {
            command.arg("--verbose");
        }

        if let Some(color) = self.color {
            command.arg(format!("--color={color}"));
        }

        self.package.apply(command);
        self.feature.apply(command);
        self.compilation.apply(command);
        self.manifest.apply(command);
    }
}
