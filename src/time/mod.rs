//! Typed time units + the blessed clocks — the one authority for timestamp and
//! duration scale across the stack.
//!
//! Time used to be a bare `i64` whose unit lived only in a variable name, and
//! names go stale (the ns→µs lake migration left `t_ns` identifiers holding
//! microseconds and a decode path that silently dropped a 1000× factor). These
//! newtypes make the unit part of the *type* — the inner field is **private**,
//! so a wrong-unit value cannot flow without a greppable `from_raw`/`raw_*`
//! escape hatch, and every cross-unit conversion is a named method.
//!
//! Canonical units in the system (time-construct audit, 2026-07-31):
//!   * **[`Nanos`]**  — the edge **source clock** (sensor/driver stamps,
//!     `model::Sample.t_ns`). Lives only inside the edge process.
//!   * **[`Micros`]** — the **serialized** unit (`event_time` = Arrow
//!     `Timestamp(µs, UTC)`; scalar wires carry `_us`).
//!   * **[`Millis`]** — the **presentation** unit (HTTP/replay surface, player
//!     cursor, window bounds).
//!
//! Instants and durations are distinct types: instant − instant = duration,
//! instant ± duration = instant, and instant + instant deliberately does not
//! compile (summing two epoch stamps is always a bug).
//!
//! Clock policy — exactly three reads exist:
//!   * [`wall_now`] (and its ns view [`wall_now_ns`]) — the wall clock, for
//!     epoch labels. It PANICS if the system clock reads pre-epoch rather than
//!     returning 0: a 1970 stamp is indistinguishable from a real instant, and
//!     day-partition retention deletes it on arrival — a crash gets fixed, a
//!     silent 1970 stamp ships.
//!   * [`mono`] — the monotonic clock, for durations and deadlines. Never
//!     compare a raw wall `now` against anything that must survive an NTP step.
//!   * [`mono_counting_sleep`] — the monotonic clock that KEEPS COUNTING while
//!     the host sleeps, for anything that turns elapsed time back into an
//!     epoch stamp (a mono→wall anchor). [`mono`] stops during sleep on macOS
//!     and Linux, so an anchor built on it falls behind the wall by every
//!     second the lid was shut.

use serde::{Deserialize, Serialize};

/// Nanoseconds per microsecond.
pub const NS_PER_US: i64 = 1_000;
/// Nanoseconds per millisecond.
pub const NS_PER_MS: i64 = 1_000_000;
/// Nanoseconds per second.
pub const NS_PER_S: i64 = 1_000_000_000;
/// Microseconds per millisecond.
pub const US_PER_MS: i64 = 1_000;
/// Microseconds per second.
pub const US_PER_S: i64 = 1_000_000;
/// Milliseconds per second. The presentation-tier factor: ms is the display
/// unit, so a human-facing "N seconds" and a `*_ms` deadline cross here.
pub const MS_PER_S: i64 = 1_000;
/// Milliseconds per second, as a float — the same factor for the `f64` duration
/// arithmetic that would otherwise spell it `* 1000.0`.
pub const MS_PER_S_F64: f64 = 1_000.0;

/// How far past *now* an instant may claim to be before nothing believes it.
///
/// One number, three enforcement points — the edge publish boundary, the
/// lake's `land()`, and windowed readers — because a device with a fast
/// RTC is one device, and a bound that differs by layer is a bound a user
/// cannot reason about. Sized from what an honest clock can be off by: the
/// edge→cloud hop plus NTP tolerance is milliseconds, so 5 s admits every real
/// clock and no fantasy.
///
/// The failure it exists to stop is asymmetric, which is why the future is
/// bounded far more tightly than the past: a far-future stamp poisons every
/// max-based reader (the replay window, the coverage extent, the live "latest"
/// cursor) for as long as it is retained — ONE such sample parks the replay
/// playhead in the future and blanks every lane — while a late sample only
/// costs its own bucket's freshness.
pub const FUTURE_SLACK_US: i64 = 5 * US_PER_S;
/// [`FUTURE_SLACK_US`] in nanoseconds, for the edge's in-process clock.
pub const FUTURE_SLACK_NS: i64 = FUTURE_SLACK_US * NS_PER_US;

/// An instant: nanoseconds since the Unix epoch — the edge source clock.
/// Serializes as the plain `i64` ns it wraps (wire-identical to the old alias).
#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Nanos(i64);

/// An instant: microseconds since the Unix epoch — the canonical serialized unit.
#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Micros(i64);

/// An instant: milliseconds since the Unix epoch — the presentation unit.
#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Millis(i64);

