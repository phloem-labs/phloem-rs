//! The extension stdout **frame wire** — length-prefixed framing plus the two time
//! forms every source extension declares per frame.
//!
//! This module owns three things and nothing else:
//!
//! 1. **The header bytes.** Exactly 12, pinned byte-for-byte by
//!    [`tests::header_bytes_are_pinned`]. Everything a reader needs to know how
//!    many payload bytes follow, and enough fixed bytes that a stream which is
//!    not this protocol is caught on the first frame instead of being decoded as
//!    data.
//! 2. **The terminal faults.** A framed stream cannot be resynchronised
//!    honestly: after a bad byte, the reader does not know whether what follows
//!    is a frame or the middle of one, and guessing is how silent corruption
//!    gets into a lake. So every fault here is TERMINAL and NAMED — see
//!    [`WireFault`] — and the daemon surfaces it as the source's exit reason.
//! 3. **The canonical sample instant.** [`sample_instant`] is the ONE
//!    `t_k = t0 + k·step` in the system. Producer and consumer both call it, so
//!    a uniform frame's omitted timestamps mean the same thing on both sides of
//!    the pipe.
//!
//! The Arrow payload itself lives in [`crate::wire::payload`].
//!
//! # Why length-prefixed, and why no cap
//!
//! The line wire had no length, so a reader could not know how much to buffer
//! before it had buffered it; the defence was an 8 MiB per-line cap, and a
//! 16-channel board at 100 kS/s/ch produces a 9.3 MiB chunk. The cap killed the
//! card. A length prefix removes the reason the cap existed: the reader knows
//! the size before the first payload byte, so bounding it by a constant buys
//! nothing that the length itself does not already give.
//!
//! What replaces the cap is not a bigger number — it is three structural rules:
//!
//! * **No reader ever pre-allocates a declared length.** The payload buffer
//!   grows with bytes that actually arrived, so a frame claiming 4 GiB and
//!   delivering 100 costs 100 bytes and then [`WireFault::Truncated`].
//! * **A payload is validated on its first four bytes** ([`FrameKind::prefix`]).
//!   A header that somehow passed with a nonsense length still fails within four
//!   payload bytes, not after the declared length has failed to arrive.
//! * **EOF inside a frame is a fault, not an end of stream.** A child that dies
//!   mid-frame is reported with the byte counts, never mistaken for a clean
//!   completion.
//!
//! Together those mean the reader can neither be starved into an unbounded wait
//! nor talked into an unbounded allocation, without any number standing between
//! a device and its own data.

use serde::{Deserialize, Serialize};

use crate::time::{Nanos, NS_PER_S};

/// First four bytes of every frame. Four fixed bytes at a fixed offset are what
/// make a stray `println!` a diagnosable event instead of a decode.
pub const FRAME_MAGIC: [u8; 4] = *b"PHLM";

/// Wire version, byte 4. There is exactly ONE version and no negotiation: every
/// extension is Phloem's own and ships with the daemon that reads it. The byte
/// exists so a mismatched pair fails by name rather than by misparse.
pub const FRAME_VERSION: u8 = 1;

/// Header size in bytes — `magic(4) · version(1) · kind(1) · reserved(2) · len(4)`.
pub const FRAME_HEADER_BYTES: usize = 12;

