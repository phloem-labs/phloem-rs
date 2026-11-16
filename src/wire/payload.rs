//! The extension stdout wire's **payload** — Arrow IPC in both time forms.
//!
//! [`crate::wire::frame`] owns the envelope (12 header bytes, the terminal faults, the
//! canonical `t_k`). This module owns what rides inside a
//! [`FrameKind::Data`](crate::wire::frame::FrameKind::Data) frame: an Arrow IPC stream
//! buffer holding exactly one frame, and the code that builds and takes apart
//! one. Producer and consumer share it, so there is one schema, not two
//! implementations of the same idea that drift.
//!
//! # Two forms, two shapes — and why they are not one shape
//!
//! **Uniform is WIDE.** One column per channel, no time column at all, `t_k`
//! recovered from the frame's `t0` and `step`. An f64 sample costs 8 bytes and
//! nothing else. That is the whole point of the form: a 16-channel board at
//! 100 kS/s/ch is 12.8 MB/s of payload instead of 98 MB/s of JSON text, and the
//! reader does no per-sample work beyond a slice read.
//!
//! **Explicit is LONG (tidy).** `path`, a nullable `t_us`, and one nullable
//! column per [`crate::wire::Value`] variant — the same shape the lake's `scalars`
//! table uses. It is the honest shape for event sources, because they are not
//! rectangular: a Modbus poll reads register A at one instant and register B at
//! another, and a wide frame would have to invent a shared instant (or a column
//! of holes) to say so.
//!
//! Forcing one shape on both would cost the uniform form its whole advantage
//! (a time column doubles it) or cost the explicit form its honesty (a shared
//! time axis it does not have). So: two shapes, one envelope, and the form is
//! stated in schema metadata rather than sniffed.
//!
//! # Nulls
//!
//! A sample that is non-finite, or that the device reported as missing, becomes
//! a **NULL in the validity mask** — never a dropped element. In a uniform frame
//! that distinction is load-bearing: element `k` IS the sample at `t_k`, so
//! dropping one silently re-dates every sample after it. (The nidaqmx driver
//! used to drop non-finite readings from its output vector, which did exactly
//! that.)
//!
//! On the way back out, `record::wire::explode` CARRIES a null through as
//! [`crate::wire::Value::Null`] and counts it. It used to omit them: the instants of
//! the survivors stayed correct (they come from `k`), so nothing was re-dated —
//! but the hole itself was erased, and a channel reading "open thermocouple" at
//! 100 kHz arrived downstream indistinguishable from a channel that was not
//! reporting at all. The wire had said "no reading here" and the daemon had no
//! word for it; now it does, and the statement survives to the lake as a row
//! whose typed columns are all null.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{
    BinaryBuilder, BooleanBuilder, Float64Builder, Int16Builder, Int32Builder, Int64Builder,
    RecordBatch, StringBuilder, TimestampMicrosecondBuilder,
};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use crate::time::{DurationNs, Micros, Nanos};

use crate::wire::frame::{
    FrameHeader, FrameKind, Step, TimeForm, WireFault, FORM_EXPLICIT, FORM_UNIFORM, META_ANCHOR_US,
    META_BASIS, META_FORM, META_OFFSET, META_SCALE, META_STEP_DEN, META_STEP_NUM_NS,
    META_T0_SUB_NS, META_T0_US,
};
use crate::wire::{Clock, TimeBasis, Value};

// ---------------------------------------------------------------------------
// Framing I/O — the envelope around an Arrow buffer.

/// Write one frame: 12 header bytes then the payload. Does NOT flush — the
/// caller decides where a flush belongs (once per burst, not once per frame).
///
/// Refuses a payload larger than `u32::MAX`, which is not a cap on data but the
/// range of the length field: a producer with more than 4 GiB in one frame has
/// to split it, and would have to anyway to be read incrementally.
pub fn write_frame<W: std::io::Write>(
    w: &mut W,
    kind: FrameKind,
    payload: &[u8],
) -> std::io::Result<()> {
    let len = u32::try_from(payload.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("frame payload {} bytes exceeds the u32 length field", payload.len()),
        )
    })?;
    debug_assert!(
        payload.len() < kind.prefix().len() || payload.starts_with(kind.prefix()),
        "a {kind:?} payload must start with its prefix or no reader will accept it"
    );
    w.write_all(&FrameHeader::new(kind, len).encode())?;
    w.write_all(payload)
}

