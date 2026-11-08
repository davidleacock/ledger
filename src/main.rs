//! CLI entry point: `ledger <transactions.csv> > accounts.csv`
//!
//! Account CSV goes to stdout. Diagnostics (skipped rows, rejected
//! transactions, fatal errors) go to stderr.

use std::env;
use std::fs::File;
use std::io::{self, BufWriter};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let (Some(path), None) = (args.next(), args.next()) else {
        eprintln!("usage: ledger <transactions.csv>");
        return ExitCode::FAILURE;
    };

    let input = match File::open(&path) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("error: cannot open {}: {err}", path.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };

    // The csv reader and writer buffer internally. stderr is unbuffered by
    // default, so wrap it to keep per-row logging cheap on large inputs.
    let log = BufWriter::new(io::stderr().lock());
    match ledger::run(input, io::stdout().lock(), log) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
