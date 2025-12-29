# Tango Bridge

Tango Bridge is a system-tray application that can start ADB and forward connections to Tango Web App running in a browser.

An ADB executable for each platform is included.

## Build

### Prerequisites

- [Rust](https://rustup.rs/)

### Windows

```sh
cargo build --release
```

### macOS

Xcode is required to build aarch64 (arm64) version.

```sh
cargo build --release
```

### Linux

gtk3 and libappindicator3 are required to build the project.

Arch Linux / Manjaro:

```sh
sudo pacman -S gtk3 libappindicator-gtk3
```

Ubuntu / Debian:

```sh
sudo apt install libgtk-3-dev libappindicator3-dev
```

```sh
cargo build --release
```

## Allowing additional web origins

The Bridge WebSocket server is configured with a small default allowlist of web origins. If you need to expose the bridge through a reverse proxy or a Cloudflare Tunnel with a custom domain, add your origins as a comma-separated list in the `TANGO_BRIDGE_ALLOWED_ORIGINS` environment variable before starting the app:

```sh
export TANGO_BRIDGE_ALLOWED_ORIGINS="https://ws.mydomain.com,https://other.example.com"
cargo run --release
```

Origins that cannot be parsed will be ignored with a warning in the logs.
