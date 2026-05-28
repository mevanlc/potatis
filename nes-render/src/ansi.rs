use crate::Rgb;

pub const CURSOR_HOME: &str = "\x1b[H";
pub const CURSOR_HOME_BYTES: &[u8] = CURSOR_HOME.as_bytes();
pub const CLEAR: &str = "\x1b[2J";

/// SGR color escape helpers. Two color depths are supported: indexed 256-color
/// (widest compatibility, used for netcat clients) and 24-bit truecolor
/// (higher fidelity, used by the local TUI).
pub struct Ansi;

impl Ansi {
  pub fn open_fg_256(rgb: Rgb) -> String {
    let index = ansi_colours::ansi256_from_rgb(rgb);
    format!("\x1b[38;5;{}m", index)
  }

  pub fn open_bg_256(rgb: Rgb) -> String {
    let index = ansi_colours::ansi256_from_rgb(rgb);
    format!("\x1b[48;5;{}m", index)
  }

  pub fn open_fg_true(rgb: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
  }

  pub fn open_bg_true(rgb: Rgb) -> String {
    format!("\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
  }
}
