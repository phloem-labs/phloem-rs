//! **Which clock a timestamp is on, and how well that clock is known.**
//!
//! [`TimeBasis`](crate::wire::TimeBasis) is the VOCABULARY — three words, one column,
//! parsed from a string by Rust, Python and TypeScript alike. [`Clock`] is the
//! STATEMENT a producer makes: the basis plus, for the one basis that has an
//! error to state, how large it is.
//!
//! # Why the bound lives here and not beside the basis
//!
//! It was measured at the daemon first, on 2026-08-30, by dividing arriving
//! timestamps — and that is the wrong end of the wire. The SDK already says why,
//! about the basis itself:
//!
//! > The basis is an ARGUMENT and not a defaulted setter … the daemon cannot see
//! > which clock an extension read, so a default is a guess that lands in the
//! > lake's `time_basis` column as a fact.
//!
//! The same sentence holds one word over. Only the driver that back-dated a
//! block knows how long that block waited before it was read; the daemon sees
//! the timestamps the driver already wrote and can, at best, reconstruct a
//! ceiling from them. A ceiling is not the measurement, and inferring one
//! downstream of the party holding the answer is the defect, not the fix.
//!
//! # Why a pair and not a fourth variant
//!
//! `TimeBasis` is a closed vocabulary with a string spelling
//! ([`TimeBasis::as_str`](crate::wire::TimeBasis::as_str) /
//! [`FromStr`](std::str::FromStr)), an enumeration
//! ([`TimeBasis::ALL`](crate::wire::TimeBasis::ALL)) and a lake COLUMN. A data-carrying
//! variant would break all three: the round trip through a string could not
//! restore the number, and the catalog could not name the variant without
//! inventing one. So the vocabulary stays a word and the measurement travels
//! beside it — as a second column in the lake, and as one value in the code.

use serde::{Deserialize, Serialize};

use crate::wire::TimeBasis;

/// A producer's statement about its timestamps: which clock, and — when that
/// clock is derived — how far the derivation can be off.
///
/// Construct through the three named constructors. There is deliberately no
/// struct literal and no `Default`: [`Clock::backdated`] REQUIRES the bound, so
/// a driver that back-dates cannot ship without saying how far, which is the
/// whole reason this type exists rather than two loose fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Clock {
    basis: TimeBasis,
    anchor_us: Option<i64>,
}

impl Clock {
    /// The samples carry their own capture clock. No bound: there is no
    /// derivation to be wrong about, and the residual error is the instrument's
    /// discipline, which this type does not speak for.
    pub const fn source() -> Self {
        Self { basis: TimeBasis::Source, anchor_us: None }
    }

    /// The host's arrival instant stood in. No bound HERE either, and that is a
    /// deliberate silence rather than an omission: the error is the transport's
    /// latency and jitter, which is a different quantity measured a different
    /// way. Serving a read interval in its place would answer the question with
    /// the wrong measurement, which is worse than the silence.
    pub const fn host() -> Self {
        Self { basis: TimeBasis::Host, anchor_us: None }
    }

    /// The block was anchored on the host's wall clock and the rest derived
    /// backwards at the device's rate.
    ///
    /// `anchor_us` is how far the anchor can sit from the truth — in practice
    /// the interval the producer reads at, because that is how long a block may
    /// have waited in the driver's buffer before anybody looked. It is REQUIRED,
    /// and negatives are clamped away rather than accepted: a signed bound has
    /// no meaning, and admitting one would put it on a screen.
    pub const fn backdated(anchor_us: i64) -> Self {
        Self {
            basis: TimeBasis::Backdated,
            anchor_us: Some(if anchor_us < 0 { 0 } else { anchor_us }),
        }
    }

