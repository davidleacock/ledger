//! A streaming toy payments engine.
//!
//! [`run`] connects everything: it streams transactions from any reader into a
//! [`ledger::Ledger`], logs skipped rows and rejected transactions, and writes
//! the final account states as CSV.

pub mod csv_io;
pub mod domain;
pub mod ledger;

use std::io::{Read, Write};

use thiserror::Error;

use crate::csv_io::{ReadError, TransactionReader, write_accounts};
use crate::ledger::Ledger;

/// A failure that stops processing. No account output is produced.
#[derive(Debug, Error)]
pub enum RunError {
    #[error("failed to read input: {0}")]
    Input(csv::Error),
    #[error("failed to write output: {0}")]
    Output(csv::Error),
}

/// Process every transaction in `input` and write the final accounts to
/// `output`.
///
/// - A malformed row or rejected transaction is logged to `log` and skipped.
/// - An unreadable header or an I/O failure on `input` is fatal. Nothing is
///   written to `output`, because partial balances would be misleading.
/// - Failures writing to `log` are ignored, so diagnostics can never stop
///   processing.
pub fn run(input: impl Read, output: impl Write, mut log: impl Write) -> Result<(), RunError> {
    let reader = TransactionReader::new(input).map_err(into_fatal)?;
    let mut ledger = Ledger::new();

    for row in reader {
        match row {
            Ok(transaction) => {
                if let Err(err) = ledger.apply(transaction) {
                    let _ = writeln!(log, "rejected: {err}");
                }
            }
            Err(ReadError::Csv(err)) if err.is_io_error() => return Err(RunError::Input(err)),
            Err(err) => {
                let _ = writeln!(log, "skipped: {err}");
            }
        }
    }
    let _ = log.flush();

    write_accounts(output, ledger.accounts()).map_err(RunError::Output)
}

fn into_fatal(err: ReadError) -> RunError {
    match err {
        ReadError::Csv(err) => RunError::Input(err),
        // `TransactionReader::new` only reads the header, which can't produce
        // a record-level error.
        ReadError::InvalidRecord { .. } => unreachable!("header read cannot yield InvalidRecord"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn run_str(input: &str) -> (String, String) {
        let (mut out, mut log) = (Vec::new(), Vec::new());
        run(input.as_bytes(), &mut out, &mut log).unwrap();
        (
            String::from_utf8(out).unwrap(),
            String::from_utf8(log).unwrap(),
        )
    }

    #[test]
    fn processes_and_writes_sorted_accounts() {
        let (out, log) =
            run_str("type,client,tx,amount\ndeposit,2,1,3\ndeposit,1,2,1.5\nwithdrawal,2,3,1\n");
        assert_eq!(
            out,
            "client,available,held,total,locked\n\
             1,1.5000,0.0000,1.5000,false\n\
             2,2.0000,0.0000,2.0000,false\n"
        );
        assert_eq!(log, "");
    }

    #[test]
    fn logs_skipped_rows_and_rejections_then_continues() {
        let (out, log) = run_str(
            "type,client,tx,amount\n\
             deposit,1,1,5\n\
             bogus,1,2,1\n\
             withdrawal,1,3,10\n\
             dispute,1,99,\n\
             deposit,1,4,1\n",
        );
        assert_eq!(
            out,
            "client,available,held,total,locked\n1,6.0000,0.0000,6.0000,false\n"
        );
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3, "log was:\n{log}");
        assert!(lines[0].starts_with("skipped:"), "{}", lines[0]);
        assert!(lines[1].starts_with("rejected: tx 3"), "{}", lines[1]);
        assert!(lines[2].starts_with("rejected: tx 99"), "{}", lines[2]);
    }

    struct Failing;
    impl Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("disk on fire"))
        }
    }

    #[test]
    fn io_failure_mid_stream_is_fatal_and_writes_nothing() {
        let input = "type,client,tx,amount\ndeposit,1,1,5\n"
            .as_bytes()
            .chain(Failing);
        let mut out = Vec::new();
        let result = run(input, &mut out, io::sink());
        assert!(matches!(result, Err(RunError::Input(_))));
        assert!(out.is_empty());
    }

    #[test]
    fn unreadable_header_is_fatal() {
        let mut out = Vec::new();
        assert!(matches!(
            run(Failing, &mut out, io::sink()),
            Err(RunError::Input(_))
        ));
        assert!(out.is_empty());
    }

    #[test]
    fn output_failure_is_reported() {
        struct BrokenPipe;
        impl Write for BrokenPipe {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let result = run(
            "type,client,tx,amount\ndeposit,1,1,5\n".as_bytes(),
            BrokenPipe,
            io::sink(),
        );
        assert!(matches!(result, Err(RunError::Output(_))));
    }
}
