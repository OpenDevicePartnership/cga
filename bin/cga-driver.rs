#![feature(rustc_private)]
extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_lint;
extern crate rustc_middle;
extern crate rustc_mir_transform;
extern crate rustc_monomorphize;
extern crate rustc_session;
extern crate rustc_span;

use std::process::ExitCode;
use std::{env, fs::File};

use rustc_driver::Callbacks;
use rustc_interface::interface;
use rustc_lint::LintStore;
use rustc_middle::ty::TyCtxt;
use rustc_session::EarlyDiagCtxt;
use rustc_session::config::ErrorOutputType;
use rustc_span::Symbol;

use cga_analysis::{BLACKLISTED, PANICS};

struct Cga;

fn post_mono_pass<'tcx>(
    tcx: TyCtxt<'tcx>,
    _: (),
) -> rustc_middle::queries::collect_and_partition_mono_items::ProvidedValue<'tcx> {
    let items = (rustc_interface::passes::DEFAULT_QUERY_PROVIDERS
        .queries
        .collect_and_partition_mono_items)(tcx, ());

    if let Ok(input) = std::env::var("CGA_INPUT") {
        let input: cga_data::cmd::DriverInput = serde_json::from_str(&input).expect("Invalid CGA_INPUT format");

        let roots = cga_analysis::monomorphize::collect_roots(
            tcx,
            cga_analysis::monomorphize::MonoItemCollectionStrategy::NoConstOrStatic,
        )
        .into_iter()
        .collect();

        let callgraph = cga_analysis::post_mono::generate_callgraph(tcx, roots);

        if let Some(config) = input.lint {
            cga_analysis::diagnostics::emit_lints(callgraph.clone(), &config, tcx);
        }

        if input.extract {
            let filename = tcx.output_filenames(()).with_extension("callgraph.json");

            let callgraph = callgraph.export(tcx);
            serde_json::to_writer_pretty(File::create(&filename).unwrap(), &callgraph).unwrap();

            eprintln!("cga-emit:{}", filename.display());

            // Emit that we generated an artifact (but currently cargo filters these out).
            // Not useful until https://github.com/rust-lang/cargo/issues/13672 is solved.
            // For now commented out as ui-test does not play nice with this.
            // tcx.sess
            //     .dcx()
            //     .emit_artifact_notification(&filename, "cga-callgraph");
        }
    }

    items
}

impl Callbacks for Cga {
    fn config(&mut self, config: &mut interface::Config) {
        config.track_state = Some(Box::new(|sess| {
            let mut env_depinfo = sess.env_depinfo.lock();

            env_depinfo.insert((
                Symbol::intern("RUSTC_LOG"),
                env::var("RUSTC_LOG").map(|log| Symbol::intern(&log)).ok(),
            ));
            env_depinfo.insert((
                Symbol::intern("CGA_INPUT"),
                env::var("CGA_INPUT").map(|input| Symbol::intern(&input)).ok(),
            ));
        }));

        config.register_lints = Some(Box::new(|_sess, store: &mut LintStore| {
            store.register_lints(&[PANICS, BLACKLISTED]);
        }));

        config.override_queries =
            Some(|_session, queries| queries.queries.collect_and_partition_mono_items = post_mono_pass);
    }

    fn after_analysis<'tcx>(
        &mut self,
        _compiler: &interface::Compiler,
        tcx: TyCtxt<'tcx>,
    ) -> rustc_driver::Compilation {
        let _ = tcx.collect_and_partition_mono_items(());
        rustc_driver::Compilation::Continue
    }
}

fn main() -> ExitCode {
    // rustc_driver::install_ice_hook("https://github.com/Wassasin/cga/issues/new", |_| ());
    let early_dcx = EarlyDiagCtxt::new(ErrorOutputType::default());

    rustc_driver::init_rustc_env_logger(&early_dcx);

    rustc_driver::catch_with_exit_code(move || {
        let args = rustc_driver::args::raw_args(&early_dcx);

        let ignore = args
            .windows(2)
            .find(|slice| {
                (slice[0] == "--crate-name" && slice[1] == "build_script_build")
                    || (slice[0] == "--crate-type" && slice[1] == "proc-macro")
            })
            .is_some();

        if ignore {
            rustc_driver::compiler_entrypoint(&args, &mut rustc_driver::TimePassesCallbacks::default());
        } else {
            rustc_driver::compiler_entrypoint(&args, &mut Cga)
        }
    })
}
