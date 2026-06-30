use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DriverInput {
    pub lint: Option<LintConfig>,
    pub extract: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LintOptions {
    #[serde(default)]
    pub allow_async_panic: bool,

    #[serde(default)]
    pub allow_assert: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LintConfig {
    #[serde(default)]
    pub options: LintOptions,

    /// If any element in the trace is from this whitelist, it will not emit a lint.
    #[serde(default)]
    pub whitelist: Vec<LintRule>,

    // If the leaf element is from this blacklist, and the whitelist does not apply to the trace,
    // a `cga::blacklisted` lint will be emitted.
    #[serde(default)]
    pub blacklist: Vec<LintRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LintRule {
    /// Apply rule to the simplified path of the instance, with the string as a regex.
    Path(String),

    /// Apply rule to the canonical path of the instance, with the string as a regex.
    Canonical(String),

    /// Apply rule to the crate of the instance, as exact string.
    Crate(String),
}
