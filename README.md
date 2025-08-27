# swl

A minimal tiling Wayland compositor inspired by dwl/dwm, built with Smithay and adapted from COSMIC Comp's robust backend.

## What it is

swl is a lightweight, performant Wayland compositor that brings dwm's simplicity to Wayland. It reuses the battle-tested KMS backend and input handling from COSMIC Comp while stripping away desktop environment features to focus on efficient window tiling.

## Core principles

- **Minimal**: Only essential tiling WM features - no desktop environment bloat
- **Performant**: Event-driven damage tracking from COSMIC Comp's rendering pipeline
- **Reliable**: Built on proven code from a production compositor
- **Simple**: Configuration through recompilation (dwm-style)

## Features

### Phase 1 (Current)
- KMS backend with hardware acceleration
- Basic window management
- Keyboard/pointer input
- Single workspace

### Planned
- Master/stack tiling layout
- Multiple workspaces/tags
- Configurable keybindings
- Status bar protocol support
- Rules system for window placement

## Building

```bash
cargo build --release
```

## Running

```bash
# From TTY (not inside another Wayland/X11 session)
./target/release/swl
```

## Configuration

Following dwm philosophy - edit `config.rs` and recompile.

## Requirements

- Rust 1.85 (2024 edition)
- libinput
- libgbm
- libudev
- libseat

## Status

Early development. Extracting and simplifying COSMIC Comp's backend piece by piece.

## License

GPL-3.0 (inherited from COSMIC Comp)