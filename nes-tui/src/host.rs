use std::collections::HashMap;
use std::io::BufWriter;
use std::io::Stdout;
use std::io::Write;
use std::time::Duration;
use std::time::Instant;

use crossterm::event;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use nes::frame::RenderFrame;
use nes::joypad::Joypad;
use nes::joypad::JoypadButton;
use nes::joypad::JoypadEvent;
use nes::nes::HostEvent;
use nes::nes::HostPlatform;
use nes_render::Renderer;

// Terminals that don't support the Kitty keyboard protocol only report key
// presses, never releases. So a held button looks like a burst of presses
// (auto-repeat) followed by silence. We auto-release a button this long after
// its last press; key auto-repeat refreshes it in the meantime. Terminals that
// *do* report releases (kitty, Ghostty, ...) release immediately and never hit
// this timeout.
const RELEASE_AFTER: Duration = Duration::from_millis(120);

// Erase the whole screen (ED 2). Emitted before a forced redraw so a resize
// doesn't leave the previous frame's cells/image lingering around the edges.
const CLEAR_SCREEN: &[u8] = b"\x1b[2J";

/// [`HostPlatform`] that renders into the local terminal and reads input via
/// crossterm. Assumes the terminal is already in raw mode (see `Terminal` in
/// `main.rs`).
pub struct TuiHost {
  out: BufWriter<Stdout>,
  renderer: Box<dyn Renderer>,
  start: Instant,
  // CRC of the last frame written, to skip re-emitting identical frames.
  crc: u32,
  // Set when the terminal is resized: forces the next frame to be re-emitted
  // (clearing first) even if its content is byte-identical to the last one.
  force_redraw: bool,
  pressed: HashMap<JoypadButton, Instant>,
  shutdown: bool,
}

impl TuiHost {
  pub fn new(out: Stdout, renderer: Box<dyn Renderer>) -> Self {
    Self {
      out: BufWriter::with_capacity(256 * 1024, out),
      renderer,
      start: Instant::now(),
      crc: 0,
      force_redraw: false,
      pressed: HashMap::new(),
      shutdown: false,
    }
  }

  fn map_button(code: KeyCode) -> Option<JoypadButton> {
    match code {
      KeyCode::Char('w') | KeyCode::Char('W') => Some(JoypadButton::UP),
      KeyCode::Char('s') | KeyCode::Char('S') => Some(JoypadButton::DOWN),
      KeyCode::Char('a') | KeyCode::Char('A') => Some(JoypadButton::LEFT),
      KeyCode::Char('d') | KeyCode::Char('D') => Some(JoypadButton::RIGHT),
      KeyCode::Char('l') | KeyCode::Char('L') => Some(JoypadButton::A),
      KeyCode::Char('k') | KeyCode::Char('K') => Some(JoypadButton::B),
      KeyCode::Char(' ') => Some(JoypadButton::SELECT),
      KeyCode::Enter => Some(JoypadButton::START),
      _ => None,
    }
  }

  fn press(&mut self, joypad: &mut Joypad, button: JoypadButton) {
    self.pressed.insert(button, Instant::now());
    joypad.on_event(JoypadEvent::Press(button));
  }

  fn release(&mut self, joypad: &mut Joypad, button: JoypadButton) {
    if self.pressed.remove(&button).is_some() {
      joypad.on_event(JoypadEvent::Release(button));
    }
  }

  fn release_expired(&mut self, joypad: &mut Joypad) {
    let expired: Vec<JoypadButton> = self
      .pressed
      .iter()
      .filter(|(_, at)| at.elapsed() >= RELEASE_AFTER)
      .map(|(b, _)| *b)
      .collect();
    for button in expired {
      self.release(joypad, button);
    }
  }
}

impl HostPlatform for TuiHost {
  fn render(&mut self, frame: &RenderFrame) {
    let bytes = self.renderer.render(frame);
    let crc = crc32fast::hash(&bytes);
    if self.force_redraw {
      let _ = self.out.write_all(CLEAR_SCREEN);
      let _ = self.out.write_all(&bytes);
      let _ = self.out.flush();
      self.crc = crc;
      self.force_redraw = false;
    } else if crc != self.crc {
      let _ = self.out.write_all(&bytes);
      let _ = self.out.flush();
      self.crc = crc;
    }
  }

  fn poll_events(&mut self, joypad: &mut Joypad) -> HostEvent {
    let mut reset = false;

    // Drain every event queued since the last frame without blocking.
    while event::poll(Duration::ZERO).unwrap_or(false) {
      let key = match event::read() {
        Ok(Event::Key(key)) => key,
        // A resize clears/reflows the terminal; force the next frame to be
        // re-emitted even if its content is unchanged.
        Ok(Event::Resize(_, _)) => {
          self.force_redraw = true;
          continue;
        }
        _ => continue,
      };

      // Quit: Esc or Ctrl-C (raw mode means we never get a SIGINT).
      let ctrl_c = key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
      if key.code == KeyCode::Esc || ctrl_c {
        self.shutdown = true;
        continue;
      }

      if matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R')) {
        reset = true;
        continue;
      }

      let Some(button) = Self::map_button(key.code) else {
        continue;
      };

      match key.kind {
        KeyEventKind::Press | KeyEventKind::Repeat => self.press(joypad, button),
        KeyEventKind::Release => self.release(joypad, button),
      }
    }

    self.release_expired(joypad);

    if self.shutdown {
      HostEvent::Shutdown
    } else if reset {
      HostEvent::Reset
    } else {
      HostEvent::Nothing
    }
  }

  fn elapsed_millis(&self) -> usize {
    self.start.elapsed().as_millis() as usize
  }

  fn delay(&self, d: Duration) {
    std::thread::sleep(d);
  }
}
