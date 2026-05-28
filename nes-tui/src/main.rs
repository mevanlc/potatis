mod host;

use std::io;

use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use clap::ValueEnum;
use crossterm::cursor::Hide;
use crossterm::cursor::Show;
use crossterm::event::KeyboardEnhancementFlags;
use crossterm::event::PopKeyboardEnhancementFlags;
use crossterm::event::PushKeyboardEnhancementFlags;
use crossterm::execute;
use crossterm::terminal::disable_raw_mode;
use crossterm::terminal::enable_raw_mode;
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use host::TuiHost;
use nes::cartridge::Cartridge;
use nes::nes::Nes;
use nes_render::ColorDepth;
use nes_render::HalfblockRenderer;
use nes_render::KittyRenderer;
use nes_render::Renderer;
use nes_render::SixelRenderer;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Graphics {
  /// Sixel image protocol (iTerm2, xterm, foot, WezTerm, ...).
  Sixel,
  /// Kitty graphics protocol (kitty, Ghostty, WezTerm, ...).
  Kitty,
  /// Unicode upper-half-block characters with 24-bit color. Works anywhere.
  Halfblock,
}

impl Graphics {
  fn renderer(self, scale: u32) -> Box<dyn Renderer> {
    match self {
      Graphics::Sixel => Box::new(SixelRenderer::with_scale(scale)),
      Graphics::Kitty => Box::new(KittyRenderer::with_scale(scale)),
      // Halfblock has a fixed pixel-pair-per-cell mapping; scale is ignored.
      Graphics::Halfblock => Box::new(HalfblockRenderer::new(ColorDepth::Truecolor)),
    }
  }

  fn fps(self) -> usize {
    match self {
      // Sixel re-encodes a PNG per frame and can't sustain 60fps; capping keeps
      // the emulation running at a sane wall-clock speed rather than thrashing.
      Graphics::Sixel => 15,
      Graphics::Kitty | Graphics::Halfblock => 60,
    }
  }
}

#[derive(Parser, Debug)]
#[command(
  name = "nes-tui",
  about = "Play NES games locally in your terminal",
  version
)]
struct Args {
  /// Graphics protocol to render with.
  #[arg(short = 'g', long = "graphics", value_enum)]
  graphics: Graphics,

  /// Integer pixel scale for sixel and kitty modes (ignored for halfblock).
  /// 1 = native NES resolution, 3 = 3x, etc.
  #[arg(short = 's', long = "scale", default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..=8))]
  scale: u32,

  /// Path to a .nes ROM file.
  rom: std::path::PathBuf,
}

/// RAII guard that puts the terminal into a clean full-screen raw state and
/// restores it on drop — including on panic or `?` unwind, so the user is never
/// left in a broken terminal.
struct Terminal;

impl Terminal {
  fn enter() -> Result<Self> {
    enable_raw_mode().context("enabling raw mode")?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, Hide, Clear(ClearType::All))?;
    // Request key release/repeat reporting. Terminals without the Kitty keyboard
    // protocol ignore this; TuiHost falls back to timeout-based key release.
    let _ = execute!(
      out,
      PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
    );
    Ok(Self)
  }
}

impl Drop for Terminal {
  fn drop(&mut self) {
    let mut out = io::stdout();
    let _ = execute!(out, PopKeyboardEnhancementFlags, Show, LeaveAlternateScreen);
    let _ = disable_raw_mode();
  }
}

fn main() -> Result<()> {
  let args = Args::parse();

  let cart = Cartridge::blow_dust(args.rom.clone())
    .map_err(|e| anyhow::anyhow!("failed to load ROM {}: {e}", args.rom.display()))?;

  // Enter raw/alt-screen mode only after the ROM loads, so a bad path prints a
  // normal error instead of a cleared screen. `_terminal` is declared before
  // `nes` so it is dropped *after* the host flushes its output buffer.
  let _terminal = Terminal::enter()?;

  let host = TuiHost::new(io::stdout(), args.graphics.renderer(args.scale));
  let mut nes = Nes::insert(cart, host);
  nes.fps_max(args.graphics.fps());

  while nes.powered_on() {
    nes.tick();
  }

  Ok(())
}
