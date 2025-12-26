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

## Run & use the local web console

The app ships with a local YA-WebADB console so you can work without visiting `tangoapp.dev`.

1) **Start the bridge**
   ```sh
   cargo run --release
   ```
   The tray icon appears and two servers start: the bridge on port `15037` and the web console on port `8000`.

2) **Open the console**
   - The tray app automatically opens `http://localhost:8000/?desktop=true` on first launch.
   - You can also open it manually in your browser.
   - The YA-WebADB assets are embedded in the binary, so `tango_bridge.exe` runs the console
     without any extra Node.js server or copied `web/` folder.

3) **Connect to your device**
   - Enable USB debugging on the Android device and plug it into the computer.
   - In the console, keep the default WebSocket endpoint `ws://localhost:15037/bridge/` and click **Connect**.

4) **Use the tools**
   - **Device overview** shows connection status and basic info once paired.
   - **Screen Mirror** streams video via scrcpy; click inside the canvas to interact.
   - **Shell** lets you run simple commands (e.g., `getprop ro.build.version.release`).

All traffic stays on your machine; the console talks to the local bridge over WebSocket.

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
