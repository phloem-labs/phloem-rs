//! [`UniformStream`] — the fixed-rate sender that cannot step backwards.

use crate::wire::frame::sample_instant;

use crate::encode::{Column, Error, Step, TimeBasis};

/// **A fixed-rate sender that cannot step backwards.** Anchor once; every
/// frame after starts exactly where the previous one ended — frame `k` at
/// `anchor + floor(n·num_ns/den)` for the `n` samples sent before it, the
/// floored grid the daemon places uniform samples on (`crate::wire::frame::
/// sample_instant`) — so no frame overlaps its predecessor whatever the
/// sender's loop timing.
///
/// What a sender that stamped every frame with the wall clock at send time
/// got wrong: a loop that woke 3 ms early wrote a frame 3 ms behind the
/// previous one's end, and its whole frame was refused (2026-10-06).
///
/// **The anchor.** On [`TimeBasis::Host`] the first push anchors at the host
/// clock minus the frame's duration — the frame's samples were acquired over
/// the time just past. On [`TimeBasis::Source`] / [`TimeBasis::Backdated`]
/// the instant is the device's or the recording's, which only the caller
/// knows: give it with [`anchored_at`](Self::anchored_at) (a push without
/// one is refused).
///
/// **Re-anchoring forward, only on real drift.** On the host basis each push
/// compares where the stream's frame would end with the host clock. A stream
/// more than [`drift_tolerance_ns`](Self::drift_tolerance_ns) BEHIND the
/// clock has delivered fewer samples than its stated rate produces in that
/// time — a stalled producer — so it re-anchors forward and STATES the
/// skipped span as a gap (the whole periods missed, a lower bound), never
/// silently. A stream AHEAD of the clock is never moved back: that would be
/// the overlap this type exists to prevent. Jitter inside the tolerance is
/// absorbed by the contiguous grid. The default, one second, is wider than
/// any loop's jitter and narrower than a stall worth a hole; a producer whose
/// buffer legitimately delivers late by more than that sets its own.
#[derive(Debug, Clone)]
pub struct UniformStream {
    pub(crate) paths: Vec<String>,
    pub(crate) step: Step,
    pub(crate) basis: TimeBasis,
    anchor_ns: Option<i64>,
    /// Samples per channel sent since the anchor.
    sent: u64,
    drift_tolerance_ns: i64,
    reanchors: u64,
}

/// Where a pushed frame went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pushed {
    /// The frame's first instant.
    pub t0_ns: i64,
    /// `Some(n)`: the stream had fallen behind the host clock and was
    /// re-anchored forward, and a gap of at least `n` samples was stated first.
    pub reanchored: Option<u64>,
}

pub(crate) struct Plan {
    pub(crate) t0_ns: i64,
    /// `(frontier, missing samples)` when re-anchored.
    pub(crate) reanchored: Option<(i64, u64)>,
}

/// The default re-anchor tolerance — see [`UniformStream`].
pub const DRIFT_TOLERANCE_NS: i64 = 1_000_000_000;

impl UniformStream {
    /// A stream over `channels` (each once), stepping by `step`, on `basis`.
    pub fn new(
        channels: impl IntoIterator<Item = impl Into<String>>,
        step: Step,
        basis: TimeBasis,
    ) -> Result<Self, Error> {
        let paths: Vec<String> = channels.into_iter().map(Into::into).collect();
        if paths.is_empty() {
            return Err(Error::Usage("a stream needs at least one channel".into()));
        }
        let mut seen = std::collections::HashSet::new();
        for p in &paths {
            if p.trim().is_empty() {
                return Err(Error::Usage("a stream's channel needs a path".into()));
            }
            if !seen.insert(p.as_str()) {
                return Err(Error::Usage(format!("`{p}` is named twice in the stream")));
            }
        }
        Ok(Self {
            paths,
            step,
            basis,
            anchor_ns: None,
            sent: 0,
            drift_tolerance_ns: DRIFT_TOLERANCE_NS,
            reanchors: 0,
        })
    }

