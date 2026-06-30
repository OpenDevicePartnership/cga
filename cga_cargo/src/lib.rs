use clap::Parser;

use crate::{args::Opts, command::exec};

mod args;
mod callgraph;
mod command;

pub fn main() -> Result<(), i32> {
    tracing_subscriber::fmt().pretty().init();

    let args: Vec<_> = std::env::args().collect();

    let args = if let Some(second) = args.get(1)
        && second == "cga"
    {
        // Pop the first arg as we invoke from 'cargo cga'.
        &args[1..]
    } else {
        &args[0..]
    };

    let opts: Opts = Opts::parse_from(args);

    let status = exec(opts);
    match status.code() {
        Some(0) => Ok(()),
        Some(other) => Err(other),
        None => Err(-1),
    }
}
