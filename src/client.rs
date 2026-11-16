//! Layer 3 — the CLIENT: what a program holds. One surface over either route:
//! the Phloem daemon on this machine (the local door), or straight to the cloud
//! with an API key (the direct route, feature `direct`). The frames are the
//! encoder's either way; only where they go differs.

use std::path::{Path, PathBuf};

use crate::encode::{ChannelDecl, Column, Encoder, Error, GapExtent, Step, TimeBasis, Value};
use crate::local;
use crate::stream::{Pushed, UniformStream};

/// Which way a [`Client`]'s data goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Through the Phloem daemon on this machine: it stamps, keeps and uploads.
    /// Nothing is lost while the network is down, and this program holds no
    /// key.
    Local,
    /// Straight to the cloud with an API key, from memory: for CI jobs,
    /// simulations and notebooks, where there is no Phloem on the machine.
    Direct,
}

impl Route {
    pub fn as_str(self) -> &'static str {
        match self {
            Route::Local => "local",
            Route::Direct => "direct",
        }
    }
}

/// How to open a [`Client`] — [`Client::open`].
///
/// The route is chosen by what is given: an API key (here, or in
/// `PHLOEM_API_KEY`) means [`Route::Direct`]; none means [`Route::Local`].
/// [`route`](Self::route) forces one. The direct route needs the workspace's
/// URL beside the key ([`url`](Self::url), or `PHLOEM_URL`).
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Who is sending: the producer name the daemon shows, or the program's
    /// device name in the cloud.
    pub name: String,
    /// Force a route instead of choosing by the key.
    pub route: Option<Route>,
    /// The workspace API key (direct). `None` reads `PHLOEM_API_KEY`.
    pub api_key: Option<String>,
    /// The workspace's URL (direct): its environment's API address, shown
    /// beside the API key on the account page. `None` reads `PHLOEM_URL`.
    /// There is no default: a key with no URL is refused.
    pub url: Option<String>,
    /// The daemon's door (local). `None` is its default location.
    pub socket: Option<PathBuf>,
    /// The clock the producer's stamps come off (local): a serial, a bus id.
    pub clock_id: Option<String>,
    /// The binding naming itself (`python/0.1.0`). `None` is this crate.
    pub sdk: Option<String>,
    /// Direct: how much unconfirmed data to hold in memory before a write
    /// waits (or drops, under [`drop_when_full`](Self::drop_when_full)).
    /// `None` is 64 MiB.
    pub buffer_bytes: Option<usize>,
    /// Direct: at a full buffer, drop the new record and count it instead of
    /// waiting. `close` then reports the count.
    pub drop_when_full: bool,
    /// Direct: how long `close` waits for the cloud to confirm what is left.
    /// `None` is 30 s.
    pub close_timeout_ms: Option<u64>,
}

impl Options {
    pub fn named(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Default::default() }
    }
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// The route [`Client::open`] takes for these options, and why — decided
/// before anything is opened, so a binding can say it.
pub(crate) fn choose(o: &Options) -> (Route, Option<String>) {
    let key = o.api_key.clone().filter(|k| !k.trim().is_empty()).or_else(|| env("PHLOEM_API_KEY"));
    let route = o.route.unwrap_or(if key.is_some() { Route::Direct } else { Route::Local });
    (route, key)
}

enum Enc {
    Local(Encoder<local::LocalWriter>),
    #[cfg(feature = "direct")]
    // Boxed: the direct sink is several times the local writer's size, and
    // a client is one or the other for its whole life.
    Direct(Box<Encoder<crate::direct::Sink>>),
}

/// Run `$body` on whichever encoder the client holds.
macro_rules! enc {
    ($self:expr, $e:ident => $body:expr) => {
        match &mut $self.enc {
            Enc::Local($e) => $body,
            #[cfg(feature = "direct")]
            Enc::Direct($e) => $body,
        }
    };
}

/// A program's connection to Phloem: to the daemon on this machine, or direct
/// to the cloud. See [`Client::open`].
pub struct Client {
    enc: Enc,
    /// Local: the input (permit) the connection publishes under. Direct: the
    /// program's device id.
    permit: String,
}