/// What a frame's payload is. The discriminant is a header byte, so the two
/// planes are separated before a single payload byte is interpreted — there is
/// no sniffing, and no shape that could be read as either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameKind {
    /// An Arrow IPC **stream** buffer holding one uniform or explicit frame.
    /// Built and parsed by [`crate::wire::payload`].
    Data = 1,
    /// A single UTF-8 JSON object — the extension's out-of-band statements
    /// (channel declarations). Never data: a control frame carries no samples.
    Control = 2,
    /// A single UTF-8 JSON object — [`crate::wire::gap::GapStatement`]: samples the source expected
    /// and will never produce.
    ///
    /// **Why a KIND and not a control statement.** A gap carries no samples, so
    /// [`Control`](FrameKind::Control) was the tempting home — but the two
    /// planes differ in exactly the way that matters here, and the difference is
    /// their FAILURE MODE against a reader that does not know the message. An
    /// unknown statement inside a control payload is ignored (`Control::Other`);
    /// an unknown kind byte returns `None` from [`FrameKind::from_byte`] and the
    /// reader refuses the frame by name. For every other out-of-band message,
    /// silent-ignore is the kinder default. For this one it is the bug: a gap
    /// dropped because the reader did not recognise it reproduces exactly the
    /// silence `model::Rec::Gap` exists to end.
    ///
    /// So the pairing rule is deliberate and asymmetric:
    ///
    /// * **new daemon + old extension** — fine. The old one never emits a gap
    ///   frame and its holes stay as invisible as they were. No regression.
    /// * **old daemon + new extension** — refuses the frame, loudly, by name.
    ///   Correct: that daemon cannot record gaps, and a run that silently
    ///   discarded them would be a run whose completeness nobody could later
    ///   question.
    Gap = 3,
    /// An Arrow IPC **stream** buffer holding one GEOMETRY frame — records that
    /// are drawable in a scene rather than samples on a channel: a point cloud,
    /// a transform-tree edge, detection boxes, an image. One row per record:
    /// `path · t_us · archetype · frame_id · format · bytes`, decoded by
    /// `record::wire::explode_geometry` straight into `Rec::Geometry`.
    ///
    /// Its own KIND for the same reason [`Gap`](FrameKind::Gap) has one: the
    /// failure mode against a reader that does not know it. Geometry inside a
    /// data frame would have to ride the `bytes` column of an explicit frame,
    /// where an old reader would happily land a point cloud as an opaque scalar
    /// blob — spooled, shipped, and drawn nowhere, with nothing saying why. A
    /// kind byte the reader does not know is refused by name at the first
    /// frame, which is the honest answer from a daemon that cannot draw it.
    Geometry = 4,
}

impl FrameKind {
    /// The byte on the wire.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// Inverse of [`FrameKind::as_byte`]; `None` for any other byte.
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(FrameKind::Data),
            2 => Some(FrameKind::Control),
            3 => Some(FrameKind::Gap),
            4 => Some(FrameKind::Geometry),
            _ => None,
        }
    }

    /// The four bytes every payload of this kind MUST begin with.
    ///
    /// This is the cheap half of the anti-hang rule: a reader checks it as soon
    /// as four payload bytes exist, so a length that survived the header but is
    /// nonsense costs four bytes rather than a wait for bytes that never come.
    ///
    /// * Data and Geometry — `FF FF FF FF`, the Arrow IPC continuation marker
    ///   that opens every IPC stream message.
    /// * Control — `{"` and then any two bytes; a control payload is a JSON
    ///   OBJECT, so its first byte is `{` and its second is `"` (an empty
    ///   object carries no statement and is refused as malformed anyway).
    /// * Gap — `{"` as well, for the same reason: the payload is a JSON object.
    ///   That is not a collision. The KIND BYTE is what discriminates the
    ///   planes — it is read before a payload byte is looked at — and this
    ///   prefix is only the cheap "is this payload obviously nonsense" check
    ///   the module note describes. Two kinds sharing a prefix costs nothing;
    ///   two kinds sharing a byte would be the ambiguity.
    pub const fn prefix(self) -> &'static [u8] {
        match self {
            FrameKind::Data | FrameKind::Geometry => &[0xFF, 0xFF, 0xFF, 0xFF],
            FrameKind::Control | FrameKind::Gap => b"{\"",
        }
    }
}

/// A decoded frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: FrameKind,
    /// Payload length in bytes. NOT a bound on anything — see the module note.
    pub len: u32,
}

impl FrameHeader {
    pub const fn new(kind: FrameKind, len: u32) -> Self {
        Self { kind, len }
    }

    /// The 12 header bytes, exactly as they go on the wire.
    pub fn encode(&self) -> [u8; FRAME_HEADER_BYTES] {
        let mut out = [0u8; FRAME_HEADER_BYTES];
        out[0..4].copy_from_slice(&FRAME_MAGIC);
        out[4] = FRAME_VERSION;
        out[5] = self.kind.as_byte();
        // out[6..8] reserved, zero — the extension point. A future field goes
        // here and old readers refuse rather than misread, because zero is
        // REQUIRED (below), not merely expected.
        out[8..12].copy_from_slice(&self.len.to_le_bytes());
        out
    }

    /// Parse 12 header bytes. Every rejection is terminal and names what was
    /// seen, because there is nothing safe to do with the rest of the stream.
    pub fn decode(buf: &[u8; FRAME_HEADER_BYTES]) -> Result<Self, WireFault> {
        if buf[0..4] != FRAME_MAGIC {
            return Err(WireFault::NotAFrame { head: buf[..].to_vec() });
        }
        if buf[4] != FRAME_VERSION {
            return Err(WireFault::Version { got: buf[4] });
        }
        let Some(kind) = FrameKind::from_byte(buf[5]) else {
            return Err(WireFault::Kind { got: buf[5] });
        };
        let reserved = u16::from_le_bytes([buf[6], buf[7]]);
        if reserved != 0 {
            return Err(WireFault::Reserved { got: reserved });
        }
        let len = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        Ok(Self { kind, len })
    }
}