/// A span of nanoseconds — what subtracting two [`Nanos`] instants yields.
#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct DurationNs(i64);

/// A span of milliseconds — what subtracting two [`Millis`] instants yields.
#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct DurationMs(i64);

/// A span of microseconds — what subtracting two [`Micros`] instants yields.
#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct DurationUs(i64);

impl Nanos {
    /// Wrap a raw epoch-ns `i64` — the greppable entry from untyped code
    /// (decoders, FFI, wire structs).
    #[inline]
    pub const fn from_raw(ns: i64) -> Self {
        Nanos(ns)
    }
    /// From split epoch parts — the `{secs, nsecs}` shape ROS/candump-family
    /// stamps arrive in. One named crossing instead of a bare `* 1e9 + nsec`.
    #[inline]
    pub const fn from_epoch_parts(secs: i64, nanos: u32) -> Self {
        Nanos(secs * NS_PER_S + nanos as i64)
    }
    /// From float epoch-seconds (candump timestamps, ROS float clocks). f64
    /// carries ~µs precision at 2026 epoch magnitudes — honest for file stamps;
    /// integer-part sources should prefer [`Nanos::from_epoch_parts`].
    #[inline]
    pub fn from_epoch_secs_f64(secs: f64) -> Self {
        Nanos((secs * NS_PER_S as f64) as i64)
    }
    /// The raw epoch-ns `i64` — the greppable exit to untyped code.
    #[inline]
    pub const fn raw_ns(self) -> i64 {
        self.0
    }
    /// Shift by a span, saturating at the i64 rails (window-bound arithmetic
    /// near the epoch ends must clamp, not wrap).
    #[inline]
    pub const fn saturating_add(self, d: DurationNs) -> Nanos {
        Nanos(self.0.saturating_add(d.raw()))
    }
    /// See [`Nanos::saturating_add`].
    #[inline]
    pub const fn saturating_sub(self, d: DurationNs) -> Nanos {
        Nanos(self.0.saturating_sub(d.raw()))
    }
    /// Instant − instant, `None` on i64 overflow (rebase math on untrusted
    /// file timelines).
    #[inline]
    pub const fn checked_sub(self, rhs: Nanos) -> Option<DurationNs> {
        match self.0.checked_sub(rhs.0) {
            Some(d) => Some(DurationNs(d)),
            None => None,
        }
    }
    /// The span since `earlier`, clamped at the i64 rails — for gap/throttle
    /// math where `earlier` may be a sentinel or an untrusted file stamp.
    #[inline]
    pub const fn saturating_since(self, earlier: Nanos) -> DurationNs {
        DurationNs(self.0.saturating_sub(earlier.0))
    }
    /// Truncate to microseconds (the lake-store direction; sub-µs floors to 0 —
    /// callers that need the distinction must stay in ns).
    #[inline]
    pub const fn to_micros_trunc(self) -> Micros {
        Micros(self.0 / NS_PER_US)
    }
    /// Truncate to milliseconds (the presentation direction).
    #[inline]
    pub const fn to_millis_trunc(self) -> Millis {
        Millis(self.0 / NS_PER_MS)
    }
}

impl Micros {
    /// Wrap a raw epoch-µs `i64` (Arrow readers / SQL literals).
    #[inline]
    pub const fn from_raw(us: i64) -> Self {
        Micros(us)
    }
    /// The raw epoch-µs `i64` (Arrow builders / SQL literals).
    #[inline]
    pub const fn raw_us(self) -> i64 {
        self.0
    }
    /// Scale up to nanoseconds — lossless; the read-direction mirror of
    /// [`Nanos::to_micros_trunc`], so an encode→decode round-trip is exact to µs.
    #[inline]
    pub const fn to_nanos(self) -> Nanos {
        Nanos(self.0 * NS_PER_US)
    }
    /// Truncate to milliseconds (the presentation direction).
    #[inline]
    pub const fn to_millis_trunc(self) -> Millis {
        Millis(self.0 / US_PER_MS)
    }
}

impl Millis {
    /// Wrap a raw epoch-ms `i64` (HTTP/replay wire fields).
    #[inline]
    pub const fn from_raw(ms: i64) -> Self {
        Millis(ms)
    }
    /// The raw epoch-ms `i64`.
    #[inline]
    pub const fn raw_ms(self) -> i64 {
        self.0
    }
    /// Scale up to microseconds — lossless.
    #[inline]
    pub const fn to_micros(self) -> Micros {
        Micros(self.0 * US_PER_MS)
    }
    /// Scale up to nanoseconds — lossless.
    #[inline]
    pub const fn to_nanos(self) -> Nanos {
        Nanos(self.0 * NS_PER_MS)
    }
}

