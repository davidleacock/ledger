//! The processing engine: applies transactions to client accounts.

use std::collections::{BTreeMap, HashMap, HashSet};

use thiserror::Error;

use crate::domain::{
    Account, AccountError, Action, Amount, ClientId, DisputeState, Transaction, TransactionError,
    TxId,
};

/// Why a transaction was rejected. A rejected transaction leaves every
/// account and dispute state unchanged (apart from using up its tx ID, see
/// [`Ledger::apply`]).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LedgerError {
    #[error("tx {0}: duplicate transaction id")]
    DuplicateTx(TxId),
    #[error("tx {0}: no such transaction")]
    UnknownTx(TxId),
    #[error("tx {0}: only successful deposits can be disputed")]
    NotDisputable(TxId),
    #[error("tx {tx}: belongs to client {owner}, not client {claimed}")]
    ClientMismatch {
        tx: TxId,
        owner: ClientId,
        claimed: ClientId,
    },
    #[error("tx {tx}: {source}")]
    InvalidTransition {
        tx: TxId,
        #[source]
        source: TransactionError,
    },
    #[error("tx {tx}, client {client}: {source}")]
    Account {
        tx: TxId,
        client: ClientId,
        #[source]
        source: AccountError,
    },
}

/// A successful deposit, remembered so it can be disputed later.
#[derive(Debug, Clone, Copy)]
struct StoredDeposit {
    client: ClientId,
    amount: Amount,
    state: DisputeState,
}

/// Holds every client account and the transaction history that disputes need.
///
/// Memory grows with the number of distinct clients (at most 65,536) plus the
/// number of deposit/withdrawal IDs seen. Input rows are not kept.
#[derive(Debug, Default)]
pub struct Ledger {
    /// A `BTreeMap` so output comes out sorted by client ID.
    accounts: BTreeMap<ClientId, Account>,
    /// Successful deposits: the only transactions that can be disputed.
    deposits: HashMap<TxId, StoredDeposit>,
    /// IDs used up by withdrawals and failed deposits. Kept only to detect
    /// duplicates and to tell "not disputable" apart from "unknown".
    non_disputable: HashSet<TxId>,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one transaction.
    ///
    /// Rules (see README):
    /// - Deposit and withdrawal IDs must be globally unique. A valid deposit
    ///   or withdrawal uses up its ID even if it then fails (insufficient
    ///   funds, locked account), so a replay of it is reported as a duplicate.
    /// - Deposits and withdrawals create the client's account if it doesn't
    ///   exist yet. A rejected duplicate does not.
    /// - Disputes, resolves, and chargebacks only act on successful deposits
    ///   belonging to the same client, and never create accounts.
    pub fn apply(&mut self, transaction: Transaction) -> Result<(), LedgerError> {
        match transaction {
            Transaction::Deposit { client, tx, amount } => self.deposit(client, tx, amount),
            Transaction::Withdrawal { client, tx, amount } => self.withdraw(client, tx, amount),
            Transaction::Dispute { client, tx } => self.dispute_step(client, tx, Action::Dispute),
            Transaction::Resolve { client, tx } => self.dispute_step(client, tx, Action::Resolve),
            Transaction::Chargeback { client, tx } => {
                self.dispute_step(client, tx, Action::Chargeback)
            }
        }
    }

    /// All accounts, sorted by client ID.
    pub fn accounts(&self) -> impl Iterator<Item = &Account> {
        self.accounts.values()
    }

    fn deposit(&mut self, client: ClientId, tx: TxId, amount: Amount) -> Result<(), LedgerError> {
        self.ensure_new_tx(tx)?;
        let result = self.account_mut(client).deposit(amount);
        match result {
            Ok(()) => {
                self.deposits.insert(
                    tx,
                    StoredDeposit {
                        client,
                        amount,
                        state: DisputeState::Undisputed,
                    },
                );
                Ok(())
            }
            Err(source) => {
                self.non_disputable.insert(tx);
                Err(LedgerError::Account { tx, client, source })
            }
        }
    }

    fn withdraw(&mut self, client: ClientId, tx: TxId, amount: Amount) -> Result<(), LedgerError> {
        self.ensure_new_tx(tx)?;
        self.non_disputable.insert(tx);
        self.account_mut(client)
            .withdraw(amount)
            .map_err(|source| LedgerError::Account { tx, client, source })
    }

