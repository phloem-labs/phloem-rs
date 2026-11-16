//! The golden-frame corpus: three streams the
//! Rust core writes for FIXED inputs, kept as bytes under `tests/golden/`, that
//! every binding must reproduce byte for byte. The three are the cases a
//! binding is most likely to get subtly wrong:
//!
//! - `uniform_gap.phlm` — a hello, a declaration, a uniform frame, then a gap
//!   ending it (the hole is stated at the frontier, not where data resumes).
//! - `explicit_null.phlm` — an explicit frame whose uniform sibling carries a
//!   NULL as a missing reading (a validity flag, not a shifted sample).
//! - `backdated.phlm` — a replayed recording: a backdated clock with its anchor.
//! - `ended.phlm` — a stream that ENDS by its own word: the end frame
//!   `Client::close` sends last (without it, the daemon reads the close as a
//!   cut and states the channels' outage, `producer_cut`).
//! - `hello_clock.phlm` — `ended`'s stream under a hello that names its clock
//!   (`{"clock":{"id":…}}`), the key the daemon keeps a clock's drift model under
//!   across reconnects.
//! - `uniform_i16.phlm` — a converter's 16-bit CODES with their maps: two
//!   Int16 columns, each with its own `code · scale + offset`, one
//!   with a hole, then the end.
//! - `uniform_i32.phlm` — the same for 32-bit codes, one Int32 column.
//!
//! `UPDATE_GOLDEN=1 cargo test -p phloem-sdk --test golden` rewrites the
//! corpus; the diff is the review. Byte identity holds because Arrow IPC
//! writes schema metadata in sorted key order and the batches carry no
//! timestamps of their own.

use phloem::{ChannelDecl, Column, Encoder, GapExtent, Step, TimeBasis, Value};
use std::path::PathBuf;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}

fn check(name: &str, bytes: Vec<u8>) {
    let path = golden_dir().join(name);
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &bytes).unwrap();
        return;
    }
    let want = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(
        want == bytes,
        "{name} drifted from the golden ({} vs {} bytes) — if the change is intended, \
         UPDATE_GOLDEN=1 and review the diff",
        bytes.len(),
        want.len()
    );
}

/// Case 1: a uniform frame ended by a gap.
pub fn uniform_gap() -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.hello("golden", "rust/golden").unwrap();
    e.declare(&[
        ChannelDecl::new("/rig/pressure").archetype("scalar").unwrap().unit("bar"),
        ChannelDecl::new("/rig/valve_open").archetype("scalar").unwrap(),
    ])
    .unwrap();
    let pressure: Vec<f64> = (0..8).map(|k| 10.0 + k as f64 * 0.25).collect();
    let valve: Vec<bool> = (0..8).map(|k| k >= 4).collect();
    e.uniform(
        1_700_000_000_000_000_000,
        Step::from_rate_hz(1000, 1).unwrap(),
        TimeBasis::Source,
        &[Column::f64("/rig/pressure", pressure), Column::bool("/rig/valve_open", valve)],
    )
    .unwrap();
    // Good data stopped after the 8th sample: frontier = t0 + 8 ms.
    e.gap(
        &["/rig/pressure".into(), "/rig/valve_open".into()],
        1_700_000_000_000_000_000 + 8_000_000,
        GapExtent::AtLeastSamples(12),
        "buffer overrun",
    )
    .unwrap();
    e.into_inner()
}

/// Case 2: a NULL as a missing reading, beside an explicit frame.
pub fn explicit_null() -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.hello("golden", "rust/golden").unwrap();
    e.uniform(
        1_700_000_000_000_000_000,
        Step::from_rate_hz(10, 1).unwrap(),
        TimeBasis::Host,
        &[
            Column::f64("/t/temp", vec![21.5, 21.6, f64::NAN, 21.8])
                .with_validity(vec![true, true, false, true]),
            Column::i64("/t/count", vec![1, 2, 3, 4]),
        ],
    )
    .unwrap();
    e.explicit(
        TimeBasis::Host,
        &[
            ("/t/mode".into(), Some(1_700_000_000_000_000_000), Value::Text("idle".into())),
            ("/t/mode".into(), Some(1_700_000_000_250_000_000), Value::Text("run".into())),
            ("/t/armed".into(), Some(1_700_000_000_250_000_000), Value::Bool(true)),
            ("/t/note".into(), None, Value::I64(7)),
        ],
    )
    .unwrap();
    e.into_inner()
}

/// Case 3: a replayed recording on a backdated clock.
pub fn backdated() -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.hello("golden", "rust/golden").unwrap();
    let basis = TimeBasis::Backdated { anchor_us: 1_700_000_100_000_000 };
    e.uniform(
        1_600_000_000_000_000_000,
        Step::from_rate_hz(50, 1).unwrap(),
        basis,
        &[Column::f64("/replay/x", vec![0.0, 0.5, 1.0])],
    )
    .unwrap();
    e.explicit(basis, &[("/replay/tag".into(), Some(1_600_000_000_020_000_000), Value::F64(2.5))])
        .unwrap();
    e.into_inner()
}

