//! **A hole, as its producer states it** — the payload of a
//! [`FrameKind::Gap`](crate::wire::frame::FrameKind::Gap) frame and the width it
//! carries. Who lost it (`GapOrigin`) and what it is as a record (`Rec::Gap`)
//! are the daemon's vocabulary and live in `model::gap`.

use serde::{Deserialize, Serialize};
use crate::time::Nanos;

/// How wide the hole is — or that nobody can say.
///
/// [`Unknown`](GapExtent::Unknown) is a VARIANT and not a zero, because "lost an
/// unknown amount" and "lost nothing" are opposite facts and a zero erases the
/// difference.
///
/// A count here is a **LOWER BOUND** and is documented as one at every source
/// that produces it: a ring that reports `n` samples past its capacity knows at
/// least `n` were overwritten, never that only `n` were.
/// **Externally tagged** (serde's default), deliberately. It was written
/// `#[serde(tag = "kind")]`, and an internally-tagged enum cannot serialize a
/// newtype variant wrapping an integer — in ANY format. So `AtLeastSamples`
/// errored on `serde_json::to_vec` and, more to the point, on the
/// `bincode::serialize` every spool append runs: a `Rec::Gap` naming a bounded
/// hole could not be made durable at all. Nothing had ever constructed one, so
/// nothing had ever tried. Pinned now by `a_bounded_extent_survives_the_codec_the
/// _spool_actually_uses`, which encodes with the spool's own codec rather than a
/// self-describing stand-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapExtent {
    /// The driver said samples were lost and cannot say how many — a board that
    /// faulted mid-block, a source whose protocol reports a loss and no count.
    /// (NI-DAQmx was the example here until 2026-09-27: `-200279` states no
    /// width, but the read position before and after the loss counts it
    /// exactly, so nidaqmx reports [`AtLeastSamples`](GapExtent::AtLeastSamples).)
    Unknown,
    /// The driver's own LOWER BOUND on the samples-per-channel lost. The true
    /// hole is this wide or wider.
    AtLeastSamples(u64),
}

impl GapExtent {
    /// Sample periods the new frame is placed past the old frame's end.
    ///
    /// At least ONE, always: a reported loss with a zero-width hole is not a
    /// loss, and placing the next frame exactly at the frontier is the thing
    /// that reads as contiguous. Where a bound is known it is used, because a
    /// known lower bound is nearer the truth than the minimum one.
    pub fn periods(self) -> u64 {
        match self {
            GapExtent::Unknown => 1,
            GapExtent::AtLeastSamples(n) => n.max(1),
        }
    }

    /// The samples-per-channel this hole is known to be at least, when anything
    /// can say. `None` is [`GapExtent::Unknown`] — never a zero, for the reason
    /// on the variant.
    pub fn at_least(self) -> Option<u64> {
        match self {
            GapExtent::Unknown => None,
            GapExtent::AtLeastSamples(n) => Some(n),
        }
    }
}

/// **The gap frame's payload** — one hole, as the extension states it, crossing
/// the stdout wire as [`crate::wire::frame::FrameKind::Gap`].
///
/// It is the producer half of `model::Rec::Gap` and deliberately not the same
/// type: the wire says it ONCE for the whole affected channel set, and the
/// daemon expands it into one record per path (a DAQ overrun loses every
/// channel in the task at once, and `WHERE path = …` must answer "is this
/// channel's history complete?" without a join). Expanding at the daemon rather
/// than on the wire keeps a 64-channel overrun one small JSON object on a pipe
/// that is already the throughput bottleneck.
///
/// **One statement per EVENT, never per lost sample.** [`GapExtent`] carries the
/// width, so a hole of 300 000 samples is one statement. Volume is therefore
/// bounded by how often acquisition BREAKS, not by how much was lost — which is
/// what makes it safe to emit unconditionally. An extension's stderr may be
/// rate-limited by the daemon; this must never be. Rate-limiting the log is
/// tidiness, rate-limiting the record is the defect being fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapStatement {
    /// Every channel the hole covers — the acquisition's whole channel set,
    /// because a source that loses a block loses it on all of them at once.
    pub paths: Vec<String>,
    /// **The frontier**: the instant just past the last sample that was really
    /// acquired, i.e. where good data stopped and the hole opens. Not the
    /// instant the loss was noticed, and not where the next block resumes —
    /// either of those would place the hole somewhere it was not.
    pub frontier_ns: Nanos,
    /// How wide, or that nobody can say.
    pub extent: GapExtent,
    /// The losing party's own words — a DAQmx status, "chunk arrived 100 ms
    /// late". Free text: the machine-readable half is the extent and the
    /// origin, and this is the half that saves an engineer a source dive.
    pub detail: String,
}
