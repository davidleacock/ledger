//! A single client's asset account.

use thiserror::Error;

use super::amount::Amount;
use super::transaction::ClientId;

/// A client's account balances.
///
/// `total` is derived (`available + held`) rather than stored, so the
/// invariant `total == available + held` holds by construction.
///
/// Every mutating operation is atomic: if it returns an error, the account is
/// left exactly as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    client: ClientId,
    available: Amount,
    held: Amount,
    locked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AccountError {
    #[error("account is locked")]
    Locked,
    #[error("insufficient available funds")]
    InsufficientFunds,
    #[error("insufficient held funds")]
    InsufficientHeld,
    #[error("arithmetic overflow")]
    Overflow,
}

impl Account {
    pub fn new(client: ClientId) -> Self {
        Account {
            client,
            available: Amount::ZERO,
            held: Amount::ZERO,
            locked: false,
        }
    }

    pub fn client(&self) -> ClientId {
        self.client
    }

    pub fn available(&self) -> Amount {
        self.available
    }

    pub fn held(&self) -> Amount {
        self.held
    }

    /// `available + held`.
    pub fn total(&self) -> Amount {
        self.available
            .checked_add(self.held)
            .expect("available + held should fit in a Decimal")
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Credit `amount` to available funds.
    pub fn deposit(&mut self, amount: Amount) -> Result<(), AccountError> {
        self.ensure_unlocked()?;
        let available = add(self.available, amount)?;
        self.ensure_total_in_range(available, self.held)?;
        self.available = available;
        Ok(())
    }

    /// Debit `amount` from available funds. Fails without changing anything
    /// if available funds are insufficient.
    pub fn withdraw(&mut self, amount: Amount) -> Result<(), AccountError> {
        self.ensure_unlocked()?;
        if self.available < amount {
            return Err(AccountError::InsufficientFunds);
        }
        self.available = sub(self.available, amount)?;
        Ok(())
    }

    /// Move `amount` from available to held (a dispute). Available may go
    /// negative if the disputed funds were already withdrawn.
    pub fn hold(&mut self, amount: Amount) -> Result<(), AccountError> {
        self.ensure_unlocked()?;
        let available = sub(self.available, amount)?;
        let held = add(self.held, amount)?;
        self.available = available;
        self.held = held;
        Ok(())
    }

    /// Move `amount` from held back to available (a resolve).
    pub fn release(&mut self, amount: Amount) -> Result<(), AccountError> {
        self.ensure_unlocked()?;
        self.ensure_held(amount)?;
        let held = sub(self.held, amount)?;
        let available = add(self.available, amount)?;
        self.held = held;
        self.available = available;
        Ok(())
    }

    /// Remove `amount` from held funds and lock the account (a chargeback).
    pub fn chargeback(&mut self, amount: Amount) -> Result<(), AccountError> {
        self.ensure_unlocked()?;
        self.ensure_held(amount)?;
        self.held = sub(self.held, amount)?;
        self.locked = true;
        Ok(())
    }

    fn ensure_unlocked(&self) -> Result<(), AccountError> {
        if self.locked {
            Err(AccountError::Locked)
        } else {
            Ok(())
        }
    }

    fn ensure_held(&self, amount: Amount) -> Result<(), AccountError> {
        if self.held < amount {
            Err(AccountError::InsufficientHeld)
        } else {
            Ok(())
        }
    }

    fn ensure_total_in_range(&self, available: Amount, held: Amount) -> Result<(), AccountError> {
        add(available, held).map(|_| ())
    }
}

fn add(a: Amount, b: Amount) -> Result<Amount, AccountError> {
    a.checked_add(b).ok_or(AccountError::Overflow)
}

fn sub(a: Amount, b: Amount) -> Result<Amount, AccountError> {
    a.checked_sub(b).ok_or(AccountError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn amt(s: &str) -> Amount {
        s.parse().unwrap()
    }

    fn assert_balances(acct: &Account, available: &str, held: &str, total: &str) {
        assert_eq!(acct.available(), amt(available), "available");
        assert_eq!(acct.held(), amt(held), "held");
        assert_eq!(acct.total(), amt(total), "total");
        assert_eq!(
            acct.total(),
            acct.available().checked_add(acct.held()).unwrap(),
            "total == available + held"
        );
    }

    fn funded(available: &str) -> Account {
        let mut a = Account::new(1);
        a.deposit(amt(available)).unwrap();
        a
    }

    #[test]
    fn new_account_is_empty_and_unlocked() {
        let a = Account::new(42);
        assert_eq!(a.client(), 42);
        assert_balances(&a, "0", "0", "0");
        assert!(!a.is_locked());
    }

    #[test]
    fn deposit_increases_available_and_total() {
        let mut a = Account::new(1);
        a.deposit(amt("1.5")).unwrap();
        a.deposit(amt("0.25")).unwrap();
        assert_balances(&a, "1.75", "0", "1.75");
    }

    #[test]
    fn withdraw_decreases_available_and_total() {
        let mut a = funded("3");
        a.withdraw(amt("1.25")).unwrap();
        assert_balances(&a, "1.75", "0", "1.75");
    }

    #[test]
    fn withdraw_entire_balance() {
        let mut a = funded("3");
        a.withdraw(amt("3")).unwrap();
        assert_balances(&a, "0", "0", "0");
    }

    #[test]
    fn withdraw_with_insufficient_funds_fails_and_changes_nothing() {
        let mut a = funded("1");
        let before = a.clone();
        assert_eq!(
            a.withdraw(amt("1.0001")),
            Err(AccountError::InsufficientFunds)
        );
        assert_eq!(a, before);
    }

    #[test]
    fn withdraw_cannot_use_held_funds() {
        let mut a = funded("2");
        a.hold(amt("2")).unwrap();
        assert_eq!(a.withdraw(amt("1")), Err(AccountError::InsufficientFunds));
        assert_balances(&a, "0", "2", "2");
    }

    #[test]
    fn hold_moves_available_to_held_keeping_total() {
        let mut a = funded("5");
        a.hold(amt("2")).unwrap();
        assert_balances(&a, "3", "2", "5");
    }

    #[test]
    fn hold_then_release_round_trips() {
        let mut a = funded("5");
        a.hold(amt("2")).unwrap();
        a.release(amt("2")).unwrap();
        assert_balances(&a, "5", "0", "5");
        assert!(!a.is_locked());
    }

    #[test]
    fn hold_then_chargeback_reduces_total_and_locks() {
        let mut a = funded("5");
        a.hold(amt("2")).unwrap();
        a.chargeback(amt("2")).unwrap();
        assert_balances(&a, "3", "0", "3");
        assert!(a.is_locked());
    }

    #[test]
    fn hold_can_drive_available_negative() {
        // deposit 10, withdraw 10, dispute the deposit
        let mut a = funded("10");
        a.withdraw(amt("10")).unwrap();
        a.hold(amt("10")).unwrap();
        assert_balances(&a, "-10", "10", "0");
    }

    #[test]
    fn chargeback_after_negative_hold_leaves_debt() {
        let mut a = funded("10");
        a.withdraw(amt("10")).unwrap();
        a.hold(amt("10")).unwrap();
        a.chargeback(amt("10")).unwrap();
        assert_balances(&a, "-10", "0", "-10");
        assert!(a.is_locked());
    }

    #[test]
    fn release_or_chargeback_more_than_held_fails_and_changes_nothing() {
        let mut a = funded("5");
        a.hold(amt("1")).unwrap();
        let before = a.clone();
        assert_eq!(a.release(amt("2")), Err(AccountError::InsufficientHeld));
        assert_eq!(a.chargeback(amt("2")), Err(AccountError::InsufficientHeld));
        assert_eq!(a, before);
    }

    #[test]
    fn locked_account_rejects_every_operation_and_changes_nothing() {
        let mut a = funded("5");
        a.hold(amt("2")).unwrap();
        a.hold(amt("1")).unwrap();
        a.chargeback(amt("2")).unwrap();
        let before = a.clone();

        assert_eq!(a.deposit(amt("1")), Err(AccountError::Locked));
        assert_eq!(a.withdraw(amt("1")), Err(AccountError::Locked));
        assert_eq!(a.hold(amt("1")), Err(AccountError::Locked));
        assert_eq!(a.release(amt("1")), Err(AccountError::Locked));
        assert_eq!(a.chargeback(amt("1")), Err(AccountError::Locked));
        assert_eq!(a, before);
    }

    #[test]
    fn deposit_overflow_fails_and_changes_nothing() {
        let max = Amount::try_from(Decimal::MAX).unwrap();
        let mut a = Account::new(1);
        a.deposit(max).unwrap();
        let before = a.clone();
        assert_eq!(a.deposit(amt("1")), Err(AccountError::Overflow));
        assert_eq!(a, before);
    }

    #[test]
    fn deposit_that_would_overflow_total_fails() {
        // available + held must stay representable.
        let max = Amount::try_from(Decimal::MAX).unwrap();
        let mut a = Account::new(1);
        a.deposit(max).unwrap();
        a.hold(max).unwrap(); // available 0, held MAX
        let before = a.clone();
        assert_eq!(a.deposit(amt("1")), Err(AccountError::Overflow));
        assert_eq!(a, before);
    }
}