#[test]
fn uniform_gap_matches_the_golden() {
    check("uniform_gap.phlm", uniform_gap());
}

#[test]
fn explicit_null_matches_the_golden() {
    check("explicit_null.phlm", explicit_null());
}

/// Case 4: a stream that ends by its own word. f64 only, so the C ABI and Go
/// write every frame of it themselves.
pub fn ended() -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.hello("golden", "rust/golden").unwrap();
    e.uniform(
        1_700_000_000_000_000_000,
        Step::from_rate_hz(100, 1).unwrap(),
        TimeBasis::Source,
        &[Column::f64("/rig/flow", vec![1.0, 1.5, 2.0, 2.5])],
    )
    .unwrap();
    e.end().unwrap();
    e.into_inner()
}

/// Case 5: a hello that STATES its clock (`clock.id`), then the same f64
/// stream as `ended`. f64 only, so the C ABI and Go write every frame.
pub const GOLDEN_CLOCK_ID: &str = "usb:0x0683-SN-01A2B3C4";
pub fn hello_clock() -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.hello_with_clock("golden", "rust/golden", Some(GOLDEN_CLOCK_ID)).unwrap();
    e.uniform(
        1_700_000_000_000_000_000,
        Step::from_rate_hz(100, 1).unwrap(),
        TimeBasis::Source,
        &[Column::f64("/rig/flow", vec![1.0, 1.5, 2.0, 2.5])],
    )
    .unwrap();
    e.end().unwrap();
    e.into_inner()
}

/// Case 6: 16-bit converter codes with their maps. Codes only, so the C ABI
/// and Go write every frame of it themselves.
pub const I16_A_SCALE: f64 = 10.0 / 32768.0;
pub fn uniform_i16() -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.hello("golden", "rust/golden").unwrap();
    e.uniform(
        1_700_000_000_000_000_000,
        Step::from_rate_hz(10_000, 1).unwrap(),
        TimeBasis::Source,
        &[
            Column::i16("/adc/ai0", vec![-32768, -1, 0, 1, 32767], I16_A_SCALE, 0.0).unwrap(),
            Column::i16("/adc/ai1", vec![100, 200, 300, 400, 500], 0.001, -2.5)
                .unwrap()
                .with_validity(vec![true, true, false, true, true]),
        ],
    )
    .unwrap();
    e.end().unwrap();
    e.into_inner()
}

/// Case 7: 32-bit converter codes with their map.
pub fn uniform_i32() -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.hello("golden", "rust/golden").unwrap();
    e.uniform(
        1_700_000_000_000_000_000,
        Step::from_rate_hz(1000, 1).unwrap(),
        TimeBasis::Source,
        &[Column::i32("/adc/strain", vec![i32::MIN, -7, 0, 7, i32::MAX], 1e-6, -3.0).unwrap()],
    )
    .unwrap();
    e.end().unwrap();
    e.into_inner()
}

#[test]
fn uniform_i16_matches_the_golden() {
    check("uniform_i16.phlm", uniform_i16());
}

#[test]
fn uniform_i32_matches_the_golden() {
    check("uniform_i32.phlm", uniform_i32());
}

/// A map that is not one is refused at the call, and a column built around
/// the constructor is refused by the encoder with nothing written.
#[test]
fn a_code_column_without_a_map_is_refused_before_anything_is_written() {
    for (scale, offset) in [(0.0, 0.0), (f64::NAN, 0.0), (f64::INFINITY, 0.0), (1.0, f64::NAN)] {
        let e = Column::i16("/adc/x", vec![1], scale, offset).unwrap_err();
        assert!(e.to_string().contains("/adc/x"), "{e}");
        assert!(Column::i32("/adc/x", vec![1], scale, offset).is_err());
    }
    let mut e = Encoder::new(Vec::new());
    let bad = Column {
        path: "/adc/x".into(),
        values: phloem::ColumnValues::I16 { codes: vec![1, 2], scale: 0.0, offset: 0.0 },
        valid: None,
    };
    let r = e.uniform(0, Step::from_rate_hz(10, 1).unwrap(), TimeBasis::Source, &[bad]);
    assert!(matches!(r, Err(phloem::Error::Usage(_))), "{r:?}");
    assert!(e.into_inner().is_empty(), "nothing was written");
}

#[test]
fn hello_clock_matches_the_golden() {
    check("hello_clock.phlm", hello_clock());
    // Control: the same stream with the clock unstated is `ended`, not this.
    assert_ne!(hello_clock(), ended());
}

#[test]
fn ended_matches_the_golden() {
    check("ended.phlm", ended());
}

#[test]
fn backdated_matches_the_golden() {
    check("backdated.phlm", backdated());
}

#[test]
fn the_encoder_is_deterministic_across_runs() {
    assert_eq!(uniform_gap(), uniform_gap());
    assert_eq!(explicit_null(), explicit_null());
    assert_eq!(backdated(), backdated());
    assert_eq!(ended(), ended());
    assert_eq!(hello_clock(), hello_clock());
    assert_eq!(uniform_i16(), uniform_i16());
    assert_eq!(uniform_i32(), uniform_i32());
}
