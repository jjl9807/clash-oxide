# Development Guide

## Language and documentation

- Write all project documentation and source comments in English, including README files, this guide, doc comments, and comments in scripts and configuration files.
- Keep localized interface strings and Unicode test fixtures in their intended languages. The English documentation policy does not remove application translations.
- Keep `README.md` focused on the project introduction, major features, installation, and license. Put architecture, implementation constraints, development workflows, and validation details in this file.
- The project is in early, active development. Functional prototypes are being validated on Linux; cross-platform support is a project direction, not a claim that every platform already works.
- The application embeds its proxy core in a single executable. Builds still need compiler dependencies, and desktop builds use the host's graphics libraries and drivers.

## Architecture

Clash Oxide is a Rust workspace with a GPUI Kit GUI, a Ratatui TUI, an independent daemon, and an embedded clash-rs engine. The GUI uses native rendering. The daemon runs the engine in its own process rather than launching an external core executable.

| Path | Responsibility |
| --- | --- |
| `crates/app` | Single executable, subcommands, and optional GUI feature |
| `crates/model` | Shared state, commands, and IPC protocol |
| `crates/i18n` | Shared strings, language preferences, and diagnostic translation |
| `crates/client` | Background IPC, daemon lifecycle, search, sorting, history, and view preferences |
| `crates/engine-clash` | Embedded core adapter, configuration preparation, engine lifecycle, and snapshots |
| `crates/daemon` | Serialized configuration transactions, persistence, routes, system proxy, and Unix socket |
| `crates/gui` | Native desktop interface, themes, tray, and single-instance activation |
| `crates/tui` | Native terminal interface |
| `vendor/clash-rs` | Pinned upstream core and minimal integration patches |
| `packaging` | Linux service, desktop entry, and shared application icon |
| `scripts` | Installer and isolated integration checks |

GUI and TUI share the client and state model. The daemon serializes all mutations. IPC uses bounded JSON frames, timeouts, a Unix socket, and peer UID validation.

Each engine lifetime has a dedicated Tokio runtime on a thread inside the daemon. Shut down and join that runtime before replacing it, including after failed startup, so upstream background tasks and TUN devices do not survive a reload. Confirm mixed-listener and TUN readiness before reporting successful engine startup.

Upstream provenance and integration patches are documented in [vendor/clash-rs/UPSTREAM.md](vendor/clash-rs/UPSTREAM.md) and [patches/clash-rs.patch](patches/clash-rs.patch). Preserve upstream license notices. Update the compiler, core, TLS patches, and lockfile together when changing the pinned integration.

## Building

The tested environment is Debian 13 on x86_64. `rust-toolchain.toml` selects the tested compiler; the first build needs network access for Rust and Cargo dependencies. `.cargo/config.toml` scopes `RUSTC_BOOTSTRAP` to the three upstream crates that need unstable library features.

Common build tools and the Linux routing utility:

```bash
sudo apt install git build-essential cmake clang pkg-config protobuf-compiler iproute2
```

Additional graphics development libraries for the desktop build:

```bash
sudo apt install libfontconfig-dev libfreetype-dev libwayland-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libxcb-xkb-dev \
  libvulkan-dev libasound2-dev libudev-dev libxrandr-dev libxi-dev \
  libxcursor-dev libxinerama-dev libegl-dev libgbm-dev
```

```bash
# GUI, TUI, daemon, and ctl.
cargo build --release --locked

# TUI, daemon, and ctl without GUI compilation or linking.
cargo build --release --locked --no-default-features
```

Both variants produce `target/release/clash-oxide`; a later build replaces that executable. `--build-info` reports whether GUI support is included. The default workspace member is `crates/app`. Do not add `--workspace` to a headless build: that would also build the GUI crate.

The desktop build needs an X11 or Wayland session and a usable Vulkan driver. Its other subcommands still link against graphics runtime libraries; use the headless variant on minimal servers.

## Entrypoints and daemon lifecycle

```bash
# Open an interface, starting the daemon if needed.
target/release/clash-oxide gui
target/release/clash-oxide tui

# Run the daemon in the foreground for debugging.
target/release/clash-oxide daemon

# Stop the daemon after restoring application-owned network settings.
target/release/clash-oxide kill
```

Without a subcommand, the desktop build opens the GUI and the headless build opens the TUI. TUI requires an interactive terminal; automation uses `ctl`. A headless build reports an explicit error for `gui`.

