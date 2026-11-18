# LUNAR

LUNAR is a native Rust Minecraft Java Edition launcher built around local offline profiles and a focused desktop interface.

## Features

- Native desktop UI with Home, Play, Versions, Profiles and Settings screens
- Offline profile creation, selection, renaming, deletion and persistent storage
- Mojang's official version manifest with release and snapshot selection
- Downloads version metadata, the client JAR, libraries, native libraries and asset indexes
- SHA-1 verification for downloaded game files
- Parallel asset downloads with progress reporting
- Native library extraction with path traversal protection
- Java detection, memory allocation and process launching
- Captured game output written to the launcher game log
- Local game data stored in the operating system's application-data directory

Offline profiles are local identities only. They do not authenticate with Mojang or Microsoft, bypass game ownership, or grant access to online-mode servers. Use a legitimate Minecraft installation and only servers that permit offline-mode connections.

## Build

Install the stable Rust toolchain and the native graphics development packages required by eframe on your platform.

```sh
cargo run
cargo fmt --check
cargo check --all-targets
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

## Game data

LUNAR stores game versions, libraries, assets and logs inside its application-data directory under `minecraft`. The Play screen installs the selected official version before enabling launch. A compatible Java runtime must be installed and available on `PATH`.

Game output is appended to `minecraft/logs/lunar-game.log`.

## Current scope

The launcher currently targets vanilla Minecraft Java Edition. Fabric, Forge, NeoForge, modpack import, automatic Java runtime provisioning, Microsoft authentication, and server browsing are not included.

## Disclaimer

Minecraft is a trademark of Mojang Studios. LUNAR is an independent project and is not affiliated with or endorsed by Mojang or Microsoft.
