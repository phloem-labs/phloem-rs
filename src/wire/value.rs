//! **The wire's scalar value** — what one sample of an explicit frame carries,
//! and the one rule by which a two-state value is a number.

use serde::{Deserialize, Serialize};

/// A typed value carried by a channel sample. (Scalars + blob for v0;
/// Arrow-backed multimodal components come later in `bus`/`query`.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    F64(f64),
    I64(i64),
    Bool(bool),
    Text(String),
    Bytes(Vec<u8>),
    /// **The sample happened; the value is absent.** A reading the source
    /// reported but that could not be turned into an honest value — a
    /// calibration with no valid result, a non-finite reading, an element the
    /// producer marked invalid.
    ///
    /// It exists so that absence and non-occurrence stop being the same bytes.
    /// Deleting such a sample instead leaves a hole indistinguishable from a
    /// sensor that never reported — and on a test record those are opposite
    /// diagnoses (a bad conversion vs. a dead channel). The extension wire has
    /// always said this: "a non-finite or missing reading is a NULL in the
    /// validity mask, never a dropped element". This is the in-process form of
    /// that same rule, so the daemon can keep the promise its own wire makes.
    ///
    /// It is NOT a value: it is never numeric ([`as_f64`](Self::as_f64) is
    /// `None`), never plots as zero, and lands as a row whose typed columns are
    /// all null — the timestamp survives, the reading does not.
    ///
    /// Declared LAST deliberately: the spool's bincode encoding is positional,
    /// so appending keeps every existing variant index where it was.
    ///
    /// **Why it carries a payload it does not need.** Serde writes a UNIT
    /// variant as the bare string `"null"`, while every other variant is an
    /// object (`{"f64": 1.0}`). The renderer tests membership with `"f64" in
    /// v`, and in JavaScript that THROWS on a string primitive rather than
    /// returning false — so a unit variant would crash the live chart path the
    /// first time a calibration failed. The `()` keeps the wire shape an
    /// object, `{"null": null}`, which every existing reader already falls
    /// through on safely.
    Null {},
}

impl Value {
    /// THE numeric view of a sample — `f64`, `i64` as f64, `bool` as 0/1
    /// ([`bool_as_f64`]). Every numeric reader of a sample goes
    /// through here (derivations, detectors, calibration, history, exports,
    /// CEL's numeric operators).
    ///
    /// [`Null`](Self::Null) is `None` — the same answer as text, and for the
    /// same reason: there is no number here. A caller that substitutes 0.0 for
    /// `None` is inventing a reading.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::F64(v) => Some(*v),
            Value::I64(v) => Some(*v as f64),
            Value::Bool(v) => Some(bool_as_f64(*v)),
            _ => None,
        }
    }

    /// True when the sample occurred but carries no value.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null {})
    }
}

/// THE number a two-state value is: `1.0` on (`true`), `0.0` off (`false`).
/// Every numeric reader of a bool — a sample ([`Value::as_f64`]), an
/// Arrow `BooleanArray`, an export cell — goes through here.
#[inline]
pub fn bool_as_f64(on: bool) -> f64 {
    if on {
        1.0
    } else {
        0.0
    }
}