/// Why an extension's stdout stream ended in a way the daemon must not treat as a
/// clean completion.
///
/// Every variant is terminal for the child and carries the evidence a user
/// needs to fix it. That matters more than the taxonomy: the baseline for this
/// work found the daemon killing a stream for a protocol violation, the extension
/// exiting 0 because its stdout had closed, and the pump reporting *clean end of
/// stream* — a green input card that had never produced a sample, with nothing
/// but a WARN in the daemon log. A named fault that reaches `exit_reason` is
/// what closes that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireFault {
    /// The bytes where a header was expected are not one. Almost always a
    /// extension that wrote to stdout directly — `println!`, a library banner, a
    /// vendor DLL's chatter. `head` is what was actually there.
    NotAFrame { head: Vec<u8> },
    /// Frame version byte is not [`FRAME_VERSION`].
    Version { got: u8 },
    /// Frame kind byte is not one this reader knows — see [`FrameKind`].
    ///
    /// This is the fault an OLD daemon raises against a NEW extension's gap
    /// frame, and raising it is the design (see [`FrameKind::Gap`]): a daemon
    /// that cannot record a hole must not quietly consume the statement of one.
    Kind { got: u8 },
    /// The reserved header field is not zero — a newer writer against an older
    /// reader. Refused rather than ignored: ignoring it is how a reader silently
    /// misreads a field it does not know about.
    Reserved { got: u16 },
    /// The payload's first bytes are not what its kind requires. A length that
    /// passed the header and then pointed at something that is not a payload.
    BadPayloadPrefix { kind: FrameKind, got: Vec<u8> },
    /// The stream ended inside a frame. `declared` is what the header promised,
    /// `got` what arrived before EOF.
    Truncated { declared: u32, got: usize },
    /// A frame arrived whole and its payload would not decode. Carries the
    /// decoder's own message — this is a real bug in an extension, not a transient.
    Payload { kind: FrameKind, detail: String },
    /// The read itself failed (pipe error).
    Io { detail: String },
}

impl std::fmt::Display for WireFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireFault::NotAFrame { head } => write!(
                f,
                "extension wrote non-frame bytes to stdout — expected a {:?} frame header, got {} \
                 (hex {}). Extension diagnostics belong on stderr; stdout is the data wire",
                std::str::from_utf8(&FRAME_MAGIC).unwrap_or("PHLM"),
                printable(head),
                hex(head),
            ),
            WireFault::Version { got } => {
                write!(f, "connector speaks frame version {got}, this daemon speaks {FRAME_VERSION}")
            }
            WireFault::Kind { got } => write!(f, "unknown frame kind byte {got}"),
            WireFault::Reserved { got } => {
                write!(f, "reserved header field is {got:#06x}, must be zero")
            }
            WireFault::BadPayloadPrefix { kind, got } => write!(
                f,
                "{kind:?} frame payload does not start like one — got hex {} (expected {})",
                hex(got),
                hex(kind.prefix()),
            ),
            WireFault::Truncated { declared, got } => write!(
                f,
                "stream ended inside a frame — header declared {declared} payload bytes, {got} arrived"
            ),
            WireFault::Payload { kind, detail } => {
                write!(f, "{kind:?} frame payload did not decode: {detail}")
            }
            WireFault::Io { detail } => write!(f, "connector stdout read failed: {detail}"),
        }
    }
}

impl std::error::Error for WireFault {}

/// How much of a stray write the fault message quotes. A print is recognised by
/// its words, not by twelve bytes of them — the reader collects up to this much
/// (see `control::stdio::Frames`) and this is where it is rendered.
const QUOTE_BYTES: usize = 96;

/// First bytes as text, control characters escaped — what a user needs to
/// recognise their own debug print. Stops at the first newline: a print's LINE
/// is the useful unit, and everything after it is a second accident.
fn printable(b: &[u8]) -> String {
    let line = b.split(|&c| c == b'\n').next().unwrap_or(b);
    let shown: String = line
        .iter()
        .take(QUOTE_BYTES)
        .flat_map(|&c| std::ascii::escape_default(c))
        .map(char::from)
        .collect();
    if line.len() > QUOTE_BYTES || line.len() < b.len() {
        format!("`{shown}`…")
    } else {
        format!("`{shown}`")
    }
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for &c in b.iter().take(24) {
        let _ = write!(s, "{c:02x}");
    }
    s
}