impl Client {
    /// Open a client the way `options` say: direct when there is an API key,
    /// else the Phloem daemon on this machine.
    pub fn open(options: Options) -> Result<Self, Error> {
        if options.name.trim().is_empty() {
            return Err(Error::Usage("a client needs a name: who is sending".into()));
        }
        let (route, key) = choose(&options);
        let sdk = options
            .sdk
            .clone()
            .unwrap_or_else(|| concat!("rust/", env!("CARGO_PKG_VERSION")).into());
        match route {
            Route::Local => Self::connect_as_with_clock(
                options.socket.as_deref(),
                &options.name,
                &sdk,
                options.clock_id.as_deref(),
            ),
            Route::Direct => {
                let Some(key) = key else {
                    return Err(Error::Usage(
                        "the direct route needs an API key: set PHLOEM_API_KEY, or pass one".into(),
                    ));
                };
                // Decided here, before anything opens: a key says nothing
                // about where its workspace runs.
                let url = options
                    .url
                    .clone()
                    .map(|u| u.trim().to_string())
                    .filter(|u| !u.is_empty())
                    .or_else(|| env("PHLOEM_URL"));
                let Some(url) = url else {
                    return Err(Error::Usage(
                        "an API key needs its workspace's URL beside it: set PHLOEM_URL (the \
                         URL shown beside the API key on the account page), or pass it"
                            .into(),
                    ));
                };
                Self::open_direct(&options, url, key, &sdk)
            }
        }
    }

    #[cfg(feature = "direct")]
    fn open_direct(o: &Options, url: String, key: String, sdk: &str) -> Result<Self, Error> {
        let mut cfg = crate::direct::Config::new(url, key, o.name.clone());
        if let Some(b) = o.buffer_bytes {
            cfg.buffer_bytes = b;
        }
        cfg.drop_when_full = o.drop_when_full;
        if let Some(ms) = o.close_timeout_ms {
            cfg.close_timeout = std::time::Duration::from_millis(ms);
        }
        Self::direct(cfg, sdk)
    }

    #[cfg(not(feature = "direct"))]
    fn open_direct(_o: &Options, _url: String, _key: String, _sdk: &str) -> Result<Self, Error> {
        Err(Error::Usage(
            "this build of the SDK has no direct route (it was built without the `direct` \
             feature): send through the Phloem on this machine, or use a build that has it"
                .into(),
        ))
    }

    /// Open the direct route with an explicit [`Config`](crate::direct::Config).
    #[cfg(feature = "direct")]
    pub fn direct(mut cfg: crate::direct::Config, sdk: &str) -> Result<Self, Error> {
        cfg.sdk = sdk.to_string();
        let name = cfg.name.clone();
        let sink = crate::direct::Sink::connect(cfg)?;
        let permit = sink.device_id().to_string();
        let mut enc = Encoder::new(sink);
        enc.hello(&name, sdk)?;
        Ok(Self { enc: Enc::Direct(Box::new(enc)), permit })
    }

    /// Connect to the daemon at `path` (or [`default_socket_path`](crate::default_socket_path)), say
    /// hello as `producer`, and wait for the daemon's verdict.
    pub fn connect(path: Option<&Path>, producer: &str) -> Result<Self, Error> {
        Self::connect_as(path, producer, concat!("rust/", env!("CARGO_PKG_VERSION")))
    }

    /// [`connect`](Self::connect), stating the clock the producer's stamps
    /// come off ([`Encoder::hello_with_clock`]): a device serial, a bus id —
    /// something that names the same clock across reconnects. The daemon then
    /// keeps what it learned of that clock's drift when the program
    /// reconnects; without it, a reconnect re-learns it from scratch.
    pub fn connect_with_clock(
        path: Option<&Path>,
        producer: &str,
        clock_id: &str,
    ) -> Result<Self, Error> {
        Self::connect_as_with_clock(
            path,
            producer,
            concat!("rust/", env!("CARGO_PKG_VERSION")),
            Some(clock_id),
        )
    }

    /// [`connect`](Self::connect) with the binding naming itself (`python/0.1.0`).
    pub fn connect_as(path: Option<&Path>, producer: &str, sdk: &str) -> Result<Self, Error> {
        Self::connect_as_with_clock(path, producer, sdk, None)
    }

    /// [`connect_as`](Self::connect_as) with an optional clock id — the one
    /// entry every binding's local route goes through. An empty id is refused
    /// before the socket is opened.
    pub fn connect_as_with_clock(
        path: Option<&Path>,
        producer: &str,
        sdk: &str,
        clock_id: Option<&str>,
    ) -> Result<Self, Error> {
        let (enc, permit) = local::connect(path, producer, sdk, clock_id)?;
        Ok(Self { enc: Enc::Local(enc), permit })
    }

    /// Which way this client's data goes.
    pub fn route(&self) -> Route {
        match &self.enc {
            Enc::Local(_) => Route::Local,
            #[cfg(feature = "direct")]
            Enc::Direct(_) => Route::Direct,
        }
    }

    /// Local: the input (permit) this connection publishes under. Direct: the
    /// program's device id in the cloud.
    pub fn permit(&self) -> &str {
        &self.permit
    }