    /// Back-dated, and the bound is NOT KNOWABLE by this producer.
    ///
    /// A deliberate second constructor rather than an `Option` on the first,
    /// because those are different acts: one states a measurement, the other
    /// states that no measurement exists. Both are choices a producer has to
    /// make, and neither can be reached by forgetting — which is the whole
    /// discipline [`Clock::backdated`] enforces, preserved here rather than
    /// punctured.
    ///
    /// It is honest exactly where the error is dominated by something the
    /// producer cannot see. The TDMS reader is the case it was written for: it
    /// anchors a file's last sample on the file's mtime, so the error is the
    /// LOGGING APPLICATION's flush latency — how long FlexLogger held those
    /// samples before writing them — and nothing in the file records it.
    /// Publishing this connector's own poll interval there would have been a
    /// number that looked like an answer and measured the wrong thing.
    ///
    /// Reads downstream as an honest empty answer must: not zero, not
    /// absent-basis, but the basis stated and the bound unknown.
    pub const fn backdated_unbounded() -> Self {
        Self { basis: TimeBasis::Backdated, anchor_us: None }
    }

    /// The vocabulary word — what the lake's `time_basis` column carries.
    pub const fn basis(self) -> TimeBasis {
        self.basis
    }

    /// How far these stamps can sit from the truth, in µs. `Some` only for
    /// [`Clock::backdated`]; see [`Clock::host`] for why the other derived case
    /// stays silent.
    pub const fn anchor_us(self) -> Option<i64> {
        self.anchor_us
    }

    /// Rebuild from the two lake columns. An `anchor_us` on any basis but
    /// `backdated` is DROPPED rather than carried: the column pair is written by
    /// this type, so a row that disagrees with it was written by something else,
    /// and the vocabulary word is the half three languages agree on.
    pub fn from_columns(basis: TimeBasis, anchor_us: Option<i64>) -> Self {
        match basis {
            TimeBasis::Backdated => match anchor_us {
                Some(us) => Self::backdated(us),
                None => Self::backdated_unbounded(),
            },
            TimeBasis::Source => Self::source(),
            TimeBasis::Host => Self::host(),
        }
    }
}

impl From<Clock> for TimeBasis {
    fn from(c: Clock) -> Self {
        c.basis
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the type: the bound cannot be forgotten where it exists and
    /// cannot be invented where it does not.
    #[test]
    fn only_a_back_dated_clock_carries_a_bound() {
        assert_eq!(Clock::backdated(1_000_000).anchor_us(), Some(1_000_000));
        assert_eq!(Clock::source().anchor_us(), None);
        assert_eq!(Clock::host().anchor_us(), None, "the transport's error is not this number");
    }

    /// Unknown is a THIRD answer, and it must not collapse into either of the
    /// other two: not a zero bound (a perfect anchor) and not a missing basis
    /// (nobody said which clock).
    #[test]
    fn an_unbounded_back_dating_states_the_basis_and_not_a_bound() {
        let c = Clock::backdated_unbounded();
        assert_eq!(c.basis(), TimeBasis::Backdated);
        assert_eq!(c.anchor_us(), None);
        assert_ne!(c, Clock::backdated(0), "unknown is not a perfect anchor");
        assert_eq!(Clock::from_columns(TimeBasis::Backdated, None), c, "and it round trips");
    }

    #[test]
    fn every_clock_still_answers_the_vocabulary() {
        assert_eq!(Clock::backdated(5).basis().as_str(), "backdated");
        assert_eq!(Clock::source().basis(), TimeBasis::Source);
        assert_eq!(TimeBasis::from(Clock::host()), TimeBasis::Host);
    }

    /// A negative bound is not a bound. Clamping rather than accepting keeps
    /// "-3 ms" off a screen where it would read as an instrument fault.
    #[test]
    fn a_negative_bound_is_clamped_rather_than_shown() {
        assert_eq!(Clock::backdated(-7).anchor_us(), Some(0));
    }

    /// Round-tripping the lake's two columns. A stray bound on a basis that has
    /// none is dropped, not promoted — the word is the half that is written by
    /// three languages and the number is written by one.
    #[test]
    fn the_column_pair_round_trips_and_drops_what_disagrees() {
        for c in [Clock::source(), Clock::host(), Clock::backdated(2_500)] {
            assert_eq!(Clock::from_columns(c.basis(), c.anchor_us()), c);
        }
        assert_eq!(Clock::from_columns(TimeBasis::Source, Some(99)), Clock::source());
        assert_eq!(Clock::from_columns(TimeBasis::Backdated, None), Clock::backdated_unbounded());
    }
}
