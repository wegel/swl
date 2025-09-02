# swl

A minimal tiling Wayland compositor inspired by dwl/dwm and built with Smithay.

## Core principles

- **Minimal**: Only essential tiling WM features - no desktop environment bloat
- **Performant**: Event-driven damage tracking

## Features

- KMS backend with hardware acceleration
- Master/stack tiling layout
- Keyboard/pointer input
- Workspaces
- Tabbed mode
- Standard wayland protocols support

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

Currently: edit the code and recompile.

## Requirements

- Rust 1.85 (2024 edition)
- libinput
- libgbm
- libudev
- libseat

## Status

Early development.

## TODO

- XWayland support
- Mouse-driven resize/move for floating windows
- Config file format (replace env vars)
- Virtual outputs
- Smooth cursor transition between physical outputs

## License

GPL-3.0