    fn dispute_step(
        &mut self,
        client: ClientId,
        tx: TxId,
        step: Action,
    ) -> Result<(), LedgerError> {
        let Some(deposit) = self.deposits.get_mut(&tx) else {
            return Err(if self.non_disputable.contains(&tx) {
                LedgerError::NotDisputable(tx)
            } else {
                LedgerError::UnknownTx(tx)
            });
        };
        if deposit.client != client {
            return Err(LedgerError::ClientMismatch {
                tx,
                owner: deposit.client,
                claimed: client,
            });
        }

        let next_state = match step {
            Action::Dispute => deposit.state.dispute(),
            Action::Resolve => deposit.state.resolve(),
            Action::Chargeback => deposit.state.chargeback(),
        }
        .map_err(|source| LedgerError::InvalidTransition { tx, source })?;

        let account = self
            .accounts
            .get_mut(&client)
            .expect("a stored deposit always has an account");
        let amount = deposit.amount;
        match step {
            Action::Dispute => account.hold(amount),
            Action::Resolve => account.release(amount),
            Action::Chargeback => account.chargeback(amount),
        }
        .map_err(|source| LedgerError::Account { tx, client, source })?;

        // Only commit the new state once the account change has succeeded.
        deposit.state = next_state;
        Ok(())
    }

    fn ensure_new_tx(&self, tx: TxId) -> Result<(), LedgerError> {
        if self.deposits.contains_key(&tx) || self.non_disputable.contains(&tx) {
            Err(LedgerError::DuplicateTx(tx))
        } else {
            Ok(())
        }
    }

