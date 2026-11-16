//! **The control frames of the SDK door** — what a
//! [`FrameKind::Control`](crate::wire::frame::FrameKind::Control) payload says, as ONE
//! set of types for the side that writes it (the SDK) and the side
//! that reads it (the daemon).
//!
//! A control payload is one JSON object carrying one statement:
//!
//! ```text
//! producer → daemon   {"hello":{"producer":…,"sdk":…,"clock":{"id":…}}}
//!                     {"declare":[{"archetype":…,"path":…,"unit":…}, …]}
//!                     {"end":{}}
//! daemon → producer   {"welcome":{"permit":"<input id>"}}
//!                     {"refused":"<why>"}            (+ "candidate":true)
//! ```
//!
//! **The bytes are pinned.** The golden corpus (`*.phlm`) holds a hello, a hello naming
//! its clock, a declare and an end, and every binding reproduces them byte for
//! byte; the tests below hold the same bytes here. Every type serializes its
//! fields in DECLARATION order (never through a `serde_json::Value`, whose key
//! order moves with a dependency's `preserve_order`), so a field's position in
//! a struct below is part of the wire.
//!
//! No shape here buffers (`flatten`, `tag`, `untagged`): the daemon parses with
//! serde_json's `arbitrary_precision` on, and a buffered shape misreads numbers
//! parsed from text.
//!
//! An extension's stdout control plane declares channels with a different,
//! richer statement (`model::capability::ChannelDecl`); it is not this one.

use serde::{Deserialize, Deserializer, Serialize};

/// One control payload from a producer: at most one of its statements is set.
///
/// `A` is how the reader holds a declared archetype: the SDK writes the wire
/// NAME (`String`, checked against [`ARCHETYPES`]); the daemon reads its own
/// vocabulary type (`model::Archetype`), so an unknown name fails the frame
/// there exactly as it always did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "A: Serialize", deserialize = "A: Deserialize<'de>"))]
pub struct Control<A = String> {
    /// The opening frame: who is speaking. Sent once, first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hello: Option<Hello>,
    /// Channel declarations. Idempotent; a path may be declared again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declare: Vec<ChannelDecl<A>>,
    /// `{"end":{}}` — the stream ends here, by the producer's own word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<End>,
}

impl<A> Default for Control<A> {
    fn default() -> Self {
        Self { hello: None, declare: Vec::new(), end: None }
    }
}

impl<A> Control<A> {
    /// `{"hello":{…}}`.
    pub fn hello(hello: Hello) -> Self {
        Self { hello: Some(hello), ..Self::default() }
    }

    /// `{"declare":[…]}`.
    pub fn declare(channels: Vec<ChannelDecl<A>>) -> Self {
        Self { declare: channels, ..Self::default() }
    }

    /// `{"end":{}}`.
    pub fn end() -> Self {
        Self { end: Some(End {}), ..Self::default() }
    }
}

/// `{"hello": {...}}` — who is speaking, and optionally which clock its stamps
/// come off.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Hello {
    #[serde(default)]
    pub producer: String,
    #[serde(default)]
    pub sdk: String,
    /// `{"clock": {"id": "<serial or bus id>"}}` — the clock the producer's
    /// stamps come off, named so what is learned of it (the host-rung drift
    /// model) is kept by `(input, clock id)` across reconnects. An object so
    /// a `discipline` can later sit beside `id`, as on an extension's
    /// control plane.
    ///
    /// Written only when stated (no clock = no field: the older goldens'
    /// bytes). Read leniently: a hello with an unusable clock — not an
    /// object, no `id`, an `id` that is not a string — is still a hello. It
    /// reads as a clock STATED with an empty id, so a reader can tell "named
    /// no clock" (`None`) from "named one it cannot use", and
    /// [`Hello::clock_id`] answers none for both: the door never invents a
    /// key.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient_clock")]
    pub clock: Option<ClockStatement>,
}

impl Hello {
    /// A hello stating no clock.
    pub fn new(producer: impl Into<String>, sdk: impl Into<String>) -> Self {
        Self { producer: producer.into(), sdk: sdk.into(), clock: None }
    }

    /// Name the clock. The id is written as given; whether it is usable (not
    /// blank) is the writer's check to make before it states one.
    pub fn with_clock(mut self, id: impl Into<String>) -> Self {
        self.clock = Some(ClockStatement { id: id.into() });
        self
    }