/// Encode a `RecordBatch` as an Arrow IPC **stream** buffer — the exact bytes a
/// [`FrameKind::Data`] payload carries.
pub fn encode_payload(batch: &RecordBatch) -> anyhow::Result<Vec<u8>> {
    encode_ipc(batch)
}

/// Decode a [`FrameKind::Data`] payload back to its single `RecordBatch`.
///
/// A data frame holds exactly one batch. Zero or several means the producer is
/// not building frames the way this module does, which is a bug worth naming
/// rather than a shape to tolerate.
pub fn decode_payload(payload: &[u8]) -> Result<RecordBatch, WireFault> {
    decode_one(payload, FrameKind::Data)
}

/// The one-batch rule, for every Arrow-carrying kind: a frame holds exactly
/// one record batch, and the fault names the kind it was read as.
pub fn decode_one(payload: &[u8], kind: FrameKind) -> Result<RecordBatch, WireFault> {
    let mut batches = decode_ipc(payload)
        .map_err(|e| WireFault::Payload { kind, detail: format!("arrow ipc: {e}") })?;
    match batches.len() {
        1 => Ok(batches.remove(0)),
        n => Err(WireFault::Payload {
            kind,
            detail: format!("a {kind:?} frame carries exactly one record batch, got {n}"),
        }),
    }
}

// ---------------------------------------------------------------------------
// Uniform frames — wide, no time column.