// ---------------------------------------------------------------------------
// The two time forms.

/// A sample **period** as an exact rational number of nanoseconds:
/// `num_ns / den`.
///
/// Nanoseconds and not microseconds because a period is not an instant — it may
/// legitimately be sub-microsecond (a 10 MHz digitiser's period is 100 ns), and
/// the time canon's µs rule governs serialized *instants*. Rational and not a
/// float because a frame's honesty rests on `t_k` being reproducible: 1/44100 s
/// is not representable in binary, and a drifting `t0 + k·(f64 period)` would
/// put the last sample of a 30-minute run tens of microseconds away from where
/// the producer thought it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Numerator, nanoseconds. Always > 0.
    pub num_ns: u64,
    /// Denominator. Always > 0.
    pub den: u64,
}

impl Step {
    /// A period of `num_ns / den` nanoseconds, reduced. `None` if either part is
    /// zero — a zero period would make every sample in a frame simultaneous and
    /// a zero denominator is not a number.
    ///
    /// **Also `None` below one nanosecond**, which is the same fault wearing a
    /// non-zero number. [`sample_instant`] places sample `k` at
    /// `k · num_ns / den` by FLOORED integer division on a nanosecond grid, so a
    /// period of, say, 1/100 ns puts the first hundred samples on the same
    /// instant — a hundred distinct readings claiming to have happened at once.
    /// Guarding only against zero caught the case nobody hits and let through
    /// the one that reached a rig: 100-sample frames landing as 100 rows on one
    /// timestamp, with ~98 distinct values in each, labelled with the producer's
    /// own clock so nothing downstream could tell.
    ///
    /// Refused rather than rounded up. This model's grid IS the nanosecond,
    /// so a producer claiming a finer period is claiming
    /// something no instant here can express; rounding would silently restate
    /// its rate as a different one. The fastest thing this tree samples is
    /// 10 MHz — a 100 ns period — so the bound costs nothing real and the
    /// refusal reaches the operator as "the device reported X Hz, which is not
    /// a usable rate" at task start, instead of as collapsed data days later.
    pub fn new(num_ns: u64, den: u64) -> Option<Self> {
        if num_ns == 0 || den == 0 || num_ns < den {
            return None;
        }
        let g = gcd(num_ns, den);
        Some(Self { num_ns: num_ns / g, den: den / g })
    }

    /// The period of a source running at `rate_num / rate_den` Hz.
    pub fn from_rate_hz(rate_num: u64, rate_den: u64) -> Option<Self> {
        // period_ns = 1e9 · rate_den / rate_num
        Step::new((NS_PER_S as u64).checked_mul(rate_den)?, rate_num)
    }

    /// The period for a rate that arrived as a float — a config field, a vendor
    /// API's `f64` sample rate.
    ///
    /// The float is turned into an EXACT rational first: the rate is scaled by
    /// successive powers of ten until it is integral, up to 10⁶ (µHz
    /// resolution, finer than any instrument states its rate). A rate that is
    /// still not integral there is used at that resolution — the alternative,
    /// carrying the f64's true binary rational, would encode the parse error of
    /// "100000.0" as if it were the device's rate.
    pub fn from_rate_hz_f64(hz: f64) -> Option<Self> {
        if !hz.is_finite() || hz <= 0.0 {
            return None;
        }
        let mut den: u64 = 1;
        for _ in 0..6 {
            let scaled = hz * den as f64;
            if scaled.fract() == 0.0 && scaled <= u64::MAX as f64 {
                return Step::from_rate_hz(scaled as u64, den);
            }
            den = den.checked_mul(10)?;
        }
        let scaled = (hz * den as f64).round();
        if scaled < 1.0 || scaled > u64::MAX as f64 {
            return None;
        }
        Step::from_rate_hz(scaled as u64, den)
    }

    /// Nominal rate in Hz — for logs and for a human-readable manifest echo.
    /// Never used in timing arithmetic.
    pub fn rate_hz(self) -> f64 {
        NS_PER_S as f64 * self.den as f64 / self.num_ns as f64
    }
}

