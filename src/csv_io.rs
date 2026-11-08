//! Streaming CSV input (transactions) and output (accounts).
//!
//! The reader yields one [`Transaction`] per row and never holds more than
//! the current row in memory. A malformed row produces an `Err` for that row
//! only, and iteration carries on with the next one. Whether to skip or abort
//! is the caller's decision.

use std::io;

use csv::{ReaderBuilder, StringRecord, Trim};
use rust_decimal::Decimal;
use serde::Deserialize;
use thiserror::Error;

use crate::domain::{Account, Amount, AmountError, ClientId, Transaction, TransactionError, TxId};

/// An error reading a single row (or the underlying input).
#[derive(Debug, Error)]
pub enum ReadError {
    /// Malformed CSV, I/O failure, or a field that couldn't be parsed
    /// (unknown type, client not a u16, ...). `csv::Error` carries the
    /// position.
    #[error(transparent)]
    Csv(#[from] csv::Error),
    /// The row parsed, but doesn't describe a valid transaction.
    #[error("line {line}: {kind}")]
    InvalidRecord { line: u64, kind: RecordError },
}

/// Why a parsed row is not a valid transaction.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RecordError {
    #[error("deposit/withdrawal is missing an amount")]
    MissingAmount,
    #[error(transparent)]
    Amount(#[from] AmountError),
    #[error(transparent)]
    Transaction(#[from] TransactionError),
}

/// Transaction types as they appear in the `type` column (lowercase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawKind {
    Deposit,
    Withdrawal,
    Dispute,
    Resolve,
    Chargeback,
}

/// One input row exactly as it appears in the file, before validation.
#[derive(Debug, Deserialize)]
struct RawRecord {
    #[serde(rename = "type")]
    kind: RawKind,
    client: ClientId,
    tx: TxId,
    amount: Option<Decimal>,
}

impl TryFrom<RawRecord> for Transaction {
    type Error = RecordError;

    /// Deposits and withdrawals need a valid, positive amount. For disputes,
    /// resolves, and chargebacks, any amount present is ignored: they always
    /// act on the referenced deposit's amount.
    fn try_from(raw: RawRecord) -> Result<Self, Self::Error> {
        let RawRecord {
            kind,
            client,
            tx,
            amount,
        } = raw;
        let amount = || -> Result<Amount, RecordError> {
            let decimal = amount.ok_or(RecordError::MissingAmount)?;
            Ok(Amount::try_from(decimal)?)
        };
        Ok(match kind {
            RawKind::Deposit => Transaction::deposit(client, tx, amount()?)?,
            RawKind::Withdrawal => Transaction::withdrawal(client, tx, amount()?)?,
            RawKind::Dispute => Transaction::Dispute { client, tx },
            RawKind::Resolve => Transaction::Resolve { client, tx },
            RawKind::Chargeback => Transaction::Chargeback { client, tx },
        })
    }
}

/// Streams [`Transaction`]s from CSV input with a header row.
///
/// Whitespace around fields is trimmed, and rows may leave out the trailing
/// `amount` column. A single row buffer is reused, so memory use stays the
/// same however large the input is.
pub struct TransactionReader<R> {
    inner: csv::Reader<R>,
    headers: StringRecord,
    record: StringRecord,
    done: bool,
}

impl<R: io::Read> TransactionReader<R> {
    /// Reads the header row. Fails if the header can't be read.
    pub fn new(input: R) -> Result<Self, ReadError> {
        let mut inner = ReaderBuilder::new()
            .trim(Trim::All)
            .flexible(true)
            .from_reader(input);
        let headers = inner.headers()?.clone();
        Ok(TransactionReader {
            inner,
            headers,
            record: StringRecord::new(),
            done: false,
        })
    }

    fn parse_current(&self) -> Result<Transaction, ReadError> {
        let raw: RawRecord = self.record.deserialize(Some(&self.headers))?;
        Transaction::try_from(raw).map_err(|kind| ReadError::InvalidRecord {
            line: self.record.position().map_or(0, |p| p.line()),
            kind,
        })
    }
}

impl<R: io::Read> Iterator for TransactionReader<R> {
    type Item = Result<Transaction, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.inner.read_record(&mut self.record) {
            Ok(true) => Some(self.parse_current()),
            Ok(false) => {
                self.done = true;
                None
            }
            Err(err) => {
                // An I/O error means the input itself is broken. Stop after
                // reporting it, so a caller that skips bad rows can't loop
                // forever on the same failure.
                if err.is_io_error() {
                    self.done = true;
                }
                Some(Err(err.into()))
            }
        }
    }
}