impl DurationNs {
    #[inline]
    pub const fn from_raw(ns: i64) -> Self {
        DurationNs(ns)
    }
    #[inline]
    pub const fn raw_ns(self) -> i64 {
        self.0
    }
    /// Truncate to a µs span.
    #[inline]
    pub const fn to_micros_trunc(self) -> DurationUs {
        DurationUs(self.0 / NS_PER_US)
    }
    /// This span in milliseconds, KEEPING the sub-millisecond part — for
    /// DISPLAY, where a truncating `to_millis_trunc` would round a 0.4 ms
    /// offset to a confident `0`.
    ///
    /// It lives here rather than at the call site because a bare `as f64 /
    /// 1e6` is exactly the unanchored conversion the unit canon exists to
    /// stop, and the repository's time-unit check fails the build on one.
    /// The DAQ audit needed a fractional-ms offset and wrote the literal
    /// instead; a consumer that needs a unit the canon does not offer should
    /// add it HERE, once, so the next caller finds it.
    ///
    /// Returns `f64`, so it is a number to PRINT and never a value to compare
    /// or store: the typed spans stay integral on purpose.
    #[inline]
    pub fn to_millis_f64(self) -> f64 {
        self.0 as f64 / NS_PER_MS as f64
    }
    /// Fractional MICROSECONDS, for the same reason and under the same rule as
    /// [`Self::to_millis_f64`]: a number to PRINT, never one to compare or
    /// store. Added when the alignment estimator needed to say "plus 25 us for
    /// an undeclared front-end group delay" and reached for `as f64 / 1e3` —
    /// the unanchored conversion the canon exists to stop.
    #[inline]
    pub fn to_micros_f64(self) -> f64 {
        self.0 as f64 / NS_PER_US as f64
    }
    /// From a std (in-process, monotonic-derived) duration. Panics past
    /// ~292 years — a span that long is a bug, not a measurement.
    #[inline]
    pub fn from_std(d: std::time::Duration) -> Self {
        DurationNs(i64::try_from(d.as_nanos()).expect("duration overflows i64 ns"))
    }
    /// From float seconds — the shape an instrument's SAMPLE PERIOD arrives in
    /// (`1.0 / rate_hz`). Rounds rather than truncates: a 3 Hz period is
    /// 333_333_333 ns, and truncating every tick would drift the chunk anchor.
    #[inline]
    pub fn from_secs_f64(secs: f64) -> Self {
        DurationNs((secs * NS_PER_S as f64).round() as i64)
    }
}

impl DurationUs {
    #[inline]
    pub const fn from_raw(us: i64) -> Self {
        DurationUs(us)
    }
    /// From float seconds — the shape time daemons report offsets in
    /// (`chronyc -c tracking` system-time offset, `w32tm`'s `Phase Offset`).
    /// Rounds (not truncates): these are signed measurements around zero, where
    /// truncation biases toward 0 and would flatter a clock's discipline.
    #[inline]
    pub fn from_secs_f64(secs: f64) -> Self {
        DurationUs((secs * US_PER_S as f64).round() as i64)
    }
    /// Fractional SECONDS, for printing a span a human reads in seconds ("two
    /// events 60.000 s apart"). The read-direction mirror of
    /// [`Self::from_secs_f64`], under the same rule as
    /// [`DurationNs::to_millis_f64`]: a number to PRINT, never one to compare
    /// or store.
    #[inline]
    pub fn to_secs_f64(self) -> f64 {
        self.0 as f64 / US_PER_S as f64
    }
    #[inline]
    pub const fn raw_us(self) -> i64 {
        self.0
    }
    /// Scale up to a ns span — lossless.
    #[inline]
    pub const fn to_nanos(self) -> DurationNs {
        DurationNs(self.0 * NS_PER_US)
    }
}

impl DurationMs {
    #[inline]
    pub const fn from_raw(ms: i64) -> Self {
        DurationMs(ms)
    }
    /// From a std (in-process, monotonic-derived) duration, truncating sub-ms.
    /// The ms mirror of [`DurationNs::from_std`] — for elapsed-time readings
    /// that land on an `_ms` surface without a bare `as_millis() as i64`.
    #[inline]
    pub fn from_std(d: std::time::Duration) -> Self {
        DurationMs(i64::try_from(d.as_millis()).expect("duration overflows i64 ms"))
    }
    #[inline]
    pub const fn raw_ms(self) -> i64 {
        self.0
    }
    /// Scale up to a ns span — lossless.
    #[inline]
    pub const fn to_nanos(self) -> DurationNs {
        DurationNs(self.0 * NS_PER_MS)
    }
}

