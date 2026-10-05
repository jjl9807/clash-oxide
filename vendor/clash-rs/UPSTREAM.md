# Vendored clash-rs

Source: https://github.com/ibigbug/clash-rs
Commit: `0cf7ed5f6a99a6ea6d62afd9188963657f796d4c`

Included: `clash-lib`, `clash-dns`, `clash-netstack`, the unchanged upstream
workspace manifest, MIT license and upstream notices. Other workspace members,
examples, benches and test fixtures are omitted. Build from the Clash Oxide
workspace; this subset is not a standalone checkout of the upstream workspace.
The application's `Cargo.lock` pins transitive Git dependencies.

Local integration patches:

- Make only `create_components` public so the adapter can obtain the existing
  public `RuntimeComponents`. The `runner` module remains private.
- Add `Runner::wait_ready` and a TUN readiness/error channel. This reports
  initialization failure instead of treating a spawned task as a ready device.
  Other runners retain their behavior through an immediately-ready default.
- Add `ThreadSafeCacheFile::snapshot_yaml` to export the final cache. Existing
  public getters cannot enumerate all fake-IP mappings, and the periodic writer
  can miss immediate reloads. This keeps the core addition to six lines; atomic
  persistence remains in the adapter rather than changing the core's writer.
- Gate the Linux `TunDatagram` re-export on the `tproxy` feature, its only
  consumer. This fixes the TUN-only build without suppressing warnings.

The complete source patch is [clash-rs.patch](../../patches/clash-rs.patch).
It changes five Rust files (+37/-2 lines); no upstream manifests are modified.
It can be checked against the commit above with `git apply --check`.

Clash Oxide disables upstream automatic route management (`route-all: false`).
The daemon owns Linux policy routes and a crash recovery journal. Each engine
lifetime gets its own Tokio runtime in a thread within the daemon process.
The adapter joins that thread after runtime shutdown, including failed startup,
before creating a replacement engine. This also drops the TUN task/device, so
the upstream TUN `join()` implementation is unchanged. The adapter clears the
existing public interface/mark globals before starting with TUN disabled.
The native frontends never invoke the upstream external controller.

The core uses unstable compiler features. `.cargo/config.toml` scopes
`RUSTC_BOOTSTRAP` to these three crates; `rust-toolchain.toml` pins the tested
compiler. Update the compiler, core, TLS patches and lockfile together.
