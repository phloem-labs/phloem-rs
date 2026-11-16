# phloem-sdk changes

## 0.1.0 (2026-11-16)

The first stable release. `pip install phloem`, `uv add phloem`,
`go get github.com/phloem-labs/phloem-go` and `phloem-sdk = "0.1.0"` all get it.

- **Every platform:** Python wheels for macOS (Apple silicon, Intel), Linux
  x64 and Arm64 (including Alpine and other musl-based images) and Windows x64.
- **Go** from `github.com/phloem-labs/phloem-go`: the compiled core ships
  inside the module, so `go get` is the whole install (macOS and Linux).
- **C and C++** from `github.com/phloem-labs/phloem-c`: one archive per
  platform on each release, with the headers and the static and shared
  libraries. On macOS the shared library is named `@rpath/libphloem.dylib`.
- **Typed Python:** the wheel carries type information for editors and type
  checkers (`mypy --strict` clean).
- **Clearer errors:** a program the daemon has not allowed yet is told
  "refused by the Phloem daemon", with how to allow it.

## 0.1.0-rc.1 — first release (2026-11-15)

The first release of the Phloem SDK, for Python, Rust, Go, C and C++, all at
one version. A pre-release: `pip install phloem` and `uv add phloem` skip it;
`pip install --pre phloem` and `phloem-sdk = "0.1.0-rc.1"` get it.

- **Send readings from your own code.** `log_now`, `log`, `log_many`,
  fixed-rate streams, uniform blocks, explicit samples, gaps, and channel
  declarations (unit and display type). Each reading becomes a named channel.
- **Two ways to send.** Through Phloem on this machine (Phloem Computer or the
  Phloem service on a server), which keeps data on disk through a network
  outage; or straight to the cloud with a workspace API key and your
  workspace's URL (`PHLOEM_API_KEY`, `PHLOEM_URL`). One call opens either.
  The Rust crate has the first way only for now.
- **Know when something was lost.** Closing waits until everything is
  delivered and fails with the exact count of readings that were not, so a CI
  job fails instead of passing silently.
- **One program per name** when sending straight to the cloud: a second
  program started under the same name takes over, and the first one stops
  and says why.
- **Timestamps always name their clock:** `host`, `source`, or
  `backdated:<anchor_us>`. There is no default.
- **Platforms:** Python 3.9+ wheels for macOS (Apple silicon, Intel) and
  Linux x64 in this pre-release; Linux Arm64 and Windows follow. Rust from
  crates.io (`phloem-sdk`, imported as `phloem`).