// Instant − instant = duration; instant ± duration = instant. `Add<Nanos> for
// Nanos` is deliberately unimplemented: the sum of two epoch stamps means
// nothing, and making it not compile is the point of the newtypes.
macro_rules! instant_duration_ops {
    ($instant:ty, $duration:ty) => {
        impl std::ops::Sub for $instant {
            type Output = $duration;
            #[inline]
            fn sub(self, rhs: Self) -> $duration {
                <$duration>::from_raw(self.0 - rhs.0)
            }
        }
        impl std::ops::Add<$duration> for $instant {
            type Output = $instant;
            #[inline]
            fn add(self, rhs: $duration) -> $instant {
                <$instant>::from_raw(self.0 + rhs.raw())
            }
        }
        impl std::ops::Sub<$duration> for $instant {
            type Output = $instant;
            #[inline]
            fn sub(self, rhs: $duration) -> $instant {
                <$instant>::from_raw(self.0 - rhs.raw())
            }
        }
        impl std::ops::Add for $duration {
            type Output = $duration;
            #[inline]
            fn add(self, rhs: Self) -> $duration {
                <$duration>::from_raw(self.raw() + rhs.raw())
            }
        }
        impl std::ops::Sub for $duration {
            type Output = $duration;
            #[inline]
            fn sub(self, rhs: Self) -> $duration {
                <$duration>::from_raw(self.raw() - rhs.raw())
            }
        }
        /// A span scaled by a COUNT — `period * k` is the k-th tick's offset on
        /// a sample clock. Only durations scale: an instant times a number is
        /// nonsense, so `Mul` is deliberately absent there.
        impl std::ops::Mul<i64> for $duration {
            type Output = $duration;
            #[inline]
            fn mul(self, k: i64) -> $duration {
                <$duration>::from_raw(self.raw() * k)
            }
        }
    };
}
// The macro needs one raw accessor name per duration type.
impl DurationNs {
    #[inline]
    const fn raw(self) -> i64 {
        self.0
    }
}
impl DurationUs {
    #[inline]
    const fn raw(self) -> i64 {
        self.0
    }
}
impl DurationMs {
    #[inline]
    const fn raw(self) -> i64 {
        self.0
    }
}
instant_duration_ops!(Nanos, DurationNs);
instant_duration_ops!(Micros, DurationUs);
instant_duration_ops!(Millis, DurationMs);

/// The one blessed wall-clock read: microseconds since the Unix epoch, UTC.
///
/// # Panics
/// If the system clock reads before the Unix epoch (broken RTC). Never returns
/// 0 as a fallback — a silent 1970 stamp reads as a genuine instant that
/// retention then sweeps, which is strictly worse than a crash.
pub fn wall_now() -> Micros {
    Micros(
        i64::try_from(pre_epoch_checked().as_micros())
            .expect("epoch-µs overflows i64 (~year 294k)"),
    )
}

/// The wall clock at ns precision — for drivers stamping event time in-process
/// (`model::Sample.t_ns` is on the source clock; the host wall stands in when the
/// source carries none). Same read, same pre-epoch panic as [`wall_now`].
pub fn wall_now_ns() -> Nanos {
    Nanos(i64::try_from(pre_epoch_checked().as_nanos()).expect("epoch-ns overflows i64 (2262)"))
}

// The ONE sanctioned raw wall read — the repo-wide clippy.toml bans
// `SystemTime::now` precisely so that every other stamp routes through here.
#[allow(clippy::disallowed_methods)]
fn pre_epoch_checked() -> std::time::Duration {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect(
        "system clock reads before the Unix epoch — refusing to stamp \
         (a 1970 stamp reads as real data and retention sweeps it)",
    )
}

/// The one blessed monotonic read — durations and deadlines. Epoch labels come
/// from [`wall_now`]/[`wall_now_ns`] (or a mono→wall anchor re-anchored between
/// runs, which must be built on [`mono_counting_sleep`], not on this); never
/// diff a wall read against a deadline that must survive NTP steps.
pub fn mono() -> std::time::Instant {
    std::time::Instant::now()
}

/// A reading of the monotonic clock that keeps counting while the host sleeps
/// — see [`mono_counting_sleep`]. Opaque: the origin is arbitrary (boot, on
/// every platform that has one), so the only meaningful operation is the
/// difference of two readings, which is a [`DurationNs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SleepCountingInstant(i64);

