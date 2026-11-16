//! Layer 1 — ENCODE: declarations, samples and gaps become PHLM frames. No
//! I/O of its own: an [`Encoder`] hands each frame to a [`FrameSink`] — any
//! writer (a socket, a `Vec<u8>`: the frame bytes, exactly the golden corpus)
//! or a transport that takes frames whole (the direct route).

use std::collections::HashMap;
use std::io::Write;
use crate::wire::control::{Control, Hello};
pub use crate::wire::frame::Step;
use crate::wire::frame::{sample_instant, FrameKind};
pub use crate::wire::gap::GapExtent;

use crate::stream::{Pushed, UniformStream};

/// Which clock a frame's stamps are on. Stated on every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeBasis {
    /// The producer's own clock (a device, a bus, a recording's log time).
    Source,
    /// This machine's wall clock, read by the producer as it sampled.
    Host,
    /// Stamps from an earlier run, replayed now; `anchor_us` is the wall-clock
    /// instant (µs since the epoch) the replay began.
    Backdated { anchor_us: i64 },
}

impl TimeBasis {
    fn clock(self) -> crate::wire::Clock {
        match self {
            TimeBasis::Source => crate::wire::Clock::source(),
            TimeBasis::Host => crate::wire::Clock::host(),
            TimeBasis::Backdated { anchor_us } => crate::wire::Clock::backdated(anchor_us),
        }
    }

    /// Parse the wire spelling (`source` · `host` · `backdated:<anchor_us>`),
    /// for bindings whose callers pass a string.
    pub fn parse(s: &str) -> Result<Self, Error> {
        let s = s.trim();
        match s {
            "source" => Ok(TimeBasis::Source),
            "host" => Ok(TimeBasis::Host),
            _ => {
                if let Some(rest) = s.strip_prefix("backdated:") {
                    rest.trim()
                        .parse::<i64>()
                        .map(|anchor_us| TimeBasis::Backdated { anchor_us })
                        .map_err(|_| {
                            Error::Usage(format!("`{s}`: backdated needs `backdated:<anchor_us>`"))
                        })
                } else {
                    Err(Error::Usage(format!(
                        "`{s}` is not a time basis — `source`, `host` or `backdated:<anchor_us>`"
                    )))
                }
            }
        }
    }
}

/// A value in an explicit frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    F64(f64),
    I64(i64),
    Bool(bool),
    Text(String),
}

impl Value {
    fn to_wire(&self) -> crate::wire::Value {
        match self {
            Value::F64(v) => crate::wire::Value::F64(*v),
            Value::I64(v) => crate::wire::Value::I64(*v),
            Value::Bool(v) => crate::wire::Value::Bool(*v),
            Value::Text(v) => crate::wire::Value::Text(v.clone()),
        }
    }
}

/// A channel declaration: what to draw a path as, its unit, which message it
/// is a leaf of. All optional; a declaration with only a path is allowed and
/// says "infer from the values".
///
/// This is the SDK's builder; what it IS on the wire is
/// [`crate::wire::control::ChannelDecl`], the one definition the daemon reads with, and
/// it serializes as exactly that.
#[derive(Debug, Clone, Default)]
pub struct ChannelDecl {
    pub path: String,
    pub archetype: Option<String>,
    pub unit: Option<String>,
    pub message: Option<crate::wire::control::MessageOrigin>,
}

impl ChannelDecl {
    /// The statement as the wire carries it.
    fn to_wire(&self) -> crate::wire::control::ChannelDecl {
        crate::wire::control::ChannelDecl {
            archetype: self.archetype.clone(),
            message: self.message.clone(),
            path: self.path.clone(),
            unit: self.unit.clone(),
        }
    }
}

impl serde::Serialize for ChannelDecl {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_wire().serialize(s)
    }
}

impl ChannelDecl {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into(), ..Default::default() }
    }
    /// The display archetype, by its wire name (`scalar`, `text`, `image`,
    /// …). An unknown name is refused here, not by the daemon, because the
    /// vocabulary is closed and a renderer can only draw what it knows.
    pub fn archetype(mut self, name: &str) -> Result<Self, Error> {
        if !crate::wire::control::is_archetype(name) {
            return Err(Error::Usage(format!("`{name}` is not a channel archetype")));
        }
        self.archetype = Some(name.to_string());
        Ok(self)
    }
    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        let u: String = unit.into();
        self.unit = (!u.trim().is_empty()).then_some(u);
        self
    }
}