const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// **The canonical `t_k`.** The instant of sample `k` (0-based) in a uniform
/// frame that starts at `t0` and steps by `step`.
///
/// Every consumer of a uniform frame calls THIS — the SDK when it reports where
/// a frame ends, the daemon when it expands one into samples, and the tests that
/// pin both. A second implementation anywhere is a second answer, and a uniform
/// frame's whole claim is that the omitted timestamps are not a matter of
/// opinion.
///
/// Exactness: `k · num_ns` is computed in `i128`, so a 30-minute run at 44.1 kHz
/// (`k ≈ 7.9 × 10⁷`, `num_ns = 10⁹`) is nowhere near overflow, and the result is
/// the exact floor of the rational instant — never an accumulation of rounded
/// increments. `k` is applied to `t0`, never to `t_{k-1}`, so error cannot
/// compound.
///
/// Saturates rather than wraps at the edges of `i64` ns; a frame that long is
/// past the representable epoch and the clamp is visible, not silent drift.
pub fn sample_instant(t0: Nanos, step: Step, k: u64) -> Nanos {
    let offset = (k as i128 * step.num_ns as i128) / step.den as i128;
    let t = t0.raw_ns() as i128 + offset;
    Nanos::from_raw(t.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
}

/// The instants `sample_instant(anchor, step, k0 + j)` for `j = 0, 1, …`,
/// WALKED: the period split once into whole nanoseconds plus an exact leftover
/// (`num_ns = q · den + r`), the leftover carried as a remainder that gives
/// back one nanosecond whenever it reaches `den`. No division in the loop, no
/// float, no drift — bit-identical to [`sample_instant`] for every `j`
/// (`walk_tests`). Starting at `k0` costs one division, once.
pub fn walk_instants(anchor: Nanos, step: Step, k0: u64) -> impl Iterator<Item = Nanos> {
    let den = step.den as u128;
    let q = (step.num_ns / step.den) as i128;
    let r = (step.num_ns % step.den) as u128;
    let start = k0 as i128 * step.num_ns as i128;
    let base = anchor.raw_ns() as i128;
    let mut off: i128 = start / step.den as i128;
    let mut rem: u128 = (start % step.den as i128) as u128;
    std::iter::repeat_with(move || {
        let t = Nanos::from_raw((base + off).clamp(i64::MIN as i128, i64::MAX as i128) as i64);
        off += q;
        rem += r;
        if rem >= den {
            rem -= den;
            off += 1;
        }
        t
    })
}

/// Which of the two time forms a frame declares.
///
/// The choice is **per frame**, not per extension: one acquisition task can hand a
/// extension a hardware-paced block and a software-polled status word in the same
/// second, and each deserves its own answer. The daemon never infers uniformity
/// from regular-looking stamps — a source that does not say `Uniform` does not
/// get the guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeForm {
    /// Hardware-paced. The frame carries `t0` and `step`; there is NO time
    /// column, and sample `k` of every channel is at
    /// [`sample_instant(t0, step, k)`](sample_instant).
    ///
    /// The whole of that claim rests on ONE invariant: **a uniform frame never
    /// spans a discontinuity.** An overrun, a re-arm, a driver restart — any gap
    /// at all — ENDS the frame, and the next one begins with a fresh `t0`. A
    /// producer that instead kept appending across a gap would be asserting
    /// samples at instants where nothing was measured, and nothing downstream
    /// could tell. `model::frame::Contiguity` is how a producer checks.
    Uniform { t0: Nanos, step: Step },
    /// Event-paced. Every row carries its own `t_us`, and a NULL there means
    /// "the daemon stamps me on arrival" — the honest shape for a source with no
    /// clock of its own.
    Explicit,
}

// ---------------------------------------------------------------------------
// Arrow schema metadata keys — the frame's self-description.
//
// They live here, next to the header, because they are part of the same
// contract: `crate::wire::payload` writes them and the daemon reads them, and neither is
// allowed a private spelling.

