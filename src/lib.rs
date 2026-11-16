//! `phloem-sdk` — send data to Phloem. Nothing else.
//!
//! One version for every language ([`VERSION`]).
//!
//! An SDK declares channels,
//! writes uniform or explicit frames, states which clock its stamps are on,
//! states gaps, and flushes. It never configures, deploys, actuates or
//! installs: a program that needs any of that is a connector (extension) or
//! an operator (the CLI), not an SDK.
//!
//! **Two routes, one surface.** [`Client::open`] sends through the daemon on
//! this machine when there is no API key — the daemon stamps, keeps and
//! uploads, and this program holds no credential — and straight to the cloud
//! when there is one (the `direct` feature: a CI job, a simulation, a
//! notebook, where there is no Phloem on the machine). The calls are the same.
//!
//! **Three layers.** `encode` turns calls into PHLM frames and does no I/O;
//! a transport takes the frames (`local`: the daemon's SDK door; `direct`:
//! the lake); [`Client`] is what a program holds. A build without `direct`
//! links no network code.
//!
//! **One wire.** The bytes are the PHLM frame wire an extension writes to its
//! stdout (the `wire` crate). On the local route they are sent
//! as they are, over the daemon's SDK door — `<state dir>/ingest.sock` on
//! Unix, the named pipe `\\.\pipe\phloemd-ingest-<id>` on Windows
//! ([`door_in`]; `/status ingest_channel` names it). On the direct route each
//! frame is read back with the decoder the daemon reads the door with and
//! uploaded as the records the daemon would have made of it. The encoders
//! here ARE the shared `wire` crate — the default build links it and `time`,
//! no other Phloem crate; this crate adds the routes and a surface small
//! enough to bind to Python, C, C++ and Go without re-deriving anything.
//!
//! **Time is stated, never defaulted.** Every frame carries a [`TimeBasis`]:
//! `Source` (the stamps are the producer's own clock), `Host` (they are this
//! machine's wall clock), or `Backdated { anchor_us }` (a recording replayed
//! later, with the instant the replay began). The daemon treats an unstated
//! basis as a producer fault, so the API cannot express one.
//!
//! **A fixed-rate stream cannot step backwards.** A uniform frame carries no
//! timestamps, so one that starts before the previous frame for the same
//! channel ended re-claims stored instants; the daemon keeps that head out and
//! states it as a hole. The encoder refuses to write one ([`Error::Overlap`]),
//! continuing a host-clocked stream that merely woke early, and
//! [`UniformStream`] is the sender that cannot get it wrong: it anchors once
//! and places every frame where the last one ended.
//!
//! **Byte-identical bindings.** The golden corpus (`tests/golden/`) is
//! produced by [`Encoder`] alone and every binding must reproduce it byte for
//! byte (`tests/golden.rs`): a gap ending a uniform frame, a null as a missing
//! reading, a backdated clock, a stream that ends by its own word.
//!
//! **A stream ends by its own word.** [`Client::close`] sends the end frame
//! ([`Encoder::end`], Control `{"end":{}}`) before it closes the connection.
//! The daemon reads a connection that closes WITHOUT it — the program was killed
//! or crashed between frames, its link dropped — as a cut, and states the
//! outage of every channel the connection delivered (`producer_cut`). A
//! dropped [`Client`] flushes but does not end: a client dropped by a panic
//! unwinding did not finish, and saying it did would hide that.

/// This SDK's version: the one version every language's SDK is released at.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The frame wire — what this SDK and Phloem on the machine both speak.
pub mod wire;
/// Unit-typed instants and durations.
pub mod time;

mod client;
mod encode;
mod local;
mod stream;

pub use client::{Client, Options, Route};
pub use encode::{
    ChannelDecl, Column, ColumnValues, Encoder, Error, FrameSink, GapExtent, Step, TimeBasis, Value,
};
pub use local::{default_socket_path, default_state_dir, door_in};
pub use stream::{Pushed, UniformStream, DRIFT_TOLERANCE_NS};
pub use crate::wire::frame::FrameKind;