    /// [`new`](Self::new) at a rate in Hz.
    pub fn at_hz(
        channels: impl IntoIterator<Item = impl Into<String>>,
        hz: f64,
        basis: TimeBasis,
    ) -> Result<Self, Error> {
        let step = Step::from_rate_hz_f64(hz)
            .ok_or_else(|| Error::Usage(format!("{hz} Hz is not a usable rate")))?;
        Self::new(channels, step, basis)
    }

    /// The first sample's instant, on the stream's basis. Required for
    /// `Source` and `Backdated`; on `Host` it replaces the first push's
    /// clock reading.
    pub fn anchored_at(mut self, t0_ns: i64) -> Self {
        self.anchor_ns = Some(t0_ns);
        self
    }

    /// How far behind the host clock a host-basis stream may fall before it
    /// is re-anchored forward (default [`DRIFT_TOLERANCE_NS`]).
    pub fn drift_tolerance_ns(mut self, ns: i64) -> Self {
        self.drift_tolerance_ns = ns.max(0);
        self
    }

    pub fn channels(&self) -> &[String] {
        &self.paths
    }
    pub fn step(&self) -> Step {
        self.step
    }
    pub fn basis(&self) -> TimeBasis {
        self.basis
    }
    /// Where the next frame starts; `None` before a host stream's first push.
    pub fn next_t0_ns(&self) -> Option<i64> {
        self.anchor_ns.map(|a| self.at(a, self.sent))
    }
    /// Times the stream re-anchored forward.
    pub fn reanchors(&self) -> u64 {
        self.reanchors
    }

    fn at(&self, anchor: i64, n: u64) -> i64 {
        sample_instant(crate::time::Nanos::from_raw(anchor), self.step, n).raw_ns()
    }

    pub(crate) fn check(&self, columns: &[Column]) -> Result<(), Error> {
        let same = columns.len() == self.paths.len()
            && self.paths.iter().all(|p| columns.iter().filter(|c| &c.path == p).count() == 1);
        if !same {
            let got: Vec<&str> = columns.iter().map(|c| c.path.as_str()).collect();
            return Err(Error::Usage(format!(
                "a stream's frame carries exactly its channels {:?}, got {got:?}",
                self.paths
            )));
        }
        Ok(())
    }

    pub(crate) fn plan(&mut self, rows: u64, now_ns: i64) -> Result<Plan, Error> {
        let dur = self.at(0, rows);
        let anchor = match (self.anchor_ns, self.basis) {
            (Some(a), _) => a,
            (None, TimeBasis::Host) => {
                let a = now_ns.saturating_sub(dur);
                self.anchor_ns = Some(a);
                a
            }
            (None, _) => {
                return Err(Error::Usage(
                    "a stream on the source or backdated basis needs its first instant: \
                     UniformStream::anchored_at(t0_ns)"
                        .into(),
                ))
            }
        };
        let t0 = self.at(anchor, self.sent);
        if self.basis == TimeBasis::Host {
            let behind = now_ns.saturating_sub(t0.saturating_add(dur));
            if behind > self.drift_tolerance_ns {
                let new_t0 = now_ns.saturating_sub(dur);
                // Whole periods skipped, rounded down: a lower bound, as the
                // gap record promises.
                let missing = ((new_t0 - t0) as i128 * self.step.den as i128
                    / self.step.num_ns as i128) as u64;
                self.anchor_ns = Some(new_t0);
                self.sent = 0;
                self.reanchors += 1;
                return Ok(Plan { t0_ns: new_t0, reanchored: Some((t0, missing)) });
            }
        }
        Ok(Plan { t0_ns: t0, reanchored: None })
    }

    pub(crate) fn advance(&mut self, planned_ns: i64, written_ns: i64, rows: u64) {
        if written_ns == planned_ns {
            self.sent += rows;
        } else {
            // The encoder continued the frame from a later end than the
            // stream knew of — raw `uniform` calls on the same channels
            // through the same encoder. The stream follows what was written.
            self.anchor_ns = Some(written_ns);
            self.sent = rows;
        }
    }
}