    /// The stated clock id, trimmed; absent or empty = none.
    pub fn clock_id(&self) -> Option<String> {
        let id = self.clock.as_ref()?.id.trim();
        (!id.is_empty()).then(|| id.to_string())
    }
}

/// The clock a hello names: `{"id": …}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockStatement {
    pub id: String,
}

/// Read a hello's `clock` without letting an unusable one fail the hello:
/// anything but null is a clock stated, with the id it names or an empty one.
fn lenient_clock<'de, D: Deserializer<'de>>(d: D) -> Result<Option<ClockStatement>, D::Error> {
    let stated = Option::<serde_json::Value>::deserialize(d)?;
    Ok(stated.map(|v| ClockStatement {
        id: v.get("id").and_then(|id| id.as_str()).unwrap_or_default().to_string(),
    }))
}

/// One declared channel: what to draw a path as, its unit, which message it
/// is a leaf of. All optional but the path; a declaration with only a path
/// says "infer from the values".
///
/// **Field order is the wire's**: `archetype`, `message`, `path`, `unit` —
/// alphabetical, because the SDK first wrote this statement through a sorted
/// JSON map and the golden `uniform_gap.phlm` holds those bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(serialize = "A: Serialize", deserialize = "A: Deserialize<'de>"))]
pub struct ChannelDecl<A = String> {
    /// The display archetype — by wire name ([`ARCHETYPES`]) for a writer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archetype: Option<A>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageOrigin>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

impl<A> ChannelDecl<A> {
    /// A declaration of `path` alone.
    pub fn new(path: impl Into<String>) -> Self {
        Self { archetype: None, message: None, path: path.into(), unit: None }
    }
}

/// The wire names a declaration's `archetype` may state — the closed display
/// vocabulary, by name. The vocabulary's TYPE is `model::Archetype` (it also
/// decides storage routing, which is not the wire's business); a test in
/// `model` holds this list equal to `Archetype::ALL`, name for name, so a
/// writer that has only this crate refuses exactly what the daemon would.
pub const ARCHETYPES: [&str; 25] = [
    "scalar",
    "text",
    "log",
    "image",
    "segmentation_image",
    "boxes2d",
    "boxes3d",
    "points3d",
    "transform3d",
    "vector",
    "line_strips3d",
    "arrows3d",
    "poses3d",
    "mesh3d",
    "ellipsoids3d",
    "cylinders3d",
    "text3d",
    "grid_map",
    "pinhole",
    "depth_image",
    "geo_points",
    "geo_line_strings",
    "points2d",
    "line_strips2d",
    "text2d",
];

/// Is `name` a channel archetype's wire name?
pub fn is_archetype(name: &str) -> bool {
    ARCHETYPES.contains(&name)
}

/// The MESSAGE a channel is a leaf of — where a flattened channel came from.
///
/// A ROS / MCAP connector turns one `sensor_msgs/msg/BatteryState` on
/// `/battery` into `/battery/voltage`, `/battery/current`, … — each a channel
/// in its own right, and each silent about the fact that they arrived
/// together, as one message of one type. That relation is what a "raw
/// message" view needs (Foxglove's Raw Messages panel: pick a topic, see every
/// field of its latest message) and what nothing in the path can state:
/// `/battery/voltage` might be a leaf of `/battery` or a channel named that
/// way by a DAQ. So the producer SAYS it, once, on the channel's declaration,
/// and the daemon carries it onto the channel row. A channel that is not a
/// leaf of any message (a DAQ scalar, a derived signal) has none.
///
/// The type is the producer's own spelling — `sensor_msgs/msg/BatteryState`,
/// `foxglove.CompressedImage`, a protobuf full name — kept verbatim because it
/// is shown, never matched. `None` when the producer did not know it (a
/// rosbridge topic the graph never typed, a bag with an untyped channel).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageOrigin {
    /// The topic (record path) the message arrived on.
    pub topic: String,
    /// The message type, in the producer's own vocabulary.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
}

impl MessageOrigin {
    pub fn new(topic: impl Into<String>, message_type: Option<impl Into<String>>) -> Self {
        Self {
            topic: topic.into(),
            message_type: message_type.map(Into::into).filter(|t: &String| !t.trim().is_empty()),
        }
    }
}

