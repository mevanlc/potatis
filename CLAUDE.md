# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Potatis (🥔) is a multi-platform Nintendo Entertainment System (NES) emulator written in Rust. The project uses a workspace-based modular architecture with support for native SDL, WebAssembly, Android, embedded hardware (Raspberry Pi Pico), and cloud gaming deployments.

## Common Commands

### Building and Running
```bash
# Native SDL target
cargo run --release path/to/rom.nes
cargo run -- --help  # View command line options

# WebAssembly target
cd nes-wasm
wasm-pack build --release --target web
npm install
npm run dev

# Embedded target (RP-2040)
cd nes-embedded
ROM=/path/to/rom.nes cargo run --release

# Android target
cd nes-android && ./install.sh release
```

### Testing
```bash
cargo test                           # Run all tests
cargo test -p nes --test benchmark   # Performance benchmarks  
./benchmark.sh                       # Automated benchmark script
```

### Profiling and Analysis
```bash
./profile_mem.sh    # Memory profiling with dhat
./profile_perf.sh   # CPU profiling with samply (if available)
```

## Architecture

### Workspace Structure
- **`common/`** - Shared utilities and common code
- **`mos6502/`** - Generic MOS 6502 CPU emulator (passes all tests including illegal ops)
- **`nes/`** - Core NES emulation library with PPU, cartridge, and mapper support
- **`nes-sdl/`** - Native desktop target using SDL2
- **`nes-wasm/`** - WebAssembly target for browsers
- **`nes-android/`** - Android target using JNI
- **`nes-embedded/`** - Embedded target for RP-2040 (Raspberry Pi Pico)
- **`nes-cloud/`** - Cloud gaming server with terminal rendering
- **`profile/`** - Performance analysis crate

### Core Components

**CPU Emulation (`mos6502/`)**:
- Complete MOS 6502 implementation with optional debugger
- Breakpoint and memory watching capabilities  
- No-std compatible for embedded targets
- Provides nestest-compatible output formatting

**NES Emulation (`nes/`)**:
- PPU (Picture Processing Unit) implementation
- Cartridge loading and mapper support (NROM, MMC1, UxROM, CNROM, MMC3)
- Host platform abstraction pattern for cross-platform deployment
- Joypad input handling

### Platform Abstraction Pattern

The core `nes` crate uses a host platform trait for cross-platform support:

```rust
impl nes::HostPlatform for MyHost {
  fn render(&mut self, frame: &RenderFrame) {
    // frame.pixels() == 256 * 240 * 3 RGB array
  }
  fn poll_events(&mut self, joypad: &mut Joypad) {
    // pump events and forward to joypad  
  }
}
```

## Input Controls
- Movement: WASD
- A Button: L  
- B Button: K
- Select: Space
- Start: Enter
- Reset: R

## Performance Characteristics
- **Embedded (RP-Pico)**: 5 FPS, 135kB-243kB RAM usage
- **Desktop**: Full-speed emulation
- **Cloud**: Multiple rendering modes (Sixel, Unicode, ASCII)

## Test ROMs and Validation
The `/test-roms/` directory contains extensive NES test ROM collections for validation. Tests use `nestest.nes` and other standard validation ROMs to ensure accuracy.

## Current Limitations
- APU (Audio Processing Unit) not implemented
- Limited to 5 mapper types
- No save state functionality