/// Schema metadata key: `"uniform"` | `"explicit"`.
pub const META_FORM: &str = "phloem.wire.form";
/// Value of [`META_FORM`] for a hardware-paced frame.
pub const FORM_UNIFORM: &str = "uniform";
/// Value of [`META_FORM`] for an event-paced frame.
pub const FORM_EXPLICIT: &str = "explicit";
/// Schema metadata key: the frame's [`crate::wire::TimeBasis`], lowercase.
pub const META_BASIS: &str = "phloem.wire.basis";
/// Schema metadata key: how far a `backdated` frame's anchor can sit from the
/// truth, in µs — the producer's own statement (see [`crate::wire::Clock`]).
///
/// Absent on every other basis, and absent is not zero: a frame that states
/// `backdated` without this is a producer that has not said, and the reader
/// must render that as unknown rather than as a perfect anchor. It is written
/// only by [`crate::wire::Clock::backdated`], so it cannot appear without the basis
/// that gives it meaning.
pub const META_ANCHOR_US: &str = "phloem.wire.anchor_us";
/// Schema metadata key: uniform `t0`, **epoch µs** (the canon's serialized
/// instant unit). Decimal string.
pub const META_T0_US: &str = "phloem.wire.t0_us";
/// Schema metadata key: uniform `t0`'s sub-microsecond remainder, ns in
/// `0..1000`. Decimal string; absent = 0.
///
/// It exists because the canon's µs rule is about instants on a wire, and a
/// 10 MHz digitiser's `t0` genuinely has a sub-µs part that truncating would
/// throw away — silently biasing an entire frame by up to 999 ns. The instant is
/// still carried in µs; this is the remainder, named as the remainder.
pub const META_T0_SUB_NS: &str = "phloem.wire.t0_sub_ns";
/// Schema metadata key: uniform step numerator, ns. Decimal string.
pub const META_STEP_NUM_NS: &str = "phloem.wire.step_num_ns";
/// Schema metadata key: uniform step denominator. Decimal string.
pub const META_STEP_DEN: &str = "phloem.wire.step_den";
/// FIELD metadata key on a uniform frame's `Int16`/`Int32` column: the scale
/// of the producer's exact linear map, `value = code · scale + offset`
/// Decimal string, finite and non-zero. Required: an integer
/// column with no map is a count or a state code and rides as `Int64`.
pub const META_SCALE: &str = "phloem.wire.scale";
/// FIELD metadata key: the offset of that map. Decimal string, finite.
pub const META_OFFSET: &str = "phloem.wire.offset";

#[cfg(test)]
mod tests {
    // ── a period must advance the clock ─────────────────────────────────────
    //
    // THE rig case, 2026-08-31: `/nidaqmx/Dev1/ai0` landed 100-sample frames as
    // 100 rows sharing ONE instant — 98-100 distinct readings each, spanning a
    // real signal range, so genuinely different measurements claiming to have
    // happened simultaneously. `Step` guarded against a zero period but not
    // against a sub-nanosecond one, and `sample_instant` floors, so every
    // sample below the first whole nanosecond collapsed onto `t0`.
    use super::{sample_instant, Step};
    use crate::time::Nanos;

    #[test]
    fn a_period_finer_than_the_grid_is_refused() {
        // 1/100 ns — the exact shape that collapsed 100 samples onto one instant.
        assert_eq!(Step::new(1, 100), None);
        // …and every other sub-nanosecond spelling of it.
        assert_eq!(Step::new(1, 2), None);
        assert_eq!(Step::new(999, 1000), None);
        // Exactly one nanosecond is the finest thing an instant here can say,
        // and it IS expressible, so it stands.
        assert!(Step::new(1, 1).is_some());
    }

    #[test]
    fn the_rate_constructors_refuse_it_too() {
        // A device reporting a rate this tree cannot place on its grid is
        // refused at the constructor, which is what turns collapsed data into
        // "not a usable rate" at task start.
        assert_eq!(Step::from_rate_hz(2_000_000_000, 1), None); // 0.5 ns
        assert_eq!(Step::from_rate_hz_f64(2e9), None);
        // 1 GHz is the boundary and is allowed; 10 MHz is the fastest thing
        // this tree actually samples.
        assert!(Step::from_rate_hz(1_000_000_000, 1).is_some());
        assert!(Step::from_rate_hz(10_000_000, 1).is_some());
    }

    #[test]
    fn consecutive_samples_never_share_an_instant() {
        // THE invariant the guard exists to hold, stated over every rate this
        // tree uses plus the boundary. Two adjacent samples of one frame must
        // land on different nanoseconds — if they can share one, a frame can
        // report simultaneous measurements that were not simultaneous.
        for hz in [1u64, 10, 1_000, 44_100, 100_000, 10_000_000, 1_000_000_000] {
            let step = Step::from_rate_hz(hz, 1).expect("a usable rate");
            for k in [0u64, 1, 2, 99, 100, 4095] {
                let a = sample_instant(Nanos::from_raw(0), step, k);
                let b = sample_instant(Nanos::from_raw(0), step, k + 1);
                assert!(
                    b.raw_ns() > a.raw_ns(),
                    "{hz} Hz: samples {k} and {} share instant {}",
                    k + 1,
                    a.raw_ns()
                );
            }
        }
    }

    use super::*;