/// `{"end": {}}` — the stream ends here, by the producer's own word.
///
/// Written as an empty object. Read as "the key is present with anything but
/// null": the statement is the key, and a reader that refused `{"end": true}`
/// would turn a producer's clean end into a cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct End {}

impl<'de> Deserialize<'de> for End {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(d)?;
        Ok(End {})
    }
}

/// The daemon's ONE answer to a hello: `{"welcome": {"permit": …}}`, or
/// `{"refused": "<why>"}` with `"candidate": true` when the refusal left a
/// discovered candidate in Inputs to switch on.
///
/// **Field order is the wire's**: `candidate`, `refused`, `welcome` — the
/// order the daemon has always written them in (a sorted JSON map).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Reply {
    #[serde(default, skip_serializing_if = "is_false")]
    pub candidate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub welcome: Option<Welcome>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Reply {
    /// `{"welcome":{"permit":"<input id>"}}`.
    pub fn welcome(permit: impl Into<String>) -> Self {
        Self { welcome: Some(Welcome { permit: permit.into() }), ..Self::default() }
    }

    /// `{"refused":"<why>"}`, with `"candidate":true` ahead of it when set.
    pub fn refused(why: impl Into<String>, candidate: bool) -> Self {
        Self { candidate, refused: Some(why.into()), welcome: None }
    }

    /// What the daemon decided. A refusal wins over a welcome (a reply that
    /// says both is a refusal); `None` when it said neither.
    pub fn verdict(self) -> Option<Result<Welcome, Refused>> {
        if let Some(why) = self.refused {
            return Some(Err(Refused { why, candidate: self.candidate }));
        }
        self.welcome.map(Ok)
    }
}

/// `{"welcome": {"permit": …}}` — the input the producer publishes under.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Welcome {
    #[serde(default)]
    pub permit: String,
}

