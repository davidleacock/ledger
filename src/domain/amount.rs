//! Fixed-precision monetary value.

use std::fmt;
use std::str::FromStr;

use rust_decimal::Decimal;
use thiserror::Error;

/// Maximum number of digits allowed after the decimal point.
pub const MAX_SCALE: u32 = 4;

/// A monetary value with at most four decimal places.
///
/// `Amount` is signed: account balances may legitimately go negative (e.g. a
/// dispute on a deposit whose funds were already withdrawn). The requirement
/// that *transaction* amounts be strictly positive is enforced where
/// transactions are constructed, not here.
///
/// Arithmetic is only exposed through checked methods so that overflow can
/// never silently corrupt a balance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Amount(Decimal);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AmountError {
    #[error("amount has more than {MAX_SCALE} decimal places")]
    TooPrecise,
    #[error("invalid amount: {0}")]
    Invalid(String),
}

impl Amount {
    pub const ZERO: Amount = Amount(Decimal::ZERO);

    pub fn is_positive(&self) -> bool {
        self.0 > Decimal::ZERO
    }

    pub fn is_negative(&self) -> bool {
        self.0 < Decimal::ZERO
    }

    pub fn checked_add(self, rhs: Amount) -> Option<Amount> {
        self.0.checked_add(rhs.0).map(Amount)
    }

    pub fn checked_sub(self, rhs: Amount) -> Option<Amount> {
        self.0.checked_sub(rhs.0).map(Amount)
    }
}

impl TryFrom<Decimal> for Amount {
    type Error = AmountError;

    /// Rejects values with more than four significant decimal places rather
    /// than rounding them: silently altering a monetary value is worse than
    /// refusing it. Trailing zeros are ignored (`1.50000` is accepted).
    fn try_from(value: Decimal) -> Result<Self, Self::Error> {
        let normalized = value.normalize();
        if normalized.scale() > MAX_SCALE {
            return Err(AmountError::TooPrecise);
        }
        Ok(Amount(normalized))
    }
}

impl FromStr for Amount {
    type Err = AmountError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let decimal =
            Decimal::from_str(trimmed).map_err(|_| AmountError::Invalid(trimmed.to_owned()))?;
        Amount::try_from(decimal)
    }
}

impl fmt::Display for Amount {
    /// Always renders exactly four decimal places, e.g. `1.5000`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.prec$}", self.0, prec = MAX_SCALE as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn amt(s: &str) -> Amount {
        s.parse().unwrap()
    }

    #[test]
    fn parses_integers_and_decimals_up_to_four_places() {
        assert_eq!(amt("1"), Amount(dec!(1)));
        assert_eq!(amt("1.5"), Amount(dec!(1.5)));
        assert_eq!(amt("1.2345"), Amount(dec!(1.2345)));
    }

    #[test]
    fn parsing_trims_surrounding_whitespace() {
        assert_eq!(amt("  2.5 "), Amount(dec!(2.5)));
    }

    #[test]
    fn rejects_more_than_four_decimal_places() {
        assert_eq!("1.23456".parse::<Amount>(), Err(AmountError::TooPrecise));
    }

    #[test]
    fn accepts_trailing_zeros_beyond_four_places() {
        assert_eq!(amt("1.50000000"), amt("1.5"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            "abc".parse::<Amount>(),
            Err(AmountError::Invalid(_))
        ));
        assert!(matches!("".parse::<Amount>(), Err(AmountError::Invalid(_))));
    }

    #[test]
    fn try_from_decimal_enforces_scale() {
        assert!(Amount::try_from(dec!(0.0001)).is_ok());
        assert_eq!(
            Amount::try_from(dec!(0.00001)),
            Err(AmountError::TooPrecise)
        );
    }

    #[test]
    fn displays_exactly_four_decimal_places() {
        assert_eq!(amt("1.5").to_string(), "1.5000");
        assert_eq!(Amount::ZERO.to_string(), "0.0000");
        assert_eq!(amt("3").to_string(), "3.0000");
        assert_eq!(amt("0.1234").to_string(), "0.1234");
    }

    #[test]
    fn negative_values_are_allowed_and_display_correctly() {
        let a = amt("-2.5");
        assert!(a.is_negative());
        assert!(!a.is_positive());
        assert_eq!(a.to_string(), "-2.5000");
    }

    #[test]
    fn sign_predicates() {
        assert!(amt("0.0001").is_positive());
        assert!(!Amount::ZERO.is_positive());
        assert!(!Amount::ZERO.is_negative());
    }

    #[test]
    fn checked_arithmetic() {
        assert_eq!(amt("1.5").checked_add(amt("2.25")), Some(amt("3.75")));
        assert_eq!(amt("1.5").checked_sub(amt("2.25")), Some(amt("-0.75")));
    }

    #[test]
    fn checked_add_overflow_returns_none() {
        let max = Amount::try_from(Decimal::MAX).unwrap();
        assert_eq!(max.checked_add(amt("1")), None);
    }

    #[test]
    fn checked_sub_overflow_returns_none() {
        let min = Amount::try_from(Decimal::MIN).unwrap();
        assert_eq!(min.checked_sub(amt("1")), None);
    }

    #[test]
    fn equality_ignores_representation_scale() {
        assert_eq!(amt("2"), amt("2.0000"));
    }
}