/// One channel's column in a uniform frame.
#[derive(Debug, Clone)]
pub struct Column {
    pub path: String,
    pub values: ColumnValues,
    /// `Some(valid)`: `false` marks a reading the producer did not get — a
    /// NULL on the wire, counted by the daemon, never shifted.
    pub valid: Option<Vec<bool>>,
}

#[derive(Debug, Clone)]
pub enum ColumnValues {
    F64(Vec<f64>),
    I64(Vec<i64>),
    Bool(Vec<bool>),
    /// A converter's 16-bit codes and the exact linear map to their value,
    /// `value = code · scale + offset`: 2 bytes a sample on
    /// the wire and in the daemon's log instead of 8, the value still exact.
    /// Uniform frames only — an explicit frame carries values, not codes.
    I16 {
        codes: Vec<i16>,
        scale: f64,
        offset: f64,
    },
    /// 32-bit codes and their map, as [`ColumnValues::I16`].
    I32 {
        codes: Vec<i32>,
        scale: f64,
        offset: f64,
    },
}

impl Column {
    pub fn f64(path: impl Into<String>, values: Vec<f64>) -> Self {
        Self { path: path.into(), values: ColumnValues::F64(values), valid: None }
    }
    pub fn i64(path: impl Into<String>, values: Vec<i64>) -> Self {
        Self { path: path.into(), values: ColumnValues::I64(values), valid: None }
    }
    pub fn bool(path: impl Into<String>, values: Vec<bool>) -> Self {
        Self { path: path.into(), values: ColumnValues::Bool(values), valid: None }
    }
    /// 16-bit converter codes and the exact map to their value, `code ·
    /// scale + offset`. Refused here, before anything is written, when the
    /// map is not one: a scale must be finite and non-zero, an offset finite.
    pub fn i16(
        path: impl Into<String>,
        codes: Vec<i16>,
        scale: f64,
        offset: f64,
    ) -> Result<Self, Error> {
        let path = path.into();
        code_map(&path, scale, offset)?;
        Ok(Self { path, values: ColumnValues::I16 { codes, scale, offset }, valid: None })
    }
    /// 32-bit converter codes and their map; refused as [`Column::i16`].
    pub fn i32(
        path: impl Into<String>,
        codes: Vec<i32>,
        scale: f64,
        offset: f64,
    ) -> Result<Self, Error> {
        let path = path.into();
        code_map(&path, scale, offset)?;
        Ok(Self { path, values: ColumnValues::I32 { codes, scale, offset }, valid: None })
    }
    pub fn with_validity(mut self, valid: Vec<bool>) -> Self {
        self.valid = Some(valid);
        self
    }
    fn len(&self) -> usize {
        match &self.values {
            ColumnValues::F64(v) => v.len(),
            ColumnValues::I64(v) => v.len(),
            ColumnValues::Bool(v) => v.len(),
            ColumnValues::I16 { codes, .. } => codes.len(),
            ColumnValues::I32 { codes, .. } => codes.len(),
        }
    }
}

/// A code column's map must be one: `code · scale + offset` with a finite,
/// non-zero scale and a finite offset. Checked by the constructors AND by the
/// encoder ([`ColumnValues`] is public), so a bad map is refused before the
/// overlap guard moves anything and before a byte is written.
fn code_map(path: &str, scale: f64, offset: f64) -> Result<(), Error> {
    if scale.is_finite() && scale != 0.0 && offset.is_finite() {
        return Ok(());
    }
    Err(Error::Usage(format!(
        "`{path}`: the map code · {scale} + {offset} is not one — a scale must be finite and \
         non-zero, an offset finite"
    )))
}

