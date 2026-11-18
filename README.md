# LUNAR

LUNAR is a native Rust Minecraft launcher focused on offline profiles, reliable installation workflows, and a refined desktop experience.

## Current implementation

- Native desktop shell built with egui/eframe
- Persistent offline profile creation, selection, editing, and deletion
- Minecraft Java Edition version manifest retrieval from Mojang's official metadata endpoint
- Release and snapshot version filtering
- Background network work so the interface remains responsive
- Local application data stored in the operating system's application-data directory
- Automated CI checks and release builds for Windows, Linux, and macOS

Offline profiles are local launcher identities. They do not authenticate with Mojang or Microsoft and do not grant access to online-mode servers or paid game entitlements.

## Development

Install the stable Rust toolchain, then run:

```sh
cargo run
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
cargo build --release
```

## Data

LUNAR stores its own settings and offline profiles under the platform-specific application data directory. Minecraft game files are not bundled.

## Status

This repository is under active development. The current build establishes the native application foundation, offline profile persistence, and official version metadata integration. Full version installation, Java runtime management, game launching, mod loaders, and server diagnostics are subsequent implementation milestones.

## Disclaimer

Minecraft is a trademark of Mojang Studios. LUNAR is an independent project and is not affiliated with or endorsed by Mojang or Microsoft.
