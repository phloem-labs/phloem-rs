//! **The Phloem frame wire** — the format a program (the SDK), an extension,
//! the daemon and the cloud all speak.
//!
//! The rule of this crate: **it IS the frame wire. If a frame's bytes or a
//! control frame's fields change, they change here and nowhere else.** The
//! golden corpus (`*.phlm`) is the byte-exact contract every binding
//! and the daemon's acceptance test reproduce.
//!
//! What is here:
//!
//! * [`frame`] — the 12-byte envelope ([`frame::FrameHeader`],
//!   [`frame::FrameKind`]), the terminal faults ([`frame::WireFault`]), a
//!   uniform frame's exact period ([`frame::Step`]) and THE canonical
//!   `t_k = t0 + k·step` ([`frame::sample_instant`]), the two time forms and
//!   the schema-metadata keys a frame describes itself with.
//! * [`TimeBasis`], [`Clock`] — which clock a frame's instants are on, and how
//!   well it is known.
//! * [`Value`] — the scalar value an explicit frame's row carries.
//! * [`gap`] — a gap frame's payload ([`gap::GapStatement`]).
//! * [`control`] — the control frames of the SDK door (hello, declare, end,
//!   and the daemon's welcome / refused), one definition for both sides.
//! * [`payload`] (feature `payload`) — framing I/O and the Arrow ENCODERS of a
//!   data frame, both time forms, with the frame's self-description
//!   ([`payload::frame_meta`]).
//! * [`door`] (feature `door`) — the names of the daemon's local doors on Windows.
//!
//! What is NOT here, because it needs the data model: the decode of a data
//! frame to samples (`record::wire::decode_data`, which returns
//! `model::Batch` / `model::series::UniformSlice`) and geometry frames
//! (`record::wire::GeometryFrame`, on `model::Rec` / `model::Archetype`).
//!
//! Dependencies: `time` (the leaf unit crate), serde, serde_json — plus arrow,
//! arrow-ipc and anyhow under `payload` (so `model`, which re-exports the
//! envelope and the vocabulary, stays serde-only) — and on no other Phloem
//! crate. `model` and `record` re-export everything here at the paths it had
//! before it moved, so the direction is `wire` ← `model` ← `record` and nothing
//! points back.

pub mod basis;
pub mod clock;
pub mod control;
pub mod door;
pub mod frame;
pub mod gap;
pub mod payload;
pub mod value;

pub use basis::TimeBasis;
pub use clock::Clock;
pub use value::{bool_as_f64, Value};