/// What this crate can get wrong, and what the daemon can say back.
#[derive(Debug)]
pub enum Error {
    /// The caller's inputs do not make a frame (mismatched lengths, an
    /// unknown archetype, a step of zero).
    Usage(String),
    /// The daemon turned the connection away; the message is its own words.
    /// `candidate` = it recorded the attempt as a discovery candidate, so
    /// enabling the SDK input in Inputs is the fix.
    Refused {
        why: String,
        candidate: bool,
    },
    /// Nobody answered: the daemon is not there (no socket, nobody
    /// listening), or on the direct route the cloud did not answer.
    Unreachable(String),
    Io(std::io::Error),
    /// A uniform frame for `path` would start at `t0_ns`, before
    /// `previous_end_ns` — where the frame this encoder last wrote for that
    /// channel ended. Nothing was written. A fixed-rate stream cannot step
    /// backwards: the frame would re-claim instants already sent, and the daemon
    /// would keep that part of it out. Continue from `previous_end_ns`, or let
    /// [`UniformStream`] place the frames.
    Overlap {
        path: String,
        previous_end_ns: i64,
        t0_ns: i64,
    },
    /// The cloud turned the key or the program away (a revoked or expired
    /// key, one without the write scope, a workspace that moved). The message
    /// is the service's own words. Direct route only.
    Unauthorized(String),
    /// The direct route closed (or gave up) with data the cloud never
    /// confirmed: `records` frames-per-channel holding `samples` samples were
    /// still unacknowledged, `dropped` more were dropped at a full buffer
    /// (only under the drop policy). A CI job exits non-zero on this.
    Unsent {
        records: u64,
        samples: u64,
        dropped: u64,
        why: String,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Usage(s) => write!(f, "{s}"),
            Error::Refused { why, candidate: false } => write!(f, "refused by the Phloem daemon: {why}"),
            // The daemon saw the attempt and listed it: say how to allow it,
            // on a machine with a screen and on one without.
            Error::Refused { why, candidate: true } => write!(
                f,
                "refused by the Phloem daemon: {why}. This program is not allowed to send yet. Allow it \
                 in Phloem Computer (Inputs, the discovered SDK program), or on a machine with \
                 no screen: phloem source set sdk --driver ingest --params '{{\"codec\":\"phlm\"}}'"
            ),
            Error::Unauthorized(s) => write!(f, "not authorized: {s}"),
            Error::Unsent { records, samples, dropped, why } => write!(
                f,
                "{samples} samples in {records} records were not confirmed by the cloud\
                 {}: {why}",
                if *dropped > 0 {
                    format!(", and {dropped} records were dropped at a full buffer")
                } else {
                    String::new()
                }
            ),
            Error::Unreachable(s) => write!(f, "not reachable: {s}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Overlap { path, previous_end_ns, t0_ns } => write!(
                f,
                "`{path}`: this uniform frame starts {:.3} ms before the previous frame for the \
                 channel ended (t0 {t0_ns} ns, previous end {previous_end_ns} ns) — a fixed-rate \
                 stream cannot step back. Start it at the previous end, or use UniformStream",
                crate::time::DurationNs::from_raw(*previous_end_ns - *t0_ns).to_millis_f64()
            ),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Validate a stated clock id: `None` states no clock; `Some` must name one.
pub(crate) fn clock_statement(clock_id: Option<&str>) -> Result<Option<&str>, Error> {
    match clock_id {
        None => Ok(None),
        Some(id) if id.trim().is_empty() => Err(Error::Usage(
            "a clock id names the clock the stamps come off (a serial, a bus id) — it cannot be \
             empty; leave the clock unstated instead"
                .into(),
        )),
        Some(id) => Ok(Some(id)),
    }
}

/// Where an [`Encoder`]'s frames go. Every writer is one — the frame is
/// written as its PHLM bytes (header, then payload), which is the local door
/// and the golden corpus — and a transport that wants frames whole (the direct
/// route turns each into the lake's records) implements it itself.
pub trait FrameSink {
    /// Take one whole frame.
    fn frame(&mut self, kind: FrameKind, payload: &[u8]) -> Result<(), Error>;
    /// Push on whatever is buffered.
    fn flush_frames(&mut self) -> Result<(), Error>;
}

impl<W: Write> FrameSink for W {
    fn frame(&mut self, kind: FrameKind, payload: &[u8]) -> Result<(), Error> {
        crate::wire::payload::write_frame(self, kind, payload)?;
        Ok(())
    }
    fn flush_frames(&mut self) -> Result<(), Error> {
        self.flush()?;
        Ok(())
    }
}

/// The frame ENCODER: turns declarations, samples and gaps into PHLM frames
/// on any writer. [`Client`] is an `Encoder` over a socket; the golden corpus
/// is an `Encoder` over a `Vec<u8>`. Bindings wrap this, never the wire.
pub struct Encoder<W: FrameSink> {
    w: W,
    /// Per channel: where the last uniform frame this encoder wrote for it
    /// ended (exclusive), and at what step — the frontier a next frame may
    /// not start behind ([`Error::Overlap`]).
    ends: HashMap<String, (i64, Step)>,
    /// Host-clocked frames moved forward onto the previous end (see
    /// [`Encoder::uniform`]).
    continued: u64,
}

impl<W: FrameSink> Encoder<W> {
    pub fn new(w: W) -> Self {
        Self { w, ends: HashMap::new(), continued: 0 }
    }

    /// Host-clocked uniform frames this encoder moved forward to continue
    /// from the previous frame's end, rather than write over it.
    pub fn continued(&self) -> u64 {
        self.continued
    }

    /// Where the last uniform frame written for `path` ended (exclusive) —
    /// the earliest instant the next may start at. `None` before the first.
    pub fn uniform_end(&self, path: &str) -> Option<i64> {
        self.ends.get(path).map(|(e, _)| *e)
    }

    pub fn into_inner(self) -> W {
        self.w
    }

    /// The writer, for an in-memory encoder that wants its bytes so far.
    pub fn get_ref(&self) -> &W {
        &self.w
    }

    fn frame(&mut self, kind: FrameKind, payload: &[u8]) -> Result<(), Error> {
        self.w.frame(kind, payload)
    }

    /// The opening frame: who is speaking. Sent once, first. States no
    /// clock — see [`hello_with_clock`](Self::hello_with_clock).
    pub fn hello(&mut self, producer: &str, sdk: &str) -> Result<(), Error> {
        self.hello_with_clock(producer, sdk, None)
    }

    /// The opening frame, naming the CLOCK the producer's stamps come off:
    /// `{"hello":{"producer":…,"sdk":…,"clock":{"id":"<serial or bus id>"}}}`.
    ///
    /// The daemon keys what it learns about a producer's clock (the host-rung
    /// drift model) by `(input, clock.id)` when the
    /// hello states one — the same clock across reconnects and processes —
    /// and otherwise by `(input, producer, connection)`, so an undeclared
    /// producer that reconnects re-learns its clock from scratch. `None`
    /// writes no `clock` field at all (the bytes of [`hello`](Self::hello));
    /// an id that is empty or only whitespace is refused before anything is
    /// written. `clock` is an object so a `discipline` can later sit beside
    /// `id`, as it does on an extension's control plane.
    pub fn hello_with_clock(
        &mut self,
        producer: &str,
        sdk: &str,
        clock_id: Option<&str>,
    ) -> Result<(), Error> {
        // The wire's own type (`crate::wire::control`), the one the daemon reads the
        // hello with: its fields serialize in declaration order whatever
        // serde_json features the build unifies, so the golden bytes cannot
        // move with a dependency's `preserve_order`.
        let mut hello = Hello::new(producer, sdk);
        if let Some(id) = clock_statement(clock_id)? {
            hello = hello.with_clock(id);
        }
        let payload = serde_json::to_vec(&Control::<String>::hello(hello))
            .map_err(|e| Error::Usage(e.to_string()))?;
        self.frame(FrameKind::Control, &payload)
    }

    /// Declare channels. Idempotent; a path may be declared again with more.
    pub fn declare(&mut self, channels: &[ChannelDecl]) -> Result<(), Error> {
        if channels.is_empty() {
            return Ok(());
        }
        for c in channels {
            if c.path.trim().is_empty() {
                return Err(Error::Usage("a declared channel needs a path".into()));
            }
        }
        let declare = Control::declare(channels.iter().map(ChannelDecl::to_wire).collect());
        let payload = serde_json::to_vec(&declare).map_err(|e| Error::Usage(e.to_string()))?;
        self.frame(FrameKind::Control, &payload)
    }

    /// A UNIFORM frame: every column sampled at the same instants, `t0 + k·step`.
    /// `t0_ns` is on the stated basis. Columns must be the same length.
    ///
    /// **It may not start behind what was already sent for a channel.** A
    /// frame whose `t0_ns` falls before the end of the previous uniform frame
    /// written for one of its channels is either:
    ///
    /// * **continued** — basis [`TimeBasis::Host`], the same step, every
    ///   channel of the frame that has a history ending at the same instant,
    ///   and reaching back by no more than the frame's own duration. That is a
    ///   sender stamping each frame with the wall clock at send time and waking
    ///   early: the samples are contiguous and the stamp is jitter, so the
    ///   frame is written at the previous end and counted in
    ///   [`continued`](Self::continued);
    /// * **refused** — anything else ([`Error::Overlap`], nothing written). A
    ///   device's own clock ([`TimeBasis::Source`]) or a recording's
    ///   ([`TimeBasis::Backdated`]) stepping back is a fact about that clock,
    ///   and moving its stamps would be a lie the lake could never undo.
    pub fn uniform(
        &mut self,
        t0_ns: i64,
        step: Step,
        basis: TimeBasis,
        columns: &[Column],
    ) -> Result<(), Error> {
        self.write_uniform(t0_ns, step, basis, columns).map(|_| ())
    }

    /// [`uniform`](Self::uniform), answering the `t0_ns` the frame was
    /// written at (the previous end, when it was continued).
    fn write_uniform(
        &mut self,
        t0_ns: i64,
        step: Step,
        basis: TimeBasis,
        columns: &[Column],
    ) -> Result<i64, Error> {
        if columns.is_empty() {
            return Err(Error::Usage("a uniform frame needs at least one channel".into()));
        }
        let rows = columns[0].len();
        for c in columns {
            if c.len() != rows {
                return Err(Error::Usage(format!(
                    "`{}` has {} samples, `{}` has {rows} — a uniform frame's channels are sampled at the same instants",
                    c.path,
                    c.len(),
                    columns[0].path
                )));
            }
            if let ColumnValues::I16 { scale, offset, .. }
            | ColumnValues::I32 { scale, offset, .. } = &c.values
            {
                code_map(&c.path, *scale, *offset)?;
            }
            if let Some(v) = &c.valid {
                if v.len() != rows {
                    return Err(Error::Usage(format!(
                        "`{}` has {} validity flags for {rows} samples",
                        c.path,
                        v.len()
                    )));
                }
            }
        }
        let t0_ns = self.not_behind(t0_ns, step, basis, columns, rows as u64)?;
        let wire: Vec<crate::wire::payload::UniformColumn<'_>> = columns
            .iter()
            .map(|c| {
                let col = match &c.values {
                    ColumnValues::F64(v) => crate::wire::payload::UniformColumn::f64(&c.path, v),
                    ColumnValues::I64(v) => crate::wire::payload::UniformColumn::i64(&c.path, v),
                    ColumnValues::Bool(v) => crate::wire::payload::UniformColumn::bool(&c.path, v),
                    ColumnValues::I16 { codes, scale, offset } => {
                        crate::wire::payload::UniformColumn::i16(&c.path, codes, *scale, *offset)
                    }
                    ColumnValues::I32 { codes, scale, offset } => {
                        crate::wire::payload::UniformColumn::i32(&c.path, codes, *scale, *offset)
                    }
                };
                match &c.valid {
                    Some(valid) => col.with_validity(valid),
                    None => col,
                }
            })
            .collect();
        let batch =
            crate::wire::payload::uniform_batch(crate::time::Nanos::from_raw(t0_ns), step, basis.clock(), &wire)
                .map_err(|e| Error::Usage(e.to_string()))?;
        let payload =
            crate::wire::payload::encode_payload(&batch).map_err(|e| Error::Usage(e.to_string()))?;
        self.frame(FrameKind::Data, &payload)?;
        let end = sample_instant(crate::time::Nanos::from_raw(t0_ns), step, rows as u64).raw_ns();
        for c in columns {
            self.ends.insert(c.path.clone(), (end, step));
        }
        Ok(t0_ns)
    }

    /// The guard on [`uniform`](Self::uniform): the `t0_ns` to write at, or
    /// the overlap refused.
    fn not_behind(
        &mut self,
        t0_ns: i64,
        step: Step,
        basis: TimeBasis,
        columns: &[Column],
        rows: u64,
    ) -> Result<i64, Error> {
        let mut behind: Option<(&str, i64, Step)> = None;
        for c in columns {
            if let Some(&(end, s)) = self.ends.get(&c.path) {
                if t0_ns < end && behind.is_none_or(|(_, e, _)| end > e) {
                    behind = Some((&c.path, end, s));
                }
            }
        }
        let Some((path, end, prev_step)) = behind else {
            return Ok(t0_ns);
        };
        let frame_ns = sample_instant(crate::time::Nanos::from_raw(0), step, rows).raw_ns();
        // Every channel with a history must end exactly here, or moving the
        // frame onto `end` would leave one of them a hole it never had.
        let one_end = columns.iter().all(|c| self.ends.get(&c.path).is_none_or(|&(e, _)| e == end));
        if basis == TimeBasis::Host && prev_step == step && one_end && end - t0_ns <= frame_ns {
            self.continued += 1;
            return Ok(end);
        }
        Err(Error::Overlap { path: path.to_string(), previous_end_ns: end, t0_ns })
    }

    /// Write the next frame of a [`UniformStream`]: `columns` are its channels
    /// (any order, each exactly once), placed where the stream's previous
    /// frame ended. On the host basis the stream is checked against the wall
    /// clock now; see [`UniformStream`] for what it does when it has fallen
    /// behind.
    pub fn push(
        &mut self,
        stream: &mut UniformStream,
        columns: &[Column],
    ) -> Result<Pushed, Error> {
        self.push_at(stream, columns, crate::time::wall_now_ns().raw_ns())
    }

    /// [`push`](Self::push) with the host clock's reading given — what a test
    /// and a binding that reads its own clock use.
    pub fn push_at(
        &mut self,
        stream: &mut UniformStream,
        columns: &[Column],
        now_ns: i64,
    ) -> Result<Pushed, Error> {
        stream.check(columns)?;
        let rows = columns[0].len() as u64;
        let plan = stream.plan(rows, now_ns)?;
        if let Some((frontier_ns, missing)) = plan.reanchored {
            self.gap(
                &stream.paths,
                frontier_ns,
                GapExtent::AtLeastSamples(missing),
                &format!(
                    "the stream fell {:.0} ms behind this machine's clock — fewer samples arrived \
                     than its stated rate produces in that time — and was re-anchored forward",
                    crate::time::DurationNs::from_raw(plan.t0_ns - frontier_ns).to_millis_f64()
                ),
            )?;
        }
        let t0_ns = self.write_uniform(plan.t0_ns, stream.step, stream.basis, columns)?;
        stream.advance(plan.t0_ns, t0_ns, rows);
        Ok(Pushed { t0_ns, reanchored: plan.reanchored.map(|(_, n)| n) })
    }

    /// An EXPLICIT frame: each sample carries its own instant (`t_ns` on the
    /// stated basis; `None` = unstamped, the daemon stamps it on arrival).
    pub fn explicit(
        &mut self,
        basis: TimeBasis,
        samples: &[(String, Option<i64>, Value)],
    ) -> Result<(), Error> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut f = crate::wire::payload::ExplicitFrame::new(basis.clock());
        for (path, t_ns, value) in samples {
            if path.trim().is_empty() {
                return Err(Error::Usage("a sample needs a path".into()));
            }
            let t = t_ns.map(|ns| crate::time::Micros::from_raw(ns.div_euclid(1000)));
            f.push(path, t, &value.to_wire());
        }
        let Some(batch) = f.finish().map_err(|e| Error::Usage(e.to_string()))? else {
            return Ok(());
        };
        let payload =
            crate::wire::payload::encode_payload(&batch).map_err(|e| Error::Usage(e.to_string()))?;
        self.frame(FrameKind::Data, &payload)
    }

    /// State a hole: good data stopped at `frontier_ns` on every path named,
    /// `extent` samples wide (or unknown), in the producer's own words.
    pub fn gap(
        &mut self,
        paths: &[String],
        frontier_ns: i64,
        extent: GapExtent,
        detail: &str,
    ) -> Result<(), Error> {
        if paths.is_empty() {
            return Err(Error::Usage("a gap needs the paths it covers".into()));
        }
        let stmt = crate::wire::gap::GapStatement {
            paths: paths.to_vec(),
            frontier_ns: crate::time::Nanos::from_raw(frontier_ns),
            extent,
            detail: detail.to_string(),
        };
        let payload = serde_json::to_vec(&stmt).map_err(|e| Error::Usage(e.to_string()))?;
        self.frame(FrameKind::Gap, &payload)
    }

    /// The END of the stream, by the producer's own word: Control
    /// `{"end":{}}`, the last frame a stream carries. The daemon closes the
    /// downtime marks of every channel the connection delivered — the
    /// producer stopped, nothing more is expected — where a connection that
    /// closes without it is a CUT whose outage is stated (`producer_cut`).
    /// [`Client::close`] sends it; nothing may be written after it.
    pub fn end(&mut self) -> Result<(), Error> {
        let payload = serde_json::to_vec(&Control::<String>::end())
            .map_err(|e| Error::Usage(e.to_string()))?;
        self.frame(FrameKind::Control, &payload)
    }

    pub fn flush(&mut self) -> Result<(), Error> {
        self.w.flush_frames()
    }

    /// The sink, mutably — for a transport that has more to say than frames
    /// (the direct route's close).
    #[cfg_attr(not(feature = "direct"), allow(dead_code))]
    pub(crate) fn sink_mut(&mut self) -> &mut W {
        &mut self.w
    }
}