/// One channel's samples in a uniform frame, borrowed from the producer's own
/// read buffer.
#[derive(Debug, Clone, Copy)]
pub enum UniformValues<'a> {
    /// Non-finite elements (NaN, ±inf) land as NULL. A DAQ that hands back a
    /// NaN for an open thermocouple is stating "no reading here", and that is
    /// what a null column entry means — dropping the element instead would
    /// re-date every later sample in the frame.
    F64(&'a [f64]),
    I64(&'a [i64]),
    Bool(&'a [bool]),
    /// A converter's codes and its exact linear map, `value = code · scale +
    /// offset`: 2 bytes a sample on the wire and in the log
    /// instead of 8, the value still exact.
    I16 {
        codes: &'a [i16],
        scale: f64,
        offset: f64,
    },
    I32 {
        codes: &'a [i32],
        scale: f64,
        offset: f64,
    },
}

impl UniformValues<'_> {
    pub fn len(&self) -> usize {
        match self {
            UniformValues::F64(v) => v.len(),
            UniformValues::I64(v) => v.len(),
            UniformValues::Bool(v) => v.len(),
            UniformValues::I16 { codes, .. } => codes.len(),
            UniformValues::I32 { codes, .. } => codes.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One column of a uniform frame.
#[derive(Debug, Clone, Copy)]
pub struct UniformColumn<'a> {
    /// The channel path — it IS the Arrow column name.
    pub path: &'a str,
    pub values: UniformValues<'a>,
    /// Per-sample validity. `false` = the device reported a hole at that
    /// instant. `None` = every sample present (the ordinary case; non-finite
    /// f64 is still nulled).
    pub valid: Option<&'a [bool]>,
}

impl<'a> UniformColumn<'a> {
    pub fn f64(path: &'a str, values: &'a [f64]) -> Self {
        Self { path, values: UniformValues::F64(values), valid: None }
    }
    pub fn i64(path: &'a str, values: &'a [i64]) -> Self {
        Self { path, values: UniformValues::I64(values), valid: None }
    }
    pub fn bool(path: &'a str, values: &'a [bool]) -> Self {
        Self { path, values: UniformValues::Bool(values), valid: None }
    }
    /// 16-bit codes and the exact map to their value (`code · scale + offset`).
    pub fn i16(path: &'a str, codes: &'a [i16], scale: f64, offset: f64) -> Self {
        Self { path, values: UniformValues::I16 { codes, scale, offset }, valid: None }
    }
    /// 32-bit codes and the exact map to their value.
    pub fn i32(path: &'a str, codes: &'a [i32], scale: f64, offset: f64) -> Self {
        Self { path, values: UniformValues::I32 { codes, scale, offset }, valid: None }
    }
    /// Attach an explicit validity mask — for holes the device reported, which
    /// no value inspection could find.
    pub fn with_validity(mut self, valid: &'a [bool]) -> Self {
        self.valid = Some(valid);
        self
    }
}

/// Write a [`Clock`] into a frame's schema metadata: always the vocabulary
/// word, and the bound only when there is one.
///
/// One function for both frame forms, because the pair is one fact: a form that
/// wrote the basis and forgot the bound would publish a back-dated frame whose
/// error is unknowable, and the whole point of `Clock` is that those two cannot
/// come apart.
pub fn write_clock(meta: &mut HashMap<String, String>, clock: Clock) {
    meta.insert(META_BASIS.to_string(), clock.basis().as_str().to_string());
    if let Some(us) = clock.anchor_us() {
        meta.insert(META_ANCHOR_US.to_string(), us.to_string());
    }
}

/// Build the `RecordBatch` for a uniform frame.
///
/// Every column must be the same length (they are samples at the same
/// instants — that is what "uniform" asserts) and paths must be distinct.
pub fn uniform_batch(
    t0: Nanos,
    step: Step,
    clock: Clock,
    columns: &[UniformColumn<'_>],
) -> anyhow::Result<RecordBatch> {
    if columns.is_empty() {
        anyhow::bail!("a uniform frame needs at least one channel");
    }
    let rows = columns[0].values.len();
    for c in columns {
        if c.values.len() != rows {
            anyhow::bail!(
                "uniform column `{}` has {} samples, `{}` has {rows} — a uniform frame's \
                 channels are sampled at the SAME instants, so their lengths must match",
                c.path,
                c.values.len(),
                columns[0].path
            );
        }
        if let UniformValues::I16 { scale, offset, .. } | UniformValues::I32 { scale, offset, .. } =
            c.values
        {
            if !(scale.is_finite() && scale != 0.0 && offset.is_finite()) {
                anyhow::bail!(
                    "uniform column `{}` states the map code · {scale} + {offset} — a scale \
                     must be finite and non-zero, an offset finite",
                    c.path
                );
            }
        }
        if let Some(v) = c.valid {
            if v.len() != rows {
                anyhow::bail!(
                    "uniform column `{}` validity mask is {} long, values are {rows}",
                    c.path,
                    v.len()
                );
            }
        }
    }
    {
        let mut seen = std::collections::BTreeSet::new();
        for c in columns {
            if !seen.insert(c.path) {
                anyhow::bail!(
                    "uniform frame names channel `{}` twice — one column per channel",
                    c.path
                );
            }
        }
    }

    let mut fields = Vec::with_capacity(columns.len());
    let mut arrays: Vec<arrow::array::ArrayRef> = Vec::with_capacity(columns.len());
    for c in columns {
        let present = |i: usize| c.valid.is_none_or(|v| v[i]);
        let (dt, arr): (DataType, arrow::array::ArrayRef) = match c.values {
            UniformValues::F64(vs) => {
                let mut b = Float64Builder::with_capacity(rows);
                for (i, v) in vs.iter().enumerate() {
                    // The two null sources meet here: a hole the device
                    // declared, and a value that is not a number.
                    if present(i) && v.is_finite() {
                        b.append_value(*v);
                    } else {
                        b.append_null();
                    }
                }
                (DataType::Float64, Arc::new(b.finish()))
            }
            UniformValues::I64(vs) => {
                let mut b = Int64Builder::with_capacity(rows);
                for (i, v) in vs.iter().enumerate() {
                    if present(i) {
                        b.append_value(*v);
                    } else {
                        b.append_null();
                    }
                }
                (DataType::Int64, Arc::new(b.finish()))
            }
            UniformValues::Bool(vs) => {
                let mut b = BooleanBuilder::with_capacity(rows);
                for (i, v) in vs.iter().enumerate() {
                    if present(i) {
                        b.append_value(*v);
                    } else {
                        b.append_null();
                    }
                }
                (DataType::Boolean, Arc::new(b.finish()))
            }
            UniformValues::I16 { codes, .. } => {
                let mut b = Int16Builder::with_capacity(rows);
                for (i, v) in codes.iter().enumerate() {
                    if present(i) {
                        b.append_value(*v);
                    } else {
                        b.append_null();
                    }
                }
                (DataType::Int16, Arc::new(b.finish()))
            }
            UniformValues::I32 { codes, .. } => {
                let mut b = Int32Builder::with_capacity(rows);
                for (i, v) in codes.iter().enumerate() {
                    if present(i) {
                        b.append_value(*v);
                    } else {
                        b.append_null();
                    }
                }
                (DataType::Int32, Arc::new(b.finish()))
            }
        };
        let field = Field::new(c.path, dt, true);
        let field = match c.values {
            UniformValues::I16 { scale, offset, .. } | UniformValues::I32 { scale, offset, .. } => {
                field.with_metadata(HashMap::from([
                    (META_SCALE.to_string(), scale.to_string()),
                    (META_OFFSET.to_string(), offset.to_string()),
                ]))
            }
            _ => field,
        };
        fields.push(field);
        arrays.push(arr);
    }

    let t0_us = t0.to_micros_trunc();
    let sub_ns = t0.raw_ns() - t0_us.to_nanos().raw_ns();
    let mut meta = HashMap::new();
    meta.insert(META_FORM.to_string(), FORM_UNIFORM.to_string());
    write_clock(&mut meta, clock);
    meta.insert(META_T0_US.to_string(), t0_us.raw_us().to_string());
    if sub_ns != 0 {
        meta.insert(META_T0_SUB_NS.to_string(), sub_ns.to_string());
    }
    meta.insert(META_STEP_NUM_NS.to_string(), step.num_ns.to_string());
    meta.insert(META_STEP_DEN.to_string(), step.den.to_string());

    Ok(RecordBatch::try_new(Arc::new(Schema::new(fields).with_metadata(meta)), arrays)?)
}

// ---------------------------------------------------------------------------
// Explicit frames — long/tidy, per-row t_us.

/// The long-form schema an explicit frame uses: `path`, a NULLABLE `t_us`, and
/// one nullable column per [`Value`] variant.
///
/// `t_us` nullable is the "stamp me on arrival" signal, and it is a real null
/// rather than a sentinel because a `0` there was indistinguishable from a
/// broken RTC reading 1970 and from a genuine epoch-0 stamp.
pub fn explicit_schema(clock: Clock) -> SchemaRef {
    let mut meta = HashMap::new();
    meta.insert(META_FORM.to_string(), FORM_EXPLICIT.to_string());
    write_clock(&mut meta, clock);
    Arc::new(
        Schema::new(vec![
            Field::new("path", DataType::Utf8, false),
            Field::new(
                "t_us",
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                true,
            ),
            Field::new("f64", DataType::Float64, true),
            Field::new("i64", DataType::Int64, true),
            Field::new("bool", DataType::Boolean, true),
            Field::new("text", DataType::Utf8, true),
            Field::new("bytes", DataType::Binary, true),
        ])
        .with_metadata(meta),
    )
}

/// Accumulates rows for an explicit frame. Appends straight into Arrow
/// builders, so a sample never lands in an intermediate `Vec<Sample>`.
pub struct ExplicitFrame {
    clock: Clock,
    path: StringBuilder,
    t_us: TimestampMicrosecondBuilder,
    f64: Float64Builder,
    i64: Int64Builder,
    bool: BooleanBuilder,
    text: StringBuilder,
    bytes: BinaryBuilder,
    rows: usize,
}

impl ExplicitFrame {
    pub fn new(clock: Clock) -> Self {
        Self {
            clock,
            path: StringBuilder::new(),
            t_us: TimestampMicrosecondBuilder::new(),
            f64: Float64Builder::new(),
            i64: Int64Builder::new(),
            bool: BooleanBuilder::new(),
            text: StringBuilder::new(),
            bytes: BinaryBuilder::new(),
            rows: 0,
        }
    }

    /// One sample. `t` of `None` = "the daemon stamps this on arrival".
    ///
    /// A non-finite f64 lands as a NULL value with its timestamp intact — the
    /// sample happened, the reading is not a number, and both facts survive.
    pub fn push(&mut self, path: &str, t: Option<Micros>, value: &Value) {
        self.path.append_value(path);
        match t {
            Some(us) => self.t_us.append_value(us.raw_us()),
            None => self.t_us.append_null(),
        }
        let mut f = None;
        let mut i = None;
        let mut b = None;
        let mut s = None;
        let mut y = None;
        match value {
            Value::F64(v) => f = v.is_finite().then_some(*v),
            Value::I64(v) => i = Some(*v),
            Value::Bool(v) => b = Some(*v),
            Value::Text(v) => s = Some(v.as_str()),
            Value::Bytes(v) => y = Some(v.as_slice()),
            // The sample occurred and carries no value: every typed column
            // stays null, the timestamp stands. Byte-identical to how a
            // non-finite f64 already landed — this row shape was always the
            // lake's way of saying "no reading here", and `Value::Null` is
            // simply the edge finally able to say it too.
            Value::Null {} => {}
        }
        self.f64.append_option(f);
        self.i64.append_option(i);
        self.bool.append_option(b);
        self.text.append_option(s);
        self.bytes.append_option(y);
        self.rows += 1;
    }

    pub fn rows(&self) -> usize {
        self.rows
    }
    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }
    /// The frame's clock — handed back whole so a caller starting a fresh
    /// frame on the same clock cannot start it on half of one.
    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// Finish the batch. `None` when nothing was pushed — an empty frame is
    /// bytes that say nothing, and the wire does not carry it.
    pub fn finish(mut self) -> anyhow::Result<Option<RecordBatch>> {
        if self.rows == 0 {
            return Ok(None);
        }
        Ok(Some(RecordBatch::try_new(
            explicit_schema(self.clock),
            vec![
                Arc::new(self.path.finish()),
                Arc::new(self.t_us.finish().with_timezone("UTC")),
                Arc::new(self.f64.finish()),
                Arc::new(self.i64.finish()),
                Arc::new(self.bool.finish()),
                Arc::new(self.text.finish()),
                Arc::new(self.bytes.finish()),
            ],
        )?))
    }
}

// ---------------------------------------------------------------------------
// A frame's self-description.

/// What a frame's schema metadata says about itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMeta {
    pub form: TimeForm,
    /// The frame's clock — basis AND, for a back-dated one, the producer's own
    /// bound. One field, because a reader that could hold the basis without the
    /// bound is a reader that can drop it.
    pub clock: Clock,
}

fn meta_get<'a>(batch: &'a RecordBatch, key: &str) -> Result<&'a str, WireFault> {
    batch.schema_ref().metadata().get(key).map(String::as_str).ok_or_else(|| WireFault::Payload {
        kind: FrameKind::Data,
        detail: format!("frame schema is missing `{key}`"),
    })
}