    /// The header is a wire contract, so it is pinned byte for byte. A change
    /// here is a change every extension binary must be rebuilt for — this test is
    /// the place that says so.
    #[test]
    fn header_bytes_are_pinned() {
        let h = FrameHeader::new(FrameKind::Data, 0x0102_0304);
        assert_eq!(
            h.encode(),
            [
                b'P', b'H', b'L', b'M', // magic
                1,    // version
                1,    // kind = data
                0, 0, // reserved
                0x04, 0x03, 0x02, 0x01, // len, little-endian
            ]
        );
        assert_eq!(FRAME_HEADER_BYTES, 12);
        assert_eq!(FrameHeader::decode(&h.encode()).unwrap(), h);

        let c = FrameHeader::new(FrameKind::Control, 7);
        assert_eq!(c.encode()[5], 2);
        assert_eq!(FrameHeader::decode(&c.encode()).unwrap(), c);
    }

    /// Every rejection names what it saw. An extension author reading the Inputs
    /// error column must not have to guess which of these fired.
    #[test]
    fn every_header_rejection_is_named() {
        let mut b = FrameHeader::new(FrameKind::Data, 4).encode();

        let mut stray = b;
        stray[0..4].copy_from_slice(b"debu");
        assert!(matches!(FrameHeader::decode(&stray), Err(WireFault::NotAFrame { .. })));

        b[4] = 9;
        assert_eq!(FrameHeader::decode(&b), Err(WireFault::Version { got: 9 }));
        b[4] = FRAME_VERSION;

        b[5] = 77;
        assert_eq!(FrameHeader::decode(&b), Err(WireFault::Kind { got: 77 }));
        b[5] = FrameKind::Data.as_byte();

        b[6] = 1;
        assert_eq!(FrameHeader::decode(&b), Err(WireFault::Reserved { got: 1 }));
    }

    /// The message a user actually reads has to contain their own print.
    #[test]
    fn a_stray_println_is_diagnosable_from_the_message() {
        let f = WireFault::NotAFrame { head: b"debug: n=3\n\x00\x01".to_vec() };
        let msg = f.to_string();
        assert!(msg.contains("debug: n=3"), "{msg}");
        assert!(msg.contains("stderr"), "the message must say where diagnostics go: {msg}");
    }

