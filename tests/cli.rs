//! End-to-end tests: run the compiled binary as the CLI is meant to be used
//! (`ledger <file>`) and compare stdout with expected CSV.
//!
//! Each directory under `tests/fixtures/` holds an `input.csv` and the
//! `expected.csv` the binary should print. Output is sorted by client and
//! always uses four decimal places, so an exact text comparison works.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn ledger(args: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ledger"))
        .args(args)
        .output()
        .expect("failed to run ledger binary")
}

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Run a fixture, assert stdout matches `expected.csv`, and return stderr.
fn check_fixture(name: &str) -> String {
    let dir = fixture_dir(name);
    let expected = fs::read_to_string(dir.join("expected.csv")).unwrap();
    let out = ledger(&[&dir.join("input.csv")]);
    assert!(
        out.status.success(),
        "{name}: exit {:?}, stderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), expected, "{name}");
    String::from_utf8(out.stderr).unwrap()
}

#[test]
fn basic_deposits_and_withdrawals() {
    let stderr = check_fixture("basic");
    // Two withdrawals fail for insufficient funds.
    assert_eq!(stderr.lines().count(), 2, "stderr:\n{stderr}");
}

#[test]
fn dispute_and_resolve() {
    check_fixture("dispute_resolve");
}

#[test]
fn chargeback_locks_account() {
    check_fixture("chargeback_locks");
}

#[test]
fn dispute_after_withdrawal_goes_negative() {
    check_fixture("dispute_after_withdrawal");
}

#[test]
fn duplicate_tx_ids_rejected() {
    let stderr = check_fixture("duplicates");
    assert!(stderr.contains("duplicate"), "stderr:\n{stderr}");
}

#[test]
fn malformed_rows_are_skipped_and_logged() {
    let stderr = check_fixture("malformed_rows");
    let skipped = stderr.lines().filter(|l| l.starts_with("skipped:")).count();
    let rejected = stderr
        .lines()
        .filter(|l| l.starts_with("rejected:"))
        .count();
    assert_eq!(skipped, 7, "stderr:\n{stderr}");
    assert_eq!(rejected, 2, "stderr:\n{stderr}");
}

#[test]
fn four_decimal_precision() {
    check_fixture("precision");
}

#[test]
fn every_fixture_has_a_test() {
    let covered = [
        "basic",
        "chargeback_locks",
        "dispute_after_withdrawal",
        "dispute_resolve",
        "duplicates",
        "malformed_rows",
        "precision",
    ];
    let mut found: Vec<String> =
        fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
    found.sort();
    assert_eq!(found, covered);
}

// ---- CLI errors --------------------------------------------------------

fn assert_usage_failure(out: &Output) {
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "nothing should go to stdout");
    assert!(!out.stderr.is_empty());
}

#[test]
fn no_arguments_prints_usage() {
    let out = ledger(&[]);
    assert_usage_failure(&out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
}

#[test]
fn too_many_arguments_prints_usage() {
    let input = fixture_dir("basic").join("input.csv");
    let out = ledger(&[&input, &input]);
    assert_usage_failure(&out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
}

#[test]
fn missing_file_is_an_error() {
    let out = ledger(&[Path::new("does/not/exist.csv")]);
    assert_usage_failure(&out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot open"));
}

#[test]
fn directory_instead_of_file_is_an_error() {
    let out = ledger(&[&fixture_dir("basic")]);
    assert_usage_failure(&out);
}