/// A refusal, as its reader holds it: the daemon's words, and whether it left
/// a candidate to switch on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub why: String,
    pub candidate: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes<T: Serialize>(v: &T) -> String {
        String::from_utf8(serde_json::to_vec(v).unwrap()).unwrap()
    }

    /// The hello of every golden (the `*.phlm` corpus), and the one that
    /// names its clock (`hello_clock.phlm`) — byte for byte.
    #[test]
    fn a_hello_is_the_goldens_bytes() {
        let plain: Control = Control::hello(Hello::new("golden", "rust/golden"));
        assert_eq!(bytes(&plain), r#"{"hello":{"producer":"golden","sdk":"rust/golden"}}"#);
        let clocked: Control = Control::hello(
            Hello::new("golden", "rust/golden").with_clock("usb:0x0683-SN-01A2B3C4"),
        );
        assert_eq!(
            bytes(&clocked),
            r#"{"hello":{"producer":"golden","sdk":"rust/golden","clock":{"id":"usb:0x0683-SN-01A2B3C4"}}}"#
        );
    }

    /// The declare of `uniform_gap.phlm` — keys in the wire's order, an
    /// unstated field not written.
    #[test]
    fn a_declare_is_the_goldens_bytes() {
        let mut pressure = ChannelDecl::new("/rig/pressure");
        pressure.archetype = Some("scalar".to_string());
        pressure.unit = Some("bar".to_string());
        let mut valve = ChannelDecl::new("/rig/valve_open");
        valve.archetype = Some("scalar".to_string());
        assert_eq!(
            bytes(&Control::declare(vec![pressure, valve])),
            r#"{"declare":[{"archetype":"scalar","path":"/rig/pressure","unit":"bar"},{"archetype":"scalar","path":"/rig/valve_open"}]}"#
        );
        // Every field stated: archetype, message (topic, then type), path, unit.
        let mut leaf = ChannelDecl::new("/battery/voltage");
        leaf.archetype = Some("scalar".to_string());
        leaf.unit = Some("V".to_string());
        leaf.message = Some(MessageOrigin::new("/battery", Some("sensor_msgs/msg/BatteryState")));
        assert_eq!(
            bytes(&Control::declare(vec![leaf])),
            r#"{"declare":[{"archetype":"scalar","message":{"topic":"/battery","type":"sensor_msgs/msg/BatteryState"},"path":"/battery/voltage","unit":"V"}]}"#
        );
    }

    /// The end of `ended.phlm`.
    #[test]
    fn an_end_is_the_goldens_bytes() {
        assert_eq!(bytes(&Control::<String>::end()), r#"{"end":{}}"#);
    }

    /// The daemon's reply, in the bytes the daemon has always written.
    #[test]
    fn a_reply_is_the_daemons_bytes() {
        assert_eq!(bytes(&Reply::welcome("in-1")), r#"{"welcome":{"permit":"in-1"}}"#);
        assert_eq!(bytes(&Reply::refused("no", false)), r#"{"refused":"no"}"#);
        assert_eq!(bytes(&Reply::refused("no", true)), r#"{"candidate":true,"refused":"no"}"#);
    }

    /// What is written is what is read back, both directions.
    #[test]
    fn every_statement_round_trips() {
        let hello: Control = Control::hello(Hello::new("p", "s").with_clock("bus-7"));
        let back: Control = serde_json::from_slice(&serde_json::to_vec(&hello).unwrap()).unwrap();
        assert_eq!(back, hello);
        assert_eq!(back.hello.unwrap().clock_id().as_deref(), Some("bus-7"));

        let end: Control = serde_json::from_str(r#"{"end":{}}"#).unwrap();
        assert_eq!(end, Control::end());
        // The statement is the key: any value but null ends the stream.
        let end: Control = serde_json::from_str(r#"{"end":true}"#).unwrap();
        assert!(end.end.is_some());
        let none: Control = serde_json::from_str(r#"{"end":null}"#).unwrap();
        assert!(none.end.is_none());

        let welcome: Reply = serde_json::from_str(r#"{"welcome":{"permit":"in-1"}}"#).unwrap();
        assert_eq!(welcome.verdict(), Some(Ok(Welcome { permit: "in-1".into() })));
        let refused: Reply = serde_json::from_str(r#"{"refused":"no","candidate":true}"#).unwrap();
        assert_eq!(refused.verdict(), Some(Err(Refused { why: "no".into(), candidate: true })));
        let neither: Reply = serde_json::from_str("{}").unwrap();
        assert_eq!(neither.verdict(), None);
    }

    /// A hello with an unusable clock is still a hello: it names no id, and
    /// says whether a clock was stated at all.
    #[test]
    fn an_unusable_clock_never_fails_the_hello() {
        for (json, stated, want) in [
            (r#"{"hello":{"producer":"p","sdk":"s"}}"#, false, None),
            (r#"{"hello":{"producer":"p","sdk":"s","clock":null}}"#, false, None),
            (r#"{"hello":{"producer":"p","sdk":"s","clock":"usb"}}"#, true, None),
            (r#"{"hello":{"producer":"p","sdk":"s","clock":{}}}"#, true, None),
            (r#"{"hello":{"producer":"p","sdk":"s","clock":{"id":7}}}"#, true, None),
            (r#"{"hello":{"producer":"p","sdk":"s","clock":{"id":"  "}}}"#, true, None),
            (r#"{"hello":{"producer":"p","sdk":"s","clock":{"id":" a1 "}}}"#, true, Some("a1")),
            (
                r#"{"hello":{"producer":"p","sdk":"s","clock":{"id":"a1","discipline":"ptp"}}}"#,
                true,
                Some("a1"),
            ),
        ] {
            let c: Control = serde_json::from_str(json).unwrap();
            let hello = c.hello.unwrap();
            assert_eq!(hello.clock.is_some(), stated, "{json}");
            assert_eq!(hello.clock_id().as_deref(), want, "{json}");
        }
    }

    /// An unknown key is ignored (a newer producer against an older daemon),
    /// and a declaration needs its path.
    #[test]
    fn a_declare_reads_what_it_knows_and_needs_a_path() {
        let c: Control =
            serde_json::from_str(r#"{"declare":[{"path":"/a","unit":"V","later":1.5}]}"#).unwrap();
        assert_eq!(c.declare[0].path, "/a");
        assert_eq!(c.declare[0].unit.as_deref(), Some("V"));
        assert!(serde_json::from_str::<Control>(r#"{"declare":[{"unit":"V"}]}"#).is_err());
    }

    #[test]
    fn the_archetype_names_are_distinct_and_checked_by_name() {
        let mut seen = std::collections::BTreeSet::new();
        assert!(ARCHETYPES.iter().all(|a| seen.insert(*a)));
        assert!(is_archetype("scalar"));
        assert!(!is_archetype("hologram"));
        assert!(!is_archetype("Scalar"));
    }
}