GUI and TUI perform a startup check once. A successful IPC handshake connects to the existing daemon. Otherwise, start the installed user's systemd instance, or spawn the `daemon` subcommand from the current executable. A startup lock coordinates concurrent frontend launches. A spawned daemon detaches from the terminal and logs to `daemon.log` in the data directory; logs larger than 8 MiB rotate to `daemon.log.1` at startup. Systemd deployments use the journal.

The daemon starts its embedded engine automatically. It loads the active profile, or uses the built-in direct configuration when no profile has been imported. The default mixed listener is `127.0.0.1:7890`; system proxy and TUN default to off. The first import activates immediately; subsequent imports preserve the active profile until the user switches.

Invalid saved configurations and occupied ports leave the daemon reachable with a failed engine state. Allow users to fix the profile or port and reload. Do not silently substitute a direct configuration for a broken saved profile.

Closing either interface leaves the daemon running. `kill` performs a normal shutdown, cleans up network settings, releases listeners, and preserves profiles and preferences; it succeeds if the daemon is already absent. Existing interfaces show a disconnected state after shutdown. Waking an existing GUI must not restart the daemon. Explicit Reload, TUI F8, or a newly launched frontend may start it again. The service uses `Restart=on-failure`, so a normal `kill` does not trigger a restart.

GUI/TUI do not expose engine start/stop controls, and CLI has no `ctl start` or `ctl stop`. System proxy and TUN switches control interception of system traffic. Turning both off leaves the local HTTP/SOCKS listener and proxy operations usable. `ctl reload` reloads the engine and retries failed startup.

Global `--socket` and `--data-dir` options work before or after subcommands. Explicitly setting either, or `CLASH_OXIDE_SOCKET`, disables systemd discovery. With only `--data-dir`, use `control.sock` in that directory. Otherwise, prefer the installed service's `/run/clash-oxide-<uid>/control.sock`, then `$XDG_RUNTIME_DIR/clash-oxide/control.sock`.

`ctl` connects to an existing daemon and does not start one. `examples/direct.yaml` is a DIRECT/REJECT connectivity fixture; real usage requires a complete Clash YAML profile.

```bash
target/release/clash-oxide ctl import "$PWD/examples/direct.yaml" --name Demo
target/release/clash-oxide ctl status
target/release/clash-oxide ctl select Proxy DIRECT
target/release/clash-oxide kill
```

## Frontend behavior

