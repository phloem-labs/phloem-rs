# Phloem SDK for Rust

[![crates.io](https://img.shields.io/crates/v/phloem-sdk.svg)](https://crates.io/crates/phloem-sdk)
[![docs.rs](https://img.shields.io/docsrs/phloem-sdk)](https://docs.rs/phloem-sdk)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Send readings from Rust to [Phloem](https://phloem.ai). Each reading becomes a
named channel you can watch live, replay, chart and check, the same as one
from a connector.

```rust
use phloem::{Client, Options, Value};

fn main() -> Result<(), phloem::Error> {
    let mut c = Client::open(Options::named("hil-runner"))?;
    c.log_now("/rig/psi", Value::F64(3.2))?;
    c.close() // fails if anything was not delivered
}
```

## Install

```toml
[dependencies]
phloem-sdk = "0.1.0"
```

The crate is `phloem-sdk`; the code imports it as `phloem`.

### Requirements

Rust 1.85 or later, on Linux, macOS or Windows. Phloem Computer or the Phloem
service on the same machine.

Run the examples in this repository with `cargo run --example quickstart`.

## Where the data goes

This crate sends through **Phloem on this machine**: Phloem Computer, or the
Phloem service on a server (`sudo phloem install`). The program is allowed
once on **Inputs**; until then the connection is refused with a message that
says how. If the network drops, nothing is lost: Phloem keeps the data on
disk and uploads it later.

Sending straight to the cloud with an API key is in the Python, Go, C and C++
SDKs and is coming to this crate.

## Sending

```rust
use phloem::{Client, Column, GapExtent, Options, Step, TimeBasis, UniformStream, Value};

fn main() -> Result<(), phloem::Error> {
    let mut c = Client::open(Options::named("bench-pc"))?;

    // one reading at its own time, on the device's clock
    let t0: i64 = 1_700_000_000_000_000_000;
    c.log("/rig/mode", t0, TimeBasis::Source, Value::Text("fire".into()))?;

    // many readings of one channel
    c.log_many("/rig/psi", TimeBasis::Source, &[t0, t0 + 1_000_000], &[3.1, 3.2])?;

    // a fixed-rate signal: each block starts where the last one ended
    let step = Step::from_rate_hz(1000, 1).expect("a non-zero rate");
    let mut s = UniformStream::new(["/daq/ch0"], step, TimeBasis::Host)?;
    c.push(&mut s, &[Column::f64("/daq/ch0", vec![0.0; 1000])])?;

    // say that data is missing from a point
    c.gap(&["/daq/ch0".to_string()], t0, GapExtent::AtLeastSamples(12), "overrun")?;

    c.close()
}
```

| Call | What it does |
|---|---|
| `Client::open(Options::named(name))` | connect to Phloem on this machine |
| `log_now(path, value)` | one reading, stamped with this machine's clock |
| `log(path, t_ns, basis, value)` | one reading at its own time |
| `log_many(path, basis, times_ns, values)` | many readings of one channel |
| `uniform(t0_ns, step, basis, columns)` | a block of evenly spaced samples for several channels |
| `UniformStream::new(paths, step, basis)` then `push(&mut stream, columns)` | a fixed-rate sender |
| `explicit(basis, samples)` | mixed readings, each with its own time |
| `gap(paths, frontier_ns, extent, detail)` | state that data is missing |
| `declare(channels)` | a channel's unit and how to draw it |
| `flush()` | send what is held now |
| `close()` | end the stream; returns an error if anything was not delivered |

**Values:** `Value::F64`, `I64`, `Bool`, `Text`, `Bytes`, `Null`. **Paths** name
channels: `/rig/psi`.

**Timestamps** always say which clock they are on: `TimeBasis::Host` (this
machine's), `TimeBasis::Source` (your device's or recording's own) or
`TimeBasis::Backdated { anchor_us }` (a recording replayed now). There is no
default.

**Options:** `clock_id` names the clock your timestamps come from (a device
serial), so Phloem keeps what it learned about that clock across restarts.

**Version:** `phloem::VERSION`. Every language's SDK is released at the same
version.

## Documentation

https://phloem.ai/docs/sdk/ · what changed in each version: `CHANGELOG.md`.

## Support

Found a bug or need something? [Open an issue](https://github.com/phloem-labs/phloem-rs/issues).

## Licence

Apache License 2.0. See `LICENSE`.
