# Clash Oxide

Fullstack Rust-native proxy client — no WebView, no external core, with first-class GUI and TUI for desktop and headless environments.

> [!IMPORTANT]
> Clash Oxide is in early, active development. Functional prototypes are currently being validated on Linux.

## Features

- **Rust-native from end to end.** A lightweight native stack built for performance, with no WebView.
- **Self-contained deployment.** The GUI, TUI, daemon, and embedded proxy core ship in one executable, with no separate core or application runtime to install.
- **Cross-platform by design.** A shared Rust foundation for Linux, macOS, and Windows; platform validation currently focuses on Linux.
- **First-class GUI and TUI.** Use the native desktop interface or manage your proxy from a terminal, including on headless servers.

## Install

### Download from release

Prebuilt packages are not available yet. Packaging and automated release publishing are planned. Once available, download the package for your operating system from [GitHub Releases](https://github.com/jjl9807/clash-oxide/releases).

### Build from source

Source builds have been validated on Debian 13 (x86_64). Install Rust through `rustup`; the repository selects its tested toolchain automatically.

Install the build prerequisites:

```bash
sudo apt install git build-essential cmake clang pkg-config protobuf-compiler iproute2
```

For the desktop build, also install the graphics development libraries:

```bash
sudo apt install libfontconfig-dev libfreetype-dev libwayland-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libxcb-xkb-dev \
  libvulkan-dev libasound2-dev libudev-dev libxrandr-dev libxi-dev \
  libxcursor-dev libxinerama-dev libegl-dev libgbm-dev
```

Clone and build:

```bash
git clone https://github.com/jjl9807/clash-oxide.git
cd clash-oxide
cargo build --release --locked
```

Launch either interface:

```bash
./target/release/clash-oxide gui
./target/release/clash-oxide tui
```

The GUI requires X11 or Wayland and a working Vulkan driver. For a headless server, build without GUI dependencies and launch the TUI:

```bash
cargo build --release --locked --no-default-features
./target/release/clash-oxide tui
```

Both builds produce `target/release/clash-oxide`. On Linux with systemd, the optional installer sets up the system service needed for TUN; a desktop build also installs a menu entry and icon:

```bash
sudo bash scripts/install-linux.sh
```

## License

This project is licensed under the [MIT License](LICENSE).
