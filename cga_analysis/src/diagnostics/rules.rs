use cga_data::cmd::LintRule;
use lazy_regex::regex::RegexSet;
use rustc_data_structures::fx::FxHashSet;
use rustc_middle::ty::TyCtxt;
use rustc_span::{Span, Symbol};

use crate::{Use, canonical_path, simplified_path};

pub struct LintRuleSet {
    path: RegexSet,
    canonical: RegexSet,
    crate_: FxHashSet<String>,
}

impl LintRuleSet {
    pub fn new<'a>(rules: &Vec<LintRule>) -> Result<Self, lazy_regex::regex::Error> {
        let mut path_regexes = vec![];
        let mut canonical_regexes = vec![];
        let mut crates = FxHashSet::default();

        for rule in rules.iter() {
            match rule {
                LintRule::Path(expr) => path_regexes.push(expr),
                LintRule::Canonical(expr) => canonical_regexes.push(expr),
                LintRule::Crate(str) => {
                    crates.insert(str.clone());
                }
            }
        }

        Ok(Self {
            path: RegexSet::new(path_regexes)?,
            canonical: RegexSet::new(canonical_regexes)?,
            crate_: crates,
        })
    }

    /// Use is covered by any rule.
    pub fn use_applies<'tcx>(&self, use_: &Use<'tcx>, tcx: TyCtxt<'tcx>) -> bool {
        // Compute path-based rule.
        if let Some(instance) = use_.opt_instance() {
            if self.path.is_match(&simplified_path(instance)) {
                return true;
            }

            if self.canonical.is_match(&canonical_path(instance)) {
                return true;
            }
        }

        // Compute crate-based rule.
        if self
            .crate_
            .contains(span_to_cratename(&use_.span, tcx).as_str())
        {
            return true;
        }

        // No rule applied.
        false
    }
}

/// Lookup the symbol of the crate from which the span originates.
fn span_to_cratename(span: &Span, tcx: TyCtxt) -> Symbol {
    tcx.crate_name(tcx.sess.source_map().lookup_char_pos(span.lo()).file.cnum)
}