/// Writes accounts as CSV (`client,available,held,total,locked`), with every
/// amount shown to exactly four decimal places.
///
/// Rows are written in the order given. Pass `io::stdout()` for the CLI, or a
/// `Vec<u8>` in tests.
pub fn write_accounts<'a, W: io::Write>(
    output: W,
    accounts: impl IntoIterator<Item = &'a Account>,
) -> Result<(), csv::Error> {
    let mut writer = csv::Writer::from_writer(output);
    writer.write_record(["client", "available", "held", "total", "locked"])?;
    for account in accounts {
        writer.write_record([
            account.client().to_string(),
            account.available().to_string(),
            account.held().to_string(),
            account.total().to_string(),
            account.is_locked().to_string(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amt(s: &str) -> Amount {
        s.parse().unwrap()
    }

    fn read_all(input: &str) -> Vec<Result<Transaction, ReadError>> {
        TransactionReader::new(input.as_bytes()).unwrap().collect()
    }

    fn read_ok(input: &str) -> Vec<Transaction> {
        read_all(input)
            .into_iter()
            .map(|r| r.expect("row should parse"))
            .collect()
    }

    fn invalid_record(result: &Result<Transaction, ReadError>) -> (u64, RecordError) {
        match result {
            Err(ReadError::InvalidRecord { line, kind }) => (*line, kind.clone()),
            other => panic!("expected InvalidRecord, got {other:?}"),
        }
    }

    // ---- reader: happy paths -------------------------------------------

    #[test]
    fn reads_rows_with_spaces_after_commas() {
        let input = "\
type, client, tx, amount
deposit, 4, 21, 12.75
deposit, 8, 22, 3.5
deposit, 4, 23, 0.25
withdrawal, 4, 24, 6.0
withdrawal, 8, 25, 9.9
";
        assert_eq!(
            read_ok(input),
            vec![
                Transaction::deposit(4, 21, amt("12.75")).unwrap(),
                Transaction::deposit(8, 22, amt("3.5")).unwrap(),
                Transaction::deposit(4, 23, amt("0.25")).unwrap(),
                Transaction::withdrawal(4, 24, amt("6.0")).unwrap(),
                Transaction::withdrawal(8, 25, amt("9.9")).unwrap(),
            ]
        );
    }

    #[test]
    fn accepts_no_whitespace_and_lots_of_whitespace() {
        let input =
            "type,client,tx,amount\ndeposit,1,1,1.2345\n   withdrawal  ,\t1 ,  2 ,  0.5  \n";
        assert_eq!(
            read_ok(input),
            vec![
                Transaction::deposit(1, 1, amt("1.2345")).unwrap(),
                Transaction::withdrawal(1, 2, amt("0.5")).unwrap(),
            ]
        );
    }

    #[test]
    fn dispute_resolve_chargeback_with_empty_amount() {
        let input = "type,client,tx,amount\ndispute,1,1,\nresolve, 1, 1, \nchargeback,1,1,\n";
        assert_eq!(
            read_ok(input),
            vec![
                Transaction::Dispute { client: 1, tx: 1 },
                Transaction::Resolve { client: 1, tx: 1 },
                Transaction::Chargeback { client: 1, tx: 1 },
            ]
        );
    }

    #[test]
    fn dispute_resolve_chargeback_with_amount_column_omitted() {
        let input = "type,client,tx,amount\ndispute,1,1\nresolve,1,1\nchargeback,1,1\n";
        assert_eq!(
            read_ok(input),
            vec![
                Transaction::Dispute { client: 1, tx: 1 },
                Transaction::Resolve { client: 1, tx: 1 },
                Transaction::Chargeback { client: 1, tx: 1 },
            ]
        );
    }

    #[test]
    fn amount_on_dispute_rows_is_ignored() {
        let input = "type,client,tx,amount\ndispute,1,1,5.0\nresolve,1,1,1.0\nchargeback,1,1,9\n";
        assert_eq!(
            read_ok(input),
            vec![
                Transaction::Dispute { client: 1, tx: 1 },
                Transaction::Resolve { client: 1, tx: 1 },
                Transaction::Chargeback { client: 1, tx: 1 },
            ]
        );
    }

    #[test]
    fn accepts_max_ids() {
        let input = "type,client,tx,amount\ndeposit,65535,4294967295,1\n";
        assert_eq!(
            read_ok(input),
            vec![Transaction::deposit(u16::MAX, u32::MAX, amt("1")).unwrap()]
        );
    }

    #[test]
    fn header_only_yields_nothing() {
        assert!(read_all("type,client,tx,amount\n").is_empty());
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(read_all("").is_empty());
    }

    #[test]
    fn blank_lines_are_skipped() {
        let input = "type,client,tx,amount\n\ndeposit,1,1,1\n\n";
        assert_eq!(
            read_ok(input),
            vec![Transaction::deposit(1, 1, amt("1")).unwrap()]
        );
    }

    // ---- reader: invalid rows ------------------------------------------

    #[test]
    fn deposit_or_withdrawal_without_amount_is_rejected_with_line() {
        let input = "type,client,tx,amount\ndeposit,1,1,\nwithdrawal,1,2\n";
        let results = read_all(input);
        assert_eq!(invalid_record(&results[0]), (2, RecordError::MissingAmount));
        assert_eq!(invalid_record(&results[1]), (3, RecordError::MissingAmount));
    }

    #[test]
    fn non_positive_amounts_are_rejected() {
        let input = "type,client,tx,amount\ndeposit,1,1,0\nwithdrawal,1,2,-1.5\n";
        let results = read_all(input);
        assert_eq!(
            invalid_record(&results[0]),
            (
                2,
                RecordError::Transaction(TransactionError::NonPositiveAmount(Amount::ZERO))
            )
        );
        assert!(matches!(
            invalid_record(&results[1]),
            (
                3,
                RecordError::Transaction(TransactionError::NonPositiveAmount(_))
            )
        ));
    }

    #[test]
    fn more_than_four_decimal_places_is_rejected() {
        let input = "type,client,tx,amount\ndeposit,1,1,1.23456\n";
        assert_eq!(
            invalid_record(&read_all(input)[0]),
            (2, RecordError::Amount(AmountError::TooPrecise))
        );
    }

    #[test]
    fn unparseable_fields_are_csv_errors() {
        let input = "\
type,client,tx,amount
transfer,1,1,1
deposit,70000,2,1
deposit,1,-3,1
deposit,1,4,abc
Deposit,1,5,1
";
        let results = read_all(input);
        assert_eq!(results.len(), 5);
        for (i, r) in results.iter().enumerate() {
            assert!(matches!(r, Err(ReadError::Csv(_))), "row {i}: {r:?}");
        }
    }

    #[test]
    fn bad_rows_do_not_stop_iteration() {
        let input = "\
type,client,tx,amount
deposit,1,1,1
bogus,1,2,1
deposit,1,3,
deposit,1,4,4
";
        let results = read_all(input);
        assert_eq!(results.len(), 4);
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
        assert!(results[2].is_err());
        assert_eq!(
            *results[3].as_ref().unwrap(),
            Transaction::deposit(1, 4, amt("4")).unwrap()
        );
    }

    #[test]
    fn io_error_ends_iteration() {
        struct Failing;
        impl io::Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("disk on fire"))
            }
        }
        // Header read fails up front.
        assert!(TransactionReader::new(Failing).is_err());

        // Header succeeds, then the stream breaks: exactly one error, then None.
        let input = io::Read::chain("type,client,tx,amount\n".as_bytes(), Failing);
        let mut reader = TransactionReader::new(input).unwrap();
        assert!(matches!(reader.next(), Some(Err(ReadError::Csv(e))) if e.is_io_error()));
        assert!(reader.next().is_none());
    }

    // ---- writer ------------------------------------------------------------

    fn write_to_string(accounts: &[Account]) -> String {
        let mut buf = Vec::new();
        write_accounts(&mut buf, accounts).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn writes_header_only_for_no_accounts() {
        assert_eq!(write_to_string(&[]), "client,available,held,total,locked\n");
    }

    #[test]
    fn writes_accounts_with_four_decimal_places() {
        let mut a = Account::new(1);
        a.deposit(amt("1.5")).unwrap();

        let mut b = Account::new(2);
        b.deposit(amt("5")).unwrap();
        b.hold(amt("2.25")).unwrap();

        let mut c = Account::new(3);
        c.deposit(amt("10")).unwrap();
        c.withdraw(amt("10")).unwrap();
        c.hold(amt("10")).unwrap();
        c.chargeback(amt("10")).unwrap();

        assert_eq!(
            write_to_string(&[a, b, c]),
            "\
client,available,held,total,locked
1,1.5000,0.0000,1.5000,false
2,2.7500,2.2500,5.0000,false
3,-10.0000,0.0000,-10.0000,true
"
        );
    }

    #[test]
    fn writes_rows_in_given_order() {
        let accounts = [Account::new(9), Account::new(3)];
        let out = write_to_string(&accounts);
        let clients: Vec<&str> = out
            .lines()
            .skip(1)
            .map(|l| l.split(',').next().unwrap())
            .collect();
        assert_eq!(clients, ["9", "3"]);
    }
}