fn meta_num<T: std::str::FromStr>(batch: &RecordBatch, key: &str) -> Result<T, WireFault> {
    let raw = meta_get(batch, key)?;
    raw.parse::<T>().map_err(|_| WireFault::Payload {
        kind: FrameKind::Data,
        detail: format!("frame schema `{key}` = `{raw}` is not a number"),
    })
}

/// Read a frame's declared form and clock.
///
/// Note what is NOT here: any path by which a frame becomes uniform without
/// saying so. A frame whose stamps merely look evenly spaced is explicit, and
/// stays explicit.
pub fn frame_meta(batch: &RecordBatch) -> Result<FrameMeta, WireFault> {
    let basis: TimeBasis =
        meta_get(batch, META_BASIS)?.parse().map_err(|e: String| WireFault::Payload {
            kind: FrameKind::Data,
            detail: format!("frame schema `{META_BASIS}`: {e}"),
        })?;
    // Absent is UNKNOWN, not zero — a producer that has not stated its bound
    // must not read as one that anchors perfectly. `Clock::from_columns` holds
    // that line, and also drops a bound stated against a basis that has none.
    let anchor_us = match batch.schema_ref().metadata().get(META_ANCHOR_US) {
        Some(_) => Some(meta_num(batch, META_ANCHOR_US)?),
        None => None,
    };
    let clock = Clock::from_columns(basis, anchor_us);
    let form = match meta_get(batch, META_FORM)? {
        FORM_UNIFORM => {
            let t0_us: i64 = meta_num(batch, META_T0_US)?;
            let sub_ns: i64 = match batch.schema_ref().metadata().get(META_T0_SUB_NS) {
                Some(_) => meta_num(batch, META_T0_SUB_NS)?,
                None => 0,
            };
            let num_ns: u64 = meta_num(batch, META_STEP_NUM_NS)?;
            let den: u64 = meta_num(batch, META_STEP_DEN)?;
            let step = Step::new(num_ns, den).ok_or_else(|| WireFault::Payload {
                kind: FrameKind::Data,
                detail: format!("uniform frame declares a step of {num_ns}/{den} ns"),
            })?;
            TimeForm::Uniform {
                t0: Micros::from_raw(t0_us).to_nanos() + DurationNs::from_raw(sub_ns),
                step,
            }
        }
        FORM_EXPLICIT => TimeForm::Explicit,
        other => {
            return Err(WireFault::Payload {
                kind: FrameKind::Data,
                detail: format!("unknown frame form `{other}`"),
            })
        }
    };
    Ok(FrameMeta { form, clock })
}