The information architecture and core workflows reference [Clash Party](https://github.com/mihomo-party-org/clash-party) at commit `c0db37e8`, implemented with native GPUI Kit components. Both interfaces expose six pages: proxy groups, profiles, connections, rules, logs, and settings. Overrides, Sub-Store, resource management, and DNS editing are not implemented yet.

### Shared workflows

- Profiles support local YAML and remote YAML subscriptions, import, refresh, rename, switch, and confirmed removal. GUI also supports file selection and drag-and-drop. A failed import preserves the draft. Removing a profile does not delete its source file; the active profile must be switched away from before removal.
- Proxy groups support expansion, search, configuration/name/latency ordering, node selection, and individual or group latency tests. Testing runs in the background. Rule mode shows configured groups, Global mode shows GLOBAL, and Direct mode shows a direct-connection hint.
- Connections support active/closed views, search, time/download ordering, paused display updates, details, copying, and closing one or all connections. Pause only freezes display. Closed history consists of connections observed by that frontend session, not persistent core history.
- Rules retain original order and numbering and support search. Logs support search, level filtering, pause, clear, and copy.
- Persistent controls expose Rule/Global/Direct, system proxy, TUN, the active profile, traffic, and engine state. Settings contain the mixed port and latency-test URL.

Node sorting, expanded groups, and the test URL are shared between GUI and TUI in `$XDG_CONFIG_HOME/clash-oxide/view.json`. The default test URL is `https://www.gstatic.com/generate_204`; timeouts are 5 seconds per node and 60 seconds per test operation.

### GUI presentation and performance

- Use virtual rows for expanded proxy nodes and a uniform virtual list for rules. Render only visible rows. Keep cached row projections, stable engine group ordering, persistent scroll handles, and row heights consistent with the virtual layout.
- Keep right-side scrollbars visible on the proxy and rule pages, supporting track clicks and thumb dragging. Recompute node columns on resize. Periodic snapshots must not reorder groups or cause unsolicited scrolling.
- A successful operation updates component state directly; do not add success banners or toasts. Preserve visible errors and input drafts so users can recover.
- Selected profiles and nodes use background and border styling without redundant checkmark prefixes in their names.
- Use the same Adwaita semantic roles in light and dark modes. Reference [libadwaita's style variables](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1-latest/css-variables.html). Light mode uses white cards and a pale sidebar; dark mode uses layered dark gray surfaces. Selected buttons, checked switches, and navigation use the same accent blue. Nodes and active profiles use a blue-tinted surface.
- Use 6 px control radii and 12 px card/dialog radii. Keep hover, pressed, keyboard-focus, disabled, error, and warning states consistent. GPUI's persistent button selection uses its active color role; keep that role equal to the primary fill to avoid mismatched selection colors.
- General settings offer Follow system, Light, and Dark. Apply and save changes immediately. Follow system observes XDG Desktop Portal appearance changes and defaults to light when the desktop supplies no preference. GUI theme preferences live separately in `$XDG_CONFIG_HOME/clash-oxide/gui.json`; a failed save must preserve the previous choice and colors.
- The sidebar's connection indicator uses green when connected and red when disconnected, alongside the status text. Align the circle with the glyphs' optical center.
- GUI shortcuts are Ctrl+1 through Ctrl+6 for pages and Ctrl+F for search.

### Window and tray behavior

Use GPUI Kit's `TitleBar` for client-side window controls, dragging, double-click maximize, and restore. Respect system-owned decorations and platform availability of minimize/maximize buttons. Minimize keeps the taskbar entry. Closing a window hides it to the tray only when a host and a registered icon exist; without a tray host, closing exits the GUI while preserving the daemon.

The tray uses `tray-icon` with the Linux `ksni` backend over session D-Bus, without GTK/AppIndicator libraries. Its menu opens the window, reloads the engine, toggles system proxy/TUN, changes modes, quits the interface while keeping the proxy running, or stops the proxy and exits after cleanup.

GNOME may need an AppIndicator extension to provide a tray host. If a host disappears while the GUI is hidden, restore the window. GUI instances are scoped to the user, display session, and daemon socket; repeated launches activate the existing window, while a different explicit socket permits an independent instance.

Tray, X11 window, and installed desktop entry share `packaging/clash-oxide.png`. Preserve aspect ratio and transparency when resizing. Rebuild and reinstall after replacing the image.

### TUI interaction

Wide terminals have a persistent left control area; narrow terminals move navigation to the top. Minimum size is 48 columns by 16 rows. Support mouse clicks and wheels, Unicode text, arrow-key editing, Ctrl-U clearing, and terminal paste.

| Key | Action |
| --- | --- |
| F1-F6 | Proxy groups, profiles, connections, rules, logs, settings |
| F8 | Reload; explicitly reconnect if the daemon has exited |
| Tab / Shift-Tab, arrows | Move among controls, toolbar, and lists |
| Enter, Esc | Open actions/details or confirm; cancel or clear search |
| `/`, PgUp / PgDn | Search; page through lists |
| `n`, `r`, `d` | Import; refresh; confirm profile removal or connection closure |
| Space | Pause/resume connection or log display |
| `m`, `t`, `s` | Change mode; toggle TUN; toggle system proxy |
| `?`, Ctrl-C | Help; exit the interface |

## Persistence and localization

Data lives in `$XDG_DATA_HOME/clash-oxide`, defaulting to `~/.local/share/clash-oxide`. Directory permissions are 0700; imported YAML and state files use 0600. Store original YAML separately from application-owned port/TUN settings. Subscription URLs remain in private daemon state and are not sent to frontends. Persist node selections per profile.

GUI, TUI, tray, and CLI share `oxide-i18n`, backed by `rust-i18n`. English and Simplified Chinese resources are embedded in the executable; no separate language files are needed at runtime. GPUI Kit built-in strings use the same language.

Settings offer Follow system, English, and Simplified Chinese. Changes refresh the current interface and GUI tray immediately, without restarting the daemon or proxy. Save the choice in `$XDG_CONFIG_HOME/clash-oxide/frontend.json` for subsequent frontend launches. Other already-running frontends do not automatically change their language.

```bash
clash-oxide --lang zh-CN gui
clash-oxide tui --lang en
clash-oxide --lang auto --help
```

Language precedence is `--lang`, `CLASH_OXIDE_LANG`, saved preference, then system locale. CLI and environment overrides affect only the current session. Explicit language overrides also reach an existing GUI when it is activated. Follow system reads `LC_ALL`, `LC_MESSAGES`, and `LANG`, respecting the `LANGUAGE` priority list. C/POSIX and unsupported locales fall back to English.

Keep JSON fields, IPC commands, proxy mode values, and user input stable across languages. IPC errors include `diagnostic` with code, arguments, and original details; snapshots may include `last_diagnostic`, preserving legacy `error` and `last_error` strings. The development protocol remains v2, without released cross-version compatibility. Translate common operation errors; preserve original core logs, configuration warnings, and low-level diagnostics. Clap's parser errors and some built-in help remain English.

When adding application strings, use matching keys and `%{argument}` placeholders in `crates/i18n/locales/en.json` and `zh-CN.json`, and call `oxide_i18n::tr!`. Never infer program state or IPC commands from displayed English text. Unit checks validate resource keys, placeholders, and embedding.

## Linux services and networking

Ordinary proxy use runs as the user. TUN uses a system service with `CAP_NET_ADMIN` and `CAP_NET_RAW`; GUI and TUI remain unprivileged.

```bash
# Build release first; installation requires administrator authorization.
sudo bash scripts/install-linux.sh

# Opening an interface starts the installed service when needed.
clash-oxide tui

# Enable TUN through the controls or CLI.
clash-oxide ctl tun on
```

The installer installs the executable, a systemd unit, and a Polkit rule allowing a user to start only their own service. Desktop builds additionally install the menu entry and icon. Installation does not enable boot startup. Passwordless service startup requires Polkit; on a server without it, start the service with `sudo systemctl start "clash-oxide@$(id -u).service"`, or install Polkit and rerun the installer.

Installation stops the ordinary daemon at its default socket. Stop custom-socket instances explicitly with `clash-oxide kill --socket <path>` before replacing the executable. Service data defaults to the user's `~/.local/share/clash-oxide`; `/etc/clash-oxide/<uid>.env` can override it and is preserved on upgrades. Reopen frontends after installation. Optional boot startup uses `sudo systemctl enable --now "clash-oxide@$(id -u).service"`.

TUN owns device `oxide0`, routing table/socket mark `20260`, policy priorities `20258-20261`, and routing protocol `186`. Reject conflicting resources. Route default IPv4/IPv6 traffic through TUN while preserving more specific routes such as LAN routes. The core intercepts port 53 DNS; engine outbound sockets bypass TUN using marks. The current implementation requires IPv6 enabled and permits only one Clash Oxide TUN instance per host.

Write recovery records before route changes. Normal shutdown, the next daemon startup, and systemd `ExecStopPost` clean up application-owned rules. Recover a manually launched daemon killed with SIGKILL by restarting it or running `clash-oxide daemon cleanup`. Never flush the system routing table.

System proxy currently supports GNOME-compatible GSettings and requires user session D-Bus. Save existing values before enabling it and restore them on shutdown. Preserve changes made by other applications. KDE system-proxy integration and prebuilt release packages are not implemented yet.

```bash
sudo systemctl status "clash-oxide@$(id -u).service"
journalctl -u "clash-oxide@$(id -u).service" -f
```

## Configuration compatibility and limits

- Imports must provide complete Clash/Mihomo YAML. Single-node `ss://`/`vmess://` links and Base64 subscription conversion are unsupported.
- Compatibility follows the pinned clash-rs revision, not complete Mihomo parity. The build enables Shadowsocks and TUN. The core also contains VMess, VLESS/REALITY, Trojan, Hysteria2, and SOCKS5 implementations and an HTTP/SOCKS mixed inbound, but public-server interoperability has not been validated for every protocol. Optional TUIC, WireGuard, and SSH features are not enabled.
- Mixed inbound binds to `127.0.0.1`, using the application port setting. Configured external controllers, other ports, authentication, and external DNS listeners are not enabled. Reject custom listeners and inbound providers explicitly.
- Application settings own TUN behavior rather than copying Mihomo's auto-route fields. Reject list-valued `nameserver-policy` entries to avoid silently changing semantics.
- Restrict HTTP-provider caches to the profile directory. Local profiles may use file providers with absolute paths or paths relative to their source file. Remote subscriptions must not reference local file providers.
- Limits are 2 MiB per import and 100 profiles. Engine snapshots include at most 500 active connections and 5,000 rules. Frontends retain at most 200 observed closed connections and 500 logs; the daemon log buffer holds 200 entries. State refreshes once per second; subscriptions refresh manually.
- Port, mode, and TUN changes rebuild the engine and interrupt connections. If a new configuration fails to start, attempt to restore the last working one.

## Validation

Run checks appropriate to the changed behavior. Documentation-only changes need content, command, path, and link verification rather than application rebuilds or integration suites.

```bash
cargo fmt -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p oxide-client -p oxide-i18n -p oxide-engine-clash -p oxide-daemon --locked
cargo build --workspace --locked
python3 scripts/smoke-test.py
python3 scripts/workflow-test.py
python3 scripts/always-on-test.py
python3 scripts/lifecycle-test.py
python3 scripts/i18n-test.py
unshare -Urnm python3 scripts/service-lifecycle-test.py
python3 scripts/packaging-test.py
unshare -Urn python3 scripts/tun-test.py
python3 scripts/system-proxy-test.py
python3 scripts/ui-party-test.py
python3 scripts/gui-theme-test.py
python3 scripts/proxy-order-test.py --gui
python3 scripts/gui-scroll-test.py
python3 scripts/window-controls-test.py
python3 scripts/tray-test.py
```

Tests use `target/debug/clash-oxide`, overridable with `CLASH_OXIDE_TEST_BINARY`. Set `CLASH_OXIDE_LANG=en` for checks that assert English UI text; localization and tray suites isolate their own preferences.

Keep integration checks isolated from the user's running daemon and network settings. Ordinary smoke checks use loopback only. Service discovery checks use private mount namespaces and a `systemctl` substitute. Installation checks redirect destinations into temporary directories. System-proxy checks use private D-Bus and dconf. TUN checks use isolated network namespaces; they require user-namespace support and `/dev/net/tun`.

| Check | Coverage and additional tools |
| --- | --- |
| `smoke-test.py` | Real daemon IPC, HTTP, and SOCKS over loopback |
| `workflow-test.py` | Profile operations, latency tests, connection closure, failed input recovery, and saved preferences |
| `always-on-test.py` | Direct startup, first-import transactions, saved configuration recovery, and port/configuration failure recovery |
| `lifecycle-test.py` | Frontend startup, concurrency, process detachment, shutdown cleanup, and no implicit restart from existing interfaces |
| `i18n-test.py` | CLI/IPC and TUI language changes, persistence, and Unicode input |
| `service-lifecycle-test.py` | Service discovery and ownership behavior in private namespaces |
| `packaging-test.py` | Desktop/headless installation and scoped Polkit rules; requires `node` and `desktop-file-validate` |
| `tun-test.py` | IPv4/IPv6 TCP/UDP, DNS, subscriptions, and crash recovery in isolated namespaces |
| `system-proxy-test.py` | Save/restore and external modification handling; requires `dbus-run-session`, `gsettings`, and dconf |
| `ui-party-test.py` | Native GUI/TUI workflows and recovery; requires `Xvfb`, `xdotool`, and ImageMagick; supports `--tui-only` |
| `gui-theme-test.py` | Light/dark pages and dialogs, selected-control colors, theme choices, live portal changes, restart, save recovery, and narrow Chinese layouts; additionally requires Python `dbus` and `gi` |
| `proxy-order-test.py --gui` | Stable group order after subscription import, snapshots, and engine rebuilds; compares collapsed, expanded, and manually scrolled native frames |
| `gui-scroll-test.py` | 5,000 rules and 3,840 expanded nodes; wheels, track/thumb input, search, node selection, and resize |
| `window-controls-test.py` | Dragging, double-clicking, minimize/maximize/restore/close, and daemon survival; additionally requires `xfwm4`, `xfconf`, and `xprop` |
| `tray-test.py` | Real SNI registration, menus, taskbar identity, activation, close-to-tray, host loss/restart, no automatic restart after kill, and both exit modes; requires Python `dbus` and `gi` |

`ui-smoke-test.py` is a compatibility entrypoint for the UI workflow suite. UI screenshots live in `target/ui-party`, `target/gui-theme`, `target/proxy-order`, and `target/gui-scroll`. `gui-scroll-test.py --benchmark-only` measures wheel-sequence wall time and GUI CPU; measurements include capture and Xvfb software rendering overhead.
