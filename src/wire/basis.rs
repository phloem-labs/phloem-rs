//! **Which clock a timestamp is on** — the closed vocabulary a frame states in
//! its schema metadata ([`crate::wire::frame::META_BASIS`]) and the lake keeps in its
//! `time_basis` column. [`crate::wire::Clock`] is the statement built on it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeBasis {
    /// The sample's OWN capture clock: a recording's `log_time`, a device that
    /// stamps its readings, an extension that carries the instrument's time. The
    /// honest case, and the only one two devices can be aligned on better than
    /// their clock discipline.
    Source,
    /// The host's ARRIVAL instant stood in — nothing upstream carried a capture
    /// time (live mqtt/serial/can/mavlink/rosbridge feeds; an unstamped extension
    /// sample). Late by the transport's latency, and jittered by it.
    Host,
    /// Host wall clock MINUS a computed offset: a block-reading DAQ hands over N
    /// samples at once and back-dates them at the declared rate. Spacing between
    /// samples is as good as the device's sample clock; the ANCHOR is the host's,
    /// so the whole block can sit a read-interval away from the truth.
    Backdated,
}

impl TimeBasis {
    /// The lowercase wire/column spelling (`source` · `host` · `backdated`).
    pub fn as_str(self) -> &'static str {
        match self {
            TimeBasis::Source => "source",
            TimeBasis::Host => "host",
            TimeBasis::Backdated => "backdated",
        }
    }

    /// Every basis — the closed vocabulary a catalog enumerates.
    pub const ALL: [TimeBasis; 3] = [TimeBasis::Source, TimeBasis::Host, TimeBasis::Backdated];

    /// What this basis means for an alignment claim's reader
    /// (`observation.basis`). See `observation::SubjectKind::meaning`.
    pub const fn meaning(self) -> &'static str {
        match self {
            TimeBasis::Source => {
                "the sample's OWN capture clock - the honest case, and the only one two \
                 devices can be aligned on better than their clock discipline"
            }
            TimeBasis::Host => {
                "the host's ARRIVAL instant stood in - nothing upstream carried a capture \
                 time. Late by the transport's latency and jittered by it"
            }
            TimeBasis::Backdated => {
                "host wall clock MINUS a computed offset - spacing is as good as the device's \
                 sample clock, but the whole block's anchor can sit a read-interval off"
            }
        }
    }
}

impl std::str::FromStr for TimeBasis {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "source" => Ok(TimeBasis::Source),
            "host" => Ok(TimeBasis::Host),
            "backdated" => Ok(TimeBasis::Backdated),
            other => Err(format!("unknown time basis `{other}`")),
        }
    }
}

impl std::fmt::Display for TimeBasis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