impl SleepCountingInstant {
    /// Time since this reading, sleep included.
    pub fn elapsed(self) -> DurationNs {
        mono_counting_sleep() - self
    }
}

impl std::ops::Sub for SleepCountingInstant {
    type Output = DurationNs;
    fn sub(self, earlier: Self) -> DurationNs {
        DurationNs(self.0.saturating_sub(earlier.0))
    }
}

/// The monotonic clock that KEEPS COUNTING through system sleep — the base for
/// any mono→wall anchor (`epoch = wall at anchor + elapsed since anchor`).
///
/// [`mono`] is `std::time::Instant`, which is `CLOCK_UPTIME_RAW` on macOS and
/// `CLOCK_MONOTONIC` on Linux: both STOP while the machine sleeps. An anchor
/// built on it loses every slept second, so on a laptop that closed its lid for
/// two hours the "epoch" it produced sat two hours in the past — the safety
/// loop's heartbeat read as dead, and every stamp it wrote was two hours early
/// (found 2026-09-30, lag = the summed sleep to the second). This read counts
/// the sleep and is still immune to NTP steps, which is the one property the
/// anchor exists for:
///   * macOS — `CLOCK_MONOTONIC` (mach continuous time: counts sleep);
///   * Linux/Android — `CLOCK_BOOTTIME` (counts suspend);
///   * Windows — `QueryInterruptTimePrecise` (interrupt time counts sleep and
///     hibernation; the *Unbiased* variant is the one that does not).
///
/// # Panics
/// If the OS refuses the read — these clocks exist on every supported kernel,
/// so a refusal is a broken host, and an anchor on a made-up number would
/// stamp nonsense silently.
pub fn mono_counting_sleep() -> SleepCountingInstant {
    SleepCountingInstant(sleep_counting_ns())
}

/// How long this machine has been up, ns — the sleep-counting clock, whose
/// origin is the boot on every supported kernel ([`mono_counting_sleep`]).
/// Never stepped by NTP or a person setting the clock, and only grows within
/// one boot: a reading below one taken earlier in the "same" boot proves it is
/// another boot. Not a timestamp.
pub fn uptime_ns() -> i64 {
    sleep_counting_ns()
}

/// **Does the OS guarantee [`boot_id`] differs on every boot?** Linux mints
/// `boot_id` at random in memory at each boot, and macOS mints
/// `kern.bootsessionuuid` the same way — nothing persisted, so a power cut
/// cannot make a boot repeat the last one's: `true`. Windows' `BootId` is a
/// counter in the SYSTEM registry hive, which is flushed to disk lazily: a
/// power cut before the hive's flush makes boot N+1 read boot N's value — so
/// there it is evidence, never proof, and a caller must ALSO check the machine
/// has been up at least as long as when it last looked ([`uptime_ns`], which
/// only grows within a boot and is never stepped) — not the boot instant,
/// which a wall-clock step moves: `false`.
pub const BOOT_ID_UNIQUE_PER_BOOT: bool =
    cfg!(any(target_os = "linux", target_os = "android", target_vendor = "apple"));

/// The wall-clock instant this machine BOOTED, epoch ns: the wall clock less
/// the sleep-counting clock, whose origin is the boot on every supported
/// kernel ([`mono_counting_sleep`]). Approximate by however far the wall clock
/// was stepped since the boot (NTP), so a caller comparing two readings
/// compares them within a tolerance — what tells one boot from the next, never
/// a timestamp to stamp anything with.
pub fn boot_wall_ns() -> i64 {
    wall_now_ns().raw_ns() - sleep_counting_ns()
}

/// **This boot of the machine, exactly** — an identity the kernel gives each
/// boot, hashed to 64 bits (FNV-1a, stable across builds): the Linux kernel's
/// `boot_id`, macOS's `kern.bootsessionuuid`, Windows' per-boot `BootId`
/// counter (`…\Memory Management\PrefetchParameters`, incremented by the
/// kernel at every boot). `None` where the OS states none — a caller then
/// falls back to [`boot_wall_ns`] within a tolerance, which a clock reset to
/// the same value every power-up (a dead CMOS battery) cannot tell apart.
///
/// Whether two boots can share one is [`BOOT_ID_UNIQUE_PER_BOOT`].
pub fn boot_id() -> Option<u64> {
    boot_id_os().map(|s| fnv1a(s.as_bytes()))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn boot_id_os() -> Option<String> {
    let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_string())
}