    /// A length is not a promise the reader has to honour blindly: the payload
    /// prefix is checked in four bytes, so a header that passed with nonsense
    /// after it cannot make a reader wait.
    #[test]
    fn payload_prefixes_pin_the_two_planes_apart() {
        assert_eq!(FrameKind::Data.prefix(), &[0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(FrameKind::Control.prefix(), b"{\"");
        // Neither can be mistaken for the other, nor for text.
        assert_ne!(FrameKind::Data.prefix()[0], FrameKind::Control.prefix()[0]);
    }

    #[test]
    fn step_reduces_and_refuses_degenerate_periods() {
        assert_eq!(Step::new(2_000, 4).unwrap(), Step { num_ns: 500, den: 1 });
        assert_eq!(Step::new(0, 1), None);
        assert_eq!(Step::new(1, 0), None);
        // 100 kHz → 10 µs exactly.
        assert_eq!(Step::from_rate_hz(100_000, 1).unwrap(), Step { num_ns: 10_000, den: 1 });
        // 44.1 kHz → 1e9/44100 ns, reduced (gcd 100).
        assert_eq!(Step::from_rate_hz(44_100, 1).unwrap(), Step { num_ns: 10_000_000, den: 441 });
    }

    /// A float rate becomes an exact rational, not a float period.
    #[test]
    fn a_float_rate_becomes_an_exact_rational() {
        assert_eq!(
            Step::from_rate_hz_f64(100_000.0).unwrap(),
            Step::from_rate_hz(100_000, 1).unwrap()
        );
        assert_eq!(
            Step::from_rate_hz_f64(44_100.0).unwrap(),
            Step::from_rate_hz(44_100, 1).unwrap()
        );
        // 0.1 Hz — a slow poll, exactly 10 s, not 9.999999999 s.
        let s = Step::from_rate_hz_f64(0.1).unwrap();
        assert_eq!(sample_instant(Nanos::from_raw(0), s, 1), Nanos::from_raw(10_000_000_000));
        // 1000.5 Hz — exact at 10⁻¹ scaling.
        let s = Step::from_rate_hz_f64(1000.5).unwrap();
        assert_eq!(s, Step::from_rate_hz(10_005, 10).unwrap());
        assert_eq!(Step::from_rate_hz_f64(0.0), None);
        assert_eq!(Step::from_rate_hz_f64(f64::NAN), None);
        assert_eq!(Step::from_rate_hz_f64(-5.0), None);
    }

    /// `t_k` is computed from `k`, so 30 minutes of 44.1 kHz lands exactly where
    /// arithmetic says — the property that lets a uniform frame omit its
    /// timestamps at all. An accumulating `t += period` implementation is the
    /// comparison: it drifts by nearly a millisecond over the same run.
    #[test]
    fn t_k_is_exact_over_a_thirty_minute_run() {
        let step = Step::from_rate_hz(44_100, 1).unwrap();
        let t0 = Nanos::from_raw(1_700_000_000_000_000_000);
        let n: u64 = 44_100 * 30 * 60; // 79,380,000 samples

        // Exact reference, computed a different way: whole seconds first, then
        // the remainder — never touching the same expression under test.
        let secs = n / 44_100;
        let rem = n % 44_100;
        let want =
            t0.raw_ns() + secs as i64 * NS_PER_S + (rem as i128 * NS_PER_S as i128 / 44_100) as i64;
        assert_eq!(sample_instant(t0, step, n).raw_ns(), want);

        // And it is exact at every second boundary along the way.
        for s in [1u64, 7, 600, 1799, 1800] {
            let k = s * 44_100;
            assert_eq!(sample_instant(t0, step, k).raw_ns(), t0.raw_ns() + s as i64 * NS_PER_S);
        }
    }

    #[test]
    fn frame_kind_bytes_round_trip() {
        for k in [FrameKind::Data, FrameKind::Control, FrameKind::Gap, FrameKind::Geometry] {
            assert_eq!(FrameKind::from_byte(k.as_byte()), Some(k));
        }
        assert_eq!(FrameKind::Geometry.prefix(), FrameKind::Data.prefix(), "both are Arrow IPC");
        assert_eq!(FrameKind::from_byte(0), None);
        assert_eq!(FrameKind::from_byte(4), Some(FrameKind::Geometry));
        assert_eq!(FrameKind::from_byte(5), None);
    }

    /// The pairing rule of [`FrameKind::Gap`], as far as one process can
    /// witness it.
    ///
    /// **What this test cannot do:** it cannot run an OLD daemon binary, so it
    /// does not prove that the daemon shipped before this kind existed refuses
    /// a gap frame. What it pins is the property that rule rests on — that an
    /// unknown kind byte is REFUSED BY NAME rather than skipped — by asking the
    /// same question of a byte no version knows. The old-daemon half is an
    /// argument about `from_byte`'s totality, not an experiment; saying so here
    /// is better than letting a green test imply an experiment was run.
    #[test]
    fn an_unknown_kind_byte_is_refused_by_name_never_skipped() {
        let mut b = FrameHeader::new(FrameKind::Gap, 4).encode();
        assert_eq!(FrameHeader::decode(&b).unwrap().kind, FrameKind::Gap);
        b[5] = 200;
        assert_eq!(FrameHeader::decode(&b), Err(WireFault::Kind { got: 200 }));
        assert!(FrameHeader::decode(&b).unwrap_err().to_string().contains("200"));
    }

    /// A gap payload is a JSON object, so it shares Control's prefix — and the
    /// KIND is what tells them apart. This pins that the two facts coexist: the
    /// prefix check passes for both, and the header never confuses them.
    #[test]
    fn a_gap_frame_shares_the_control_prefix_and_not_its_kind() {
        assert_eq!(FrameKind::Gap.prefix(), FrameKind::Control.prefix());
        assert_ne!(FrameKind::Gap.as_byte(), FrameKind::Control.as_byte());
    }
}

#[cfg(test)]
mod walk_tests {
    use super::*;

    /// The walk is `sample_instant` sample for sample, from any start index,
    /// for steps that are and are not whole nanoseconds.
    #[test]
    fn the_walk_is_sample_instant_from_any_start() {
        let anchor = Nanos::from_raw(1_791_000_000_000_000_007);
        for step in [
            Step::new(78_125, 2).unwrap(),
            Step::new(78_125, 8).unwrap(),
            Step::new(1_000_000_000, 44_100).unwrap(),
            Step::new(1_000, 1).unwrap(),
        ] {
            for k0 in [0u64, 1, 7, 44_099, 1_000_003] {
                for (j, t) in walk_instants(anchor, step, k0).take(5_000).enumerate() {
                    assert_eq!(
                        t,
                        sample_instant(anchor, step, k0 + j as u64),
                        "{step:?} k0 {k0} j {j}"
                    );
                }
            }
        }
    }
}
