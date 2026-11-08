//! Transactions and the dispute lifecycle.

use std::fmt;

use thiserror::Error;

use super::amount::Amount;

pub type ClientId = u16;
pub type TxId = u32;

/// A validated transaction.
///
/// Each variant carries exactly the data it needs: a dispute cannot carry an
/// amount and a deposit cannot lack one. Deposits and withdrawals should be
/// built via [`Transaction::deposit`] / [`Transaction::withdrawal`], which
/// guarantee a strictly positive amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transaction {
    Deposit {
        client: ClientId,
        tx: TxId,
        amount: Amount,
    },
    Withdrawal {
        client: ClientId,
        tx: TxId,
        amount: Amount,
    },
    Dispute {
        client: ClientId,
        tx: TxId,
    },
    Resolve {
        client: ClientId,
        tx: TxId,
    },
    Chargeback {
        client: ClientId,
        tx: TxId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransactionError {
    #[error("transaction amount must be positive, got {0}")]
    NonPositiveAmount(Amount),
    #[error("cannot {action} a transaction that is {from:?}")]
    InvalidTransition { from: DisputeState, action: Action },
}

/// An attempted step in the dispute lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Dispute,
    Resolve,
    Chargeback,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verb = match self {
            Action::Dispute => "dispute",
            Action::Resolve => "resolve",
            Action::Chargeback => "chargeback",
        };
        f.write_str(verb)
    }
}

impl Transaction {
    pub fn deposit(client: ClientId, tx: TxId, amount: Amount) -> Result<Self, TransactionError> {
        ensure_positive(amount)?;
        Ok(Transaction::Deposit { client, tx, amount })
    }

    pub fn withdrawal(
        client: ClientId,
        tx: TxId,
        amount: Amount,
    ) -> Result<Self, TransactionError> {
        ensure_positive(amount)?;
        Ok(Transaction::Withdrawal { client, tx, amount })
    }

    pub fn client(&self) -> ClientId {
        match *self {
            Transaction::Deposit { client, .. }
            | Transaction::Withdrawal { client, .. }
            | Transaction::Dispute { client, .. }
            | Transaction::Resolve { client, .. }
            | Transaction::Chargeback { client, .. } => client,
        }
    }

    pub fn tx(&self) -> TxId {
        match *self {
            Transaction::Deposit { tx, .. }
            | Transaction::Withdrawal { tx, .. }
            | Transaction::Dispute { tx, .. }
            | Transaction::Resolve { tx, .. }
            | Transaction::Chargeback { tx, .. } => tx,
        }
    }
}

fn ensure_positive(amount: Amount) -> Result<(), TransactionError> {
    if amount.is_positive() {
        Ok(())
    } else {
        Err(TransactionError::NonPositiveAmount(amount))
    }
}

/// Where a disputable (deposit) transaction is in its dispute lifecycle.
///
/// ```text
/// Undisputed --dispute--> Disputed --resolve----> Resolved     (terminal)
///                                  \--chargeback-> ChargedBack (terminal)
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DisputeState {
    #[default]
    Undisputed,
    Disputed,
    Resolved,
    ChargedBack,
}

impl DisputeState {
    pub fn dispute(self) -> Result<DisputeState, TransactionError> {
        match self {
            DisputeState::Undisputed => Ok(DisputeState::Disputed),
            from => Err(TransactionError::InvalidTransition {
                from,
                action: Action::Dispute,
            }),
        }
    }

    pub fn resolve(self) -> Result<DisputeState, TransactionError> {
        match self {
            DisputeState::Disputed => Ok(DisputeState::Resolved),
            from => Err(TransactionError::InvalidTransition {
                from,
                action: Action::Resolve,
            }),
        }
    }

    pub fn chargeback(self) -> Result<DisputeState, TransactionError> {
        match self {
            DisputeState::Disputed => Ok(DisputeState::ChargedBack),
            from => Err(TransactionError::InvalidTransition {
                from,
                action: Action::Chargeback,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DisputeState::*;

    fn amt(s: &str) -> Amount {
        s.parse().unwrap()
    }

    #[test]
    fn deposit_and_withdrawal_accept_positive_amounts() {
        assert_eq!(
            Transaction::deposit(1, 10, amt("1.5")),
            Ok(Transaction::Deposit {
                client: 1,
                tx: 10,
                amount: amt("1.5")
            })
        );
        assert_eq!(
            Transaction::withdrawal(2, 20, amt("0.0001")),
            Ok(Transaction::Withdrawal {
                client: 2,
                tx: 20,
                amount: amt("0.0001")
            })
        );
    }

    #[test]
    fn deposit_rejects_zero_and_negative() {
        assert_eq!(
            Transaction::deposit(1, 1, Amount::ZERO),
            Err(TransactionError::NonPositiveAmount(Amount::ZERO))
        );
        assert!(Transaction::deposit(1, 1, amt("-1")).is_err());
    }

    #[test]
    fn withdrawal_rejects_zero_and_negative() {
        assert!(Transaction::withdrawal(1, 1, Amount::ZERO).is_err());
        assert!(Transaction::withdrawal(1, 1, amt("-0.0001")).is_err());
    }

    #[test]
    fn accessors_work_for_every_variant() {
        let all = [
            Transaction::deposit(7, 70, amt("1")).unwrap(),
            Transaction::withdrawal(7, 70, amt("1")).unwrap(),
            Transaction::Dispute { client: 7, tx: 70 },
            Transaction::Resolve { client: 7, tx: 70 },
            Transaction::Chargeback { client: 7, tx: 70 },
        ];
        for t in all {
            assert_eq!(t.client(), 7);
            assert_eq!(t.tx(), 70);
        }
    }

    #[test]
    fn dispute_state_defaults_to_undisputed() {
        assert_eq!(DisputeState::default(), Undisputed);
    }

    #[test]
    fn valid_transitions() {
        assert_eq!(Undisputed.dispute(), Ok(Disputed));
        assert_eq!(Disputed.resolve(), Ok(Resolved));
        assert_eq!(Disputed.chargeback(), Ok(ChargedBack));
    }

    #[test]
    fn dispute_only_allowed_from_undisputed() {
        for from in [Disputed, Resolved, ChargedBack] {
            assert_eq!(
                from.dispute(),
                Err(TransactionError::InvalidTransition {
                    from,
                    action: Action::Dispute
                })
            );
        }
    }

    #[test]
    fn resolve_only_allowed_from_disputed() {
        for from in [Undisputed, Resolved, ChargedBack] {
            assert_eq!(
                from.resolve(),
                Err(TransactionError::InvalidTransition {
                    from,
                    action: Action::Resolve
                })
            );
        }
    }

    #[test]
    fn chargeback_only_allowed_from_disputed() {
        for from in [Undisputed, Resolved, ChargedBack] {
            assert_eq!(
                from.chargeback(),
                Err(TransactionError::InvalidTransition {
                    from,
                    action: Action::Chargeback
                })
            );
        }
    }

    #[test]
    fn invalid_transition_error_message_names_the_action() {
        let err = Resolved.dispute().unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot dispute a transaction that is Resolved"
        );
    }
}