    fn account_mut(&mut self, client: ClientId) -> &mut Account {
        self.accounts
            .entry(client)
            .or_insert_with(|| Account::new(client))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amt(s: &str) -> Amount {
        s.parse().unwrap()
    }

    fn deposit(client: ClientId, tx: TxId, amount: &str) -> Transaction {
        Transaction::deposit(client, tx, amt(amount)).unwrap()
    }

    fn withdrawal(client: ClientId, tx: TxId, amount: &str) -> Transaction {
        Transaction::withdrawal(client, tx, amt(amount)).unwrap()
    }

    fn dispute(client: ClientId, tx: TxId) -> Transaction {
        Transaction::Dispute { client, tx }
    }

    fn resolve(client: ClientId, tx: TxId) -> Transaction {
        Transaction::Resolve { client, tx }
    }

    fn chargeback(client: ClientId, tx: TxId) -> Transaction {
        Transaction::Chargeback { client, tx }
    }

    /// Apply transactions that are all expected to succeed.
    fn ledger_with(txs: &[Transaction]) -> Ledger {
        let mut ledger = Ledger::new();
        for &t in txs {
            ledger
                .apply(t)
                .unwrap_or_else(|e| panic!("{t:?} failed: {e}"));
        }
        ledger
    }

    fn account(ledger: &Ledger, client: ClientId) -> &Account {
        ledger
            .accounts()
            .find(|a| a.client() == client)
            .unwrap_or_else(|| panic!("no account for client {client}"))
    }

    fn assert_account(
        ledger: &Ledger,
        client: ClientId,
        available: &str,
        held: &str,
        total: &str,
        locked: bool,
    ) {
        let a = account(ledger, client);
        assert_eq!(a.available(), amt(available), "client {client} available");
        assert_eq!(a.held(), amt(held), "client {client} held");
        assert_eq!(a.total(), amt(total), "client {client} total");
        assert_eq!(a.is_locked(), locked, "client {client} locked");
    }

    // ---- deposits & withdrawals --------------------------------------------

    #[test]
    fn spec_example() {
        let mut ledger = ledger_with(&[
            deposit(1, 1, "1.0"),
            deposit(2, 2, "2.0"),
            deposit(1, 3, "2.0"),
            withdrawal(1, 4, "1.5"),
        ]);
        assert!(matches!(
            ledger.apply(withdrawal(2, 5, "3.0")),
            Err(LedgerError::Account {
                tx: 5,
                client: 2,
                source: AccountError::InsufficientFunds
            })
        ));
        assert_account(&ledger, 1, "1.5", "0", "1.5", false);
        assert_account(&ledger, 2, "2", "0", "2", false);
    }

    #[test]
    fn accounts_are_sorted_by_client() {
        let ledger = ledger_with(&[deposit(9, 1, "1"), deposit(3, 2, "1"), deposit(500, 3, "1")]);
        let clients: Vec<_> = ledger.accounts().map(Account::client).collect();
        assert_eq!(clients, [3, 9, 500]);
    }

    #[test]
    fn first_event_withdrawal_is_rejected_but_creates_empty_account() {
        let mut ledger = Ledger::new();
        assert!(matches!(
            ledger.apply(withdrawal(7, 1, "1")),
            Err(LedgerError::Account {
                source: AccountError::InsufficientFunds,
                ..
            })
        ));
        assert_account(&ledger, 7, "0", "0", "0", false);
    }

    // ---- duplicate tx ids ------------------------------------------------

    #[test]
    fn duplicate_ids_are_rejected_across_deposits_and_withdrawals() {
        let mut ledger = ledger_with(&[deposit(1, 1, "10"), withdrawal(1, 2, "1")]);
        for dup in [
            deposit(1, 1, "5"),    // deposit after deposit
            withdrawal(1, 1, "5"), // withdrawal after deposit
            deposit(1, 2, "5"),    // deposit after withdrawal
            withdrawal(1, 2, "5"), // withdrawal after withdrawal
        ] {
            assert_eq!(ledger.apply(dup), Err(LedgerError::DuplicateTx(dup.tx())));
        }
        assert_account(&ledger, 1, "9", "0", "9", false);
    }

    #[test]
    fn duplicate_from_another_client_is_rejected_and_creates_no_account() {
        let mut ledger = ledger_with(&[deposit(1, 1, "10")]);
        assert_eq!(
            ledger.apply(deposit(2, 1, "5")),
            Err(LedgerError::DuplicateTx(1))
        );
        assert_eq!(ledger.accounts().count(), 1);
    }

    #[test]
    fn failed_withdrawal_still_uses_up_its_id() {
        let mut ledger = Ledger::new();
        assert!(ledger.apply(withdrawal(1, 1, "5")).is_err());
        assert_eq!(
            ledger.apply(deposit(1, 1, "5")),
            Err(LedgerError::DuplicateTx(1))
        );
    }

    #[test]
    fn failed_deposit_still_uses_up_its_id_and_is_not_disputable() {
        let mut ledger = ledger_with(&[
            deposit(1, 1, "5"),
            dispute(1, 1),
            chargeback(1, 1), // account now locked
        ]);
        assert!(matches!(
            ledger.apply(deposit(1, 2, "3")),
            Err(LedgerError::Account {
                source: AccountError::Locked,
                ..
            })
        ));
        assert_eq!(
            ledger.apply(deposit(1, 2, "3")),
            Err(LedgerError::DuplicateTx(2))
        );
        assert_eq!(
            ledger.apply(dispute(1, 2)),
            Err(LedgerError::NotDisputable(2))
        );
    }

    // ---- disputes ----------------------------------------------------------

    #[test]
    fn dispute_holds_funds() {
        let ledger = ledger_with(&[deposit(1, 1, "10"), deposit(1, 2, "5"), dispute(1, 1)]);
        assert_account(&ledger, 1, "5", "10", "15", false);
    }

    #[test]
    fn dispute_then_resolve_releases_funds() {
        let ledger = ledger_with(&[deposit(1, 1, "10"), dispute(1, 1), resolve(1, 1)]);
        assert_account(&ledger, 1, "10", "0", "10", false);
    }

    #[test]
    fn dispute_then_chargeback_removes_funds_and_locks() {
        let ledger = ledger_with(&[
            deposit(1, 1, "10"),
            deposit(1, 2, "5"),
            dispute(1, 1),
            chargeback(1, 1),
        ]);
        assert_account(&ledger, 1, "5", "0", "5", true);
    }

    #[test]
    fn dispute_after_withdrawal_drives_available_negative() {
        let ledger = ledger_with(&[
            deposit(1, 1, "10"),
            withdrawal(1, 2, "10"),
            dispute(1, 1),
            chargeback(1, 1),
        ]);
        assert_account(&ledger, 1, "-10", "0", "-10", true);
    }

    #[test]
    fn concurrent_disputes_on_separate_deposits() {
        let ledger = ledger_with(&[
            deposit(1, 1, "10"),
            deposit(1, 2, "3"),
            dispute(1, 1),
            dispute(1, 2),
            resolve(1, 2),
        ]);
        assert_account(&ledger, 1, "3", "10", "13", false);
    }

    #[test]
    fn dispute_of_unknown_tx_is_ignored_and_creates_no_account() {
        let mut ledger = Ledger::new();
        assert_eq!(
            ledger.apply(dispute(1, 99)),
            Err(LedgerError::UnknownTx(99))
        );
        assert_eq!(
            ledger.apply(resolve(1, 99)),
            Err(LedgerError::UnknownTx(99))
        );
        assert_eq!(
            ledger.apply(chargeback(1, 99)),
            Err(LedgerError::UnknownTx(99))
        );
        assert_eq!(ledger.accounts().count(), 0);
    }

    #[test]
    fn withdrawals_cannot_be_disputed() {
        let mut ledger = ledger_with(&[deposit(1, 1, "10"), withdrawal(1, 2, "4")]);
        assert_eq!(
            ledger.apply(dispute(1, 2)),
            Err(LedgerError::NotDisputable(2))
        );
        assert_account(&ledger, 1, "6", "0", "6", false);
    }

    #[test]
    fn dispute_steps_from_wrong_client_are_rejected() {
        let mut ledger = ledger_with(&[deposit(1, 1, "10"), deposit(2, 2, "1")]);
        let mismatch = LedgerError::ClientMismatch {
            tx: 1,
            owner: 1,
            claimed: 2,
        };
        assert_eq!(ledger.apply(dispute(2, 1)), Err(mismatch.clone()));

        ledger.apply(dispute(1, 1)).unwrap();
        assert_eq!(ledger.apply(resolve(2, 1)), Err(mismatch.clone()));
        assert_eq!(ledger.apply(chargeback(2, 1)), Err(mismatch));
        assert_account(&ledger, 1, "0", "10", "10", false);
        assert_account(&ledger, 2, "1", "0", "1", false);
    }

    fn invalid(tx: TxId, from: DisputeState, action: Action) -> LedgerError {
        LedgerError::InvalidTransition {
            tx,
            source: TransactionError::InvalidTransition { from, action },
        }
    }

    #[test]
    fn resolve_or_chargeback_without_dispute_is_rejected() {
        let mut ledger = ledger_with(&[deposit(1, 1, "10")]);
        assert_eq!(
            ledger.apply(resolve(1, 1)),
            Err(invalid(1, DisputeState::Undisputed, Action::Resolve))
        );
        assert_eq!(
            ledger.apply(chargeback(1, 1)),
            Err(invalid(1, DisputeState::Undisputed, Action::Chargeback))
        );
        assert_account(&ledger, 1, "10", "0", "10", false);
    }

    #[test]
    fn double_dispute_is_rejected() {
        let mut ledger = ledger_with(&[deposit(1, 1, "10"), dispute(1, 1)]);
        assert_eq!(
            ledger.apply(dispute(1, 1)),
            Err(invalid(1, DisputeState::Disputed, Action::Dispute))
        );
        assert_account(&ledger, 1, "0", "10", "10", false);
    }

    #[test]
    fn resolved_deposit_cannot_be_disputed_again() {
        let mut ledger = ledger_with(&[deposit(1, 1, "10"), dispute(1, 1), resolve(1, 1)]);
        assert_eq!(
            ledger.apply(dispute(1, 1)),
            Err(invalid(1, DisputeState::Resolved, Action::Dispute))
        );
        assert_eq!(
            ledger.apply(resolve(1, 1)),
            Err(invalid(1, DisputeState::Resolved, Action::Resolve))
        );
        assert_account(&ledger, 1, "10", "0", "10", false);
    }

    // ---- locked accounts -------------------------------------------------

    #[test]
    fn locked_account_rejects_everything() {
        let mut ledger = ledger_with(&[
            deposit(1, 1, "10"),
            deposit(1, 2, "5"),
            deposit(1, 3, "1"),
            dispute(1, 3),
            dispute(1, 1),
            chargeback(1, 1),
        ]);
        // available 5, held 1 (tx 3 still disputed), locked
        assert_account(&ledger, 1, "5", "1", "6", true);

        let locked = |tx| LedgerError::Account {
            tx,
            client: 1,
            source: AccountError::Locked,
        };
        assert_eq!(ledger.apply(deposit(1, 10, "1")), Err(locked(10)));
        assert_eq!(ledger.apply(withdrawal(1, 11, "1")), Err(locked(11)));
        assert_eq!(ledger.apply(dispute(1, 2)), Err(locked(2)));
        assert_eq!(ledger.apply(resolve(1, 3)), Err(locked(3)));
        assert_eq!(ledger.apply(chargeback(1, 3)), Err(locked(3)));
        assert_account(&ledger, 1, "5", "1", "6", true);
    }

    #[test]
    fn rejected_dispute_step_on_locked_account_does_not_change_dispute_state() {
        // If the account op fails, the deposit's state must not move on.
        let mut ledger = ledger_with(&[
            deposit(1, 1, "10"),
            deposit(1, 2, "1"),
            dispute(1, 1),
            dispute(1, 2),
            chargeback(1, 2), // locks
        ]);
        assert!(ledger.apply(resolve(1, 1)).is_err());
        // tx 1 is still Disputed, so it reports Locked, not an invalid transition.
        assert_eq!(
            ledger.apply(chargeback(1, 1)),
            Err(LedgerError::Account {
                tx: 1,
                client: 1,
                source: AccountError::Locked
            })
        );
    }

    #[test]
    fn locking_one_client_does_not_affect_others() {
        let mut ledger = ledger_with(&[
            deposit(1, 1, "10"),
            deposit(2, 2, "10"),
            dispute(1, 1),
            chargeback(1, 1),
        ]);
        ledger.apply(withdrawal(2, 3, "4")).unwrap();
        assert_account(&ledger, 1, "0", "0", "0", true);
        assert_account(&ledger, 2, "6", "0", "6", false);
    }
}