    pub fn declare(&mut self, channels: &[ChannelDecl]) -> Result<(), Error> {
        enc!(self, e => e.declare(channels))
    }
    pub fn uniform(
        &mut self,
        t0_ns: i64,
        step: Step,
        basis: TimeBasis,
        columns: &[Column],
    ) -> Result<(), Error> {
        enc!(self, e => e.uniform(t0_ns, step, basis, columns))
    }
    pub fn explicit(
        &mut self,
        basis: TimeBasis,
        samples: &[(String, Option<i64>, Value)],
    ) -> Result<(), Error> {
        enc!(self, e => e.explicit(basis, samples))
    }
    pub fn gap(
        &mut self,
        paths: &[String],
        frontier_ns: i64,
        extent: GapExtent,
        detail: &str,
    ) -> Result<(), Error> {
        enc!(self, e => e.gap(paths, frontier_ns, extent, detail))
    }
    /// The next frame of `stream` — see [`Encoder::push`].
    pub fn push(
        &mut self,
        stream: &mut UniformStream,
        columns: &[Column],
    ) -> Result<Pushed, Error> {
        enc!(self, e => e.push(stream, columns))
    }
    /// [`push`](Self::push) with the host clock's reading given.
    pub fn push_at(
        &mut self,
        stream: &mut UniformStream,
        columns: &[Column],
        now_ns: i64,
    ) -> Result<Pushed, Error> {
        enc!(self, e => e.push_at(stream, columns, now_ns))
    }

    /// **One reading, now.** `value` at `path`, stamped with this machine's
    /// clock as the call is made ([`TimeBasis::Host`]). The three-line start;
    /// a program that knows its instants uses [`log`](Self::log) or
    /// [`log_many`](Self::log_many), and a fixed-rate one a [`UniformStream`].
    pub fn log_now(&mut self, path: &str, value: Value) -> Result<(), Error> {
        let now = crate::time::wall_now_ns().raw_ns();
        self.explicit(TimeBasis::Host, &[(path.to_string(), Some(now), value)])
    }

    /// One reading at its own instant (`t_ns` on the stated basis).
    pub fn log(
        &mut self,
        path: &str,
        t_ns: i64,
        basis: TimeBasis,
        value: Value,
    ) -> Result<(), Error> {
        self.explicit(basis, &[(path.to_string(), Some(t_ns), value)])
    }

    /// Many readings of one channel, each at its own instant. `times_ns` and
    /// `values` are the same length; the basis is stated, as everywhere.
    pub fn log_many(
        &mut self,
        path: &str,
        basis: TimeBasis,
        times_ns: &[i64],
        values: &[f64],
    ) -> Result<(), Error> {
        if times_ns.len() != values.len() {
            return Err(Error::Usage(format!(
                "`{path}`: {} instants for {} values",
                times_ns.len(),
                values.len()
            )));
        }
        let samples: Vec<(String, Option<i64>, Value)> = times_ns
            .iter()
            .zip(values)
            .map(|(t, v)| (path.to_string(), Some(*t), Value::F64(*v)))
            .collect();
        self.explicit(basis, &samples)
    }

    /// Host-clocked frames moved forward onto the previous end.
    pub fn continued(&self) -> u64 {
        match &self.enc {
            Enc::Local(e) => e.continued(),
            #[cfg(feature = "direct")]
            Enc::Direct(e) => e.continued(),
        }
    }
    /// Push buffered frames on. Local: to the daemon. Direct: asks the
    /// uploader to send what it holds now; it does not wait for the cloud
    /// ([`close`](Self::close) does).
    pub fn flush(&mut self) -> Result<(), Error> {
        enc!(self, e => e.flush())
    }
    /// END the stream ([`Encoder::end`]), flush, and close.
    ///
    /// Local: the end frame is what tells the daemon this producer stopped by
    /// choice — a connection that closes without it (a dropped client, a
    /// killed process) is a cut, and the outage of its channels is stated
    /// (`producer_cut`).
    ///
    /// Direct: waits (up to the close timeout) for the cloud to confirm
    /// everything sent, and answers [`Error::Unsent`] with the count if
    /// anything was not — what a CI job turns into a non-zero exit.
    pub fn close(mut self) -> Result<(), Error> {
        match &mut self.enc {
            Enc::Local(e) => {
                e.end()?;
                e.flush()?;
                local::shut_write(e.get_ref());
                Ok(())
            }
            #[cfg(feature = "direct")]
            Enc::Direct(e) => {
                e.end()?;
                e.sink_mut().close()
            }
        }
    }
}

/// A dropped client flushes, silently, and does NOT end the stream: a drop is
/// also what a panic unwinding does, and a producer that did not finish must
/// read as cut. Ending is [`Client::close`]'s, said on purpose.
impl Drop for Client {
    fn drop(&mut self) {
        let _ = enc!(self, e => e.flush());
    }
}