// ---------------------------------------------------------------------------
// The Arrow IPC stream codec.

/// Serialize a `RecordBatch` to an Arrow IPC **stream** byte buffer.
pub fn encode_ipc(batch: &RecordBatch) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut writer = arrow_ipc::writer::StreamWriter::try_new(&mut buf, &batch.schema())?;
        writer.write(batch)?;
        writer.finish()?;
    }
    Ok(buf)
}

/// Read all `RecordBatch`es out of an Arrow IPC stream byte buffer.
pub fn decode_ipc(bytes: &[u8]) -> anyhow::Result<Vec<RecordBatch>> {
    let reader = arrow_ipc::reader::StreamReader::try_new(std::io::Cursor::new(bytes), None)?;
    let mut out = Vec::new();
    for batch in reader {
        out.push(batch?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Float64Array;

    // The round trips — encode here, decode to samples there — are in
    // `record::wire`, beside the decoder they need.

    #[test]
    fn uniform_encode_refuses_shapes_that_are_not_uniform() {
        let a = [1.0, 2.0];
        let b = [1.0];
        let step = Step::from_rate_hz(1, 1).unwrap();
        let t0 = Nanos::from_raw(0);
        let e = uniform_batch(
            t0,
            step,
            Clock::source(),
            &[UniformColumn::f64("/a", &a), UniformColumn::f64("/b", &b)],
        )
        .unwrap_err();
        assert!(e.to_string().contains("SAME instants"), "{e}");

        let e = uniform_batch(
            t0,
            step,
            Clock::source(),
            &[UniformColumn::f64("/a", &a), UniformColumn::f64("/a", &a)],
        )
        .unwrap_err();
        assert!(e.to_string().contains("twice"), "{e}");

        assert!(uniform_batch(t0, step, Clock::source(), &[]).is_err());
    }

    /// A frame that does not describe itself is a payload fault by name, not a
    /// default that quietly applies.
    #[test]
    fn a_frame_without_metadata_is_a_named_payload_fault() {
        let plain = RecordBatch::try_new(
            Arc::new(Schema::new(vec![Field::new("/x", DataType::Float64, true)])),
            vec![Arc::new(Float64Array::from(vec![1.0]))],
        )
        .unwrap();
        let e = frame_meta(&plain).unwrap_err();
        assert!(matches!(e, WireFault::Payload { .. }));
        assert!(e.to_string().contains(META_BASIS), "{e}");
    }

    #[test]
    fn a_data_payload_that_is_not_arrow_is_a_named_payload_fault() {
        let e = decode_payload(b"\xff\xff\xff\xffnot arrow at all").unwrap_err();
        assert!(matches!(e, WireFault::Payload { kind: FrameKind::Data, .. }), "{e}");
    }

    /// An empty explicit frame is not written at all — the wire never carries
    /// bytes that say nothing.
    #[test]
    fn an_empty_explicit_frame_is_not_a_frame() {
        assert!(ExplicitFrame::new(Clock::host()).finish().unwrap().is_none());
    }
}