#[cfg(target_vendor = "apple")]
fn boot_id_os() -> Option<String> {
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: `buf`/`len` describe a valid, exclusively borrowed buffer; the
    // name is NUL-terminated; no new value is set.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len == 0 {
        return None;
    }
    let s = std::str::from_utf8(&buf[..len.min(buf.len())]).ok()?;
    let s = s.trim_end_matches('\0').trim();
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(windows)]
fn boot_id_os() -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD,
    };
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let key = wide(
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management\PrefetchParameters",
    );
    let name = wide("BootId");
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the key and value names are NUL-terminated UTF-16; `value` and
    // `size` describe a valid, exclusively borrowed DWORD.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        )
    };
    (rc == 0).then(|| format!("windows-boot-{value}"))
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple", windows)))]
fn boot_id_os() -> Option<String> {
    None
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const SLEEP_COUNTING_CLOCK: libc::clockid_t = libc::CLOCK_BOOTTIME;
#[cfg(target_vendor = "apple")]
const SLEEP_COUNTING_CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn sleep_counting_ns() -> i64 {
    clock_gettime_ns(SLEEP_COUNTING_CLOCK, "the sleep-counting clock")
}

/// The kernel's `CLOCK_MONOTONIC` now, ns since an arbitrary origin (boot) —
/// the clock a V4L2 driver stamps each captured buffer with
/// (`V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC`). It exists to map such a kernel
/// stamp onto the wall clock at the instant it is read (`wall_now − (this −
/// stamp)`); it is NOT an anchor, because it stops while the host suspends —
/// an anchor is [`mono_counting_sleep`].
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn kernel_monotonic_ns() -> i64 {
    clock_gettime_ns(libc::CLOCK_MONOTONIC, "CLOCK_MONOTONIC")
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn clock_gettime_ns(clock: libc::clockid_t, what: &str) -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid, exclusively borrowed `timespec` for the call.
    let rc = unsafe { libc::clock_gettime(clock, &mut ts) };
    assert_eq!(rc, 0, "clock_gettime refused {what}: {}", std::io::Error::last_os_error());
    // `time_t`/`c_long` are i64 on 64-bit targets (where `from` is the identity
    // clippy flags) and narrower on 32-bit ones; `from` is the lossless widening
    // that refuses to compile if either ever became wider than i64.
    #[allow(clippy::useless_conversion)]
    let (secs, nanos) = (i64::from(ts.tv_sec), i64::from(ts.tv_nsec));
    secs.saturating_mul(NS_PER_S).saturating_add(nanos)
}

/// Interrupt time is counted in 100 ns units.
#[cfg(windows)]
const NS_PER_INTERRUPT_TICK: i64 = 100;

#[cfg(windows)]
fn sleep_counting_ns() -> i64 {
    let mut ticks: u64 = 0;
    // SAFETY: `ticks` is a valid, exclusively borrowed u64 for the call; the
    // function cannot fail (it has no return value) on Windows 10+.
    unsafe {
        windows_sys::Win32::System::WindowsProgramming::QueryInterruptTimePrecise(&mut ticks)
    };
    i64::try_from(ticks)
        .expect("interrupt time overflows i64 (~29k years of uptime)")
        .saturating_mul(NS_PER_INTERRUPT_TICK)
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple", windows)))]
compile_error!(
    "time::mono_counting_sleep has no sleep-counting clock for this target — \
     name the platform's clock rather than falling back to one that stops in sleep"
);

#[cfg(test)]
mod tests {

    /// Readings never go backward, and the difference of two is the time
    /// between them.
    #[test]
    fn the_sleep_counting_clock_is_monotonic_and_measures_elapsed_time() {
        let a = mono_counting_sleep();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let b = mono_counting_sleep();
        assert!(b >= a, "never backward");
        let d = (b - a).raw_ns();
        assert!(d >= 20 * NS_PER_MS, "at least the slept 20 ms: {d} ns");
        assert!(d < 2 * NS_PER_S, "and not wildly more: {d} ns");
        assert!(a.elapsed() >= b - a, "elapsed() keeps counting");
    }

    /// THE property: boot time + this clock lands on the wall clock, because it
    /// has counted every second since boot, asleep or awake. `Instant` fails
    /// this on any machine that has ever slept — measured on the Mac that found
    /// the bug, boot plus `CLOCK_UPTIME_RAW` sat 7851 s behind the wall while boot
    /// plus this read agreed to the millisecond. On a host that never slept (a CI
    /// VM) both agree, so the test cannot fail for the wrong reason; it fails
    /// only if the read is swapped for a clock with a different origin or rate.
    /// The kernel names this boot, and names it the same on every read (the
    /// spool's wire reservation compares a reading across a restart).
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    #[test]
    fn this_boot_has_one_exact_identity() {
        let a = boot_id().expect("the kernel states a boot id here");
        assert_eq!(boot_id(), Some(a));
        assert_ne!(a, fnv1a(b""), "an identity, not the empty hash");
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    #[test]
    fn boot_time_plus_the_sleep_counting_clock_is_the_wall_clock() {
        let off = boot_wall_ns() + mono_counting_sleep().0 - wall_now_ns().raw_ns();
        // Linux states boot time in whole seconds; macOS in µs.
        assert!(off.abs() < 2 * NS_PER_S, "boot + clock is {off} ns off the wall");
    }

    /// Wall-clock epoch ns at boot, as the kernel states it.
    #[cfg(target_vendor = "apple")]
    fn boot_wall_ns() -> i64 {
        let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
        let mut len = std::mem::size_of::<libc::timeval>();
        // SAFETY: a NUL-terminated name, and an out-buffer of exactly `len` bytes.
        let rc = unsafe {
            libc::sysctlbyname(
                c"kern.boottime".as_ptr(),
                (&mut tv as *mut libc::timeval).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(rc, 0, "sysctl kern.boottime");
        tv.tv_sec * NS_PER_S + i64::from(tv.tv_usec) * NS_PER_US
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn boot_wall_ns() -> i64 {
        let stat = std::fs::read_to_string("/proc/stat").expect("/proc/stat");
        let secs: i64 = stat
            .lines()
            .find_map(|l| l.strip_prefix("btime "))
            .expect("btime line")
            .trim()
            .parse()
            .expect("btime is seconds");
        secs * NS_PER_S
    }

    /// The conversion the DAQ audit had written as a bare `as f64 / 1e6`.
    #[test]
    fn a_nanosecond_span_renders_fractional_milliseconds() {
        // the case a truncating conversion destroys: a sub-millisecond offset
        // is the interesting one, and `to_millis_trunc` calls it zero
        let sub = DurationNs::from_raw(400_000);
        assert_eq!(sub.to_millis_f64(), 0.4);
        assert_eq!(DurationNs::from_raw(1_500_000).to_millis_f64(), 1.5);
        // NEGATIVE spans survive: the audit prints ni−mcc, which is signed,
        // and an integer divide toward zero is not what a reader expects here
        assert_eq!(DurationNs::from_raw(-2_250_000).to_millis_f64(), -2.25);
        assert_eq!(DurationNs::from_raw(0).to_millis_f64(), 0.0);
    }
    use super::*;

    #[test]
    fn ns_us_round_trip_is_lossless_to_microsecond() {
        // 10_000 ns → 10 µs → 10_000 ns. The asymmetry that caused BUG-2 cannot
        // happen when both directions go through these methods.
        let n = Nanos::from_raw(10_000);
        assert_eq!(n.to_micros_trunc(), Micros::from_raw(10));
        assert_eq!(n.to_micros_trunc().to_nanos(), Nanos::from_raw(10_000));
    }

    #[test]
    fn sub_microsecond_truncates() {
        // 999 ns is below µs resolution and floors to 0 — callers that need this
        // distinction must keep ns, which is why the source clock stays `Nanos`.
        assert_eq!(Nanos::from_raw(999).to_micros_trunc(), Micros::from_raw(0));
    }

    #[test]
    fn ms_us_conversions() {
        assert_eq!(Millis::from_raw(5).to_micros(), Micros::from_raw(5_000));
        assert_eq!(Micros::from_raw(5_500).to_millis_trunc(), Millis::from_raw(5));
    }

    #[test]
    fn ns_ms_conversions() {
        assert_eq!(Nanos::from_raw(5_500_000).to_millis_trunc(), Millis::from_raw(5));
        assert_eq!(Millis::from_raw(2).to_nanos(), Nanos::from_raw(2_000_000));
    }

    #[test]
    fn instant_minus_instant_is_a_duration_and_instants_shift_by_durations() {
        let a = Nanos::from_raw(1_000);
        let b = Nanos::from_raw(400);
        assert_eq!(a - b, DurationNs::from_raw(600));
        assert_eq!(b - a, DurationNs::from_raw(-600)); // signed: order-preserving
        assert_eq!(b + DurationNs::from_raw(600), a);
        assert_eq!(a - DurationNs::from_raw(600), b);
        let u = Micros::from_raw(10) - Micros::from_raw(3);
        assert_eq!(u, DurationUs::from_raw(7));
        assert_eq!(u.to_nanos(), DurationNs::from_raw(7_000));
    }

    /// A sample clock's k-th tick: `t0 + period * k`. Scaling is a DURATION
    /// operation — the same expression with an instant on the left does not
    /// compile, which is what keeps a sample-index arithmetic slip out of the
    /// stamp.
    #[test]
    fn a_duration_scales_by_a_count() {
        let period = DurationNs::from_raw(100_000);
        assert_eq!(period * 3, DurationNs::from_raw(300_000));
        assert_eq!(Nanos::from_raw(1_000) + period * 2, Nanos::from_raw(201_000));
        assert_eq!(DurationUs::from_raw(-5) * 4, DurationUs::from_raw(-20));
    }

    /// A period from a rate: 3 Hz rounds to 333_333_333 ns, not 333_333_332 —
    /// truncating each tick would walk a chunk's anchor away from its clock.
    #[test]
    fn ns_period_from_float_seconds_rounds() {
        assert_eq!(DurationNs::from_secs_f64(1.0 / 3.0), DurationNs::from_raw(333_333_333));
        assert_eq!(DurationNs::from_secs_f64(0.0000005), DurationNs::from_raw(500));
        assert_eq!(DurationNs::from_secs_f64(-1.5), DurationNs::from_raw(-1_500_000_000));
    }

    #[test]
    fn serde_is_the_plain_integer() {
        // Wire-identical to the bare i64 the newtypes replaced — for JSON and
        // for the bincode spool WAL alike.
        assert_eq!(serde_json::to_string(&Nanos::from_raw(42)).unwrap(), "42");
        assert_eq!(serde_json::from_str::<Nanos>("42").unwrap(), Nanos::from_raw(42));
        assert_eq!(serde_json::to_string(&DurationNs::from_raw(-7)).unwrap(), "-7");
        assert_eq!(
            serde_json::from_str::<Micros>("1753920000000000").unwrap(),
            Micros::from_raw(1_753_920_000_000_000)
        );
    }

    #[test]
    fn wall_clocks_agree_and_read_a_modern_epoch() {
        // Both views of the wall clock sit after 2020 and within a step of each
        // other — catches a unit slip in either read (µs vs ns is 1000× apart).
        let us = wall_now();
        let ns = wall_now_ns();
        let y2020_us = 1_577_836_800_000_000i64;
        assert!(us.raw_us() > y2020_us, "wall_now not epoch-µs: {}", us.raw_us());
        assert!(ns.raw_ns() > y2020_us * NS_PER_US, "wall_now_ns not epoch-ns: {}", ns.raw_ns());
        let drift = ns.to_micros_trunc() - us;
        assert!(drift.raw_us().abs() < US_PER_S, "reads disagree by {:?}", drift);
    }

    #[test]
    fn epoch_part_constructors_scale_exactly() {
        assert_eq!(
            Nanos::from_epoch_parts(1_696_000_000, 500_000_000),
            Nanos::from_raw(1_696_000_000_500_000_000)
        );
        assert_eq!(
            Nanos::from_epoch_secs_f64(1_696_000_000.5),
            Nanos::from_raw(1_696_000_000_500_000_000)
        );
    }

    #[test]
    fn duration_from_std_round_trips_ns() {
        let d = std::time::Duration::from_micros(1_500);
        assert_eq!(DurationNs::from_std(d), DurationNs::from_raw(1_500_000));
        assert_eq!(DurationNs::from_std(d).to_micros_trunc(), DurationUs::from_raw(1_500));
    }

    #[test]
    fn duration_from_float_seconds_rounds_both_signs() {
        // A time daemon's offset arrives as float seconds; µs is the finest unit
        // any of them report. Rounding is symmetric, so a −0.6 µs offset does
        // not read as 0 (which would make an undisciplined clock look perfect).
        assert_eq!(DurationUs::from_secs_f64(0.000_000_348), DurationUs::from_raw(0));
        assert_eq!(DurationUs::from_secs_f64(0.000_012_5), DurationUs::from_raw(13));
        assert_eq!(DurationUs::from_secs_f64(-0.000_012_5), DurationUs::from_raw(-13));
        assert_eq!(DurationUs::from_secs_f64(-1.5), DurationUs::from_raw(-1_500_000));
    }

    #[test]
    fn duration_from_std_truncates_to_ms() {
        // 2.9 ms floors to 2 — same truncation contract as the instant types.
        let d = std::time::Duration::from_micros(2_900);
        assert_eq!(DurationMs::from_std(d), DurationMs::from_raw(2));
    }
}
