# Lunar Launcher

Lunar Launcher is a native Rust desktop launcher for Minecraft Java Edition, built around local offline profiles and a fast, animated interface.

## Features

- Native Windows desktop application with an animated moon splash screen
- Translucent interface, ambient particle effects and interactive project artwork
- Optional animated GIF backgrounds with synchronized frame timing
- Animation toggle for users who prefer a lighter interface
- Offline profile creation, selection, renaming and deletion
- Mojang's official Minecraft version manifest and installation flow
- Java detection and managed Java runtime installation
- Memory allocation and Minecraft performance presets
- Automatic Fabric + Sodium setup for vanilla launches
- Modrinth modpack, mod and resource-pack discovery and installation
- Saved servers and server status checks
- SHA-1 verification for downloaded game files

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

## Windows build

The GitHub Actions workflow produces a Windows x64 executable and a Linux x64 build. Open the repository's Actions tab, select the latest successful CI run, and download the `Lunar-Launcher-Windows-x64` artifact.

## Game data

Lunar Launcher stores game versions, libraries, assets and logs inside its application-data directory. Game output is appended to `minecraft/logs/lunar-game.log`.

## Disclaimer

Minecraft is a trademark of Mojang Studios. Lunar Launcher is an independent project and is not affiliated with or endorsed by Mojang or Microsoft.
