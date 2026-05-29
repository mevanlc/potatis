//! In-process chafa renderer using libchafa via the `chafa-sys` FFI crate.
//!
//! Wins over the subprocess [`crate::ChafaRenderer`]:
//!   * No `fork`/`exec` per frame — easily hits 60 fps where the subprocess
//!     mode caps out around 20.
//!   * No PNG encode round-trip — we hand chafa the raw RGB888 buffer.
//!   * No terminal capability probes / device-attribute replies to mop up out
//!     of stdin (the bug this whole module exists to sidestep at the source).
//!   * Cleaner shutdown — nothing to wait on.
//!
//! Cost: a build-time dependency on libchafa headers (>= 1.16) and `bindgen`.
//! See `nes-render/build.rs` for the version guard.
//!
//! Lifetime / ownership notes (these are easy to get wrong with C libs):
//!   * Chafa objects live in chafa's allocator. We `_unref` them in Drop in
//!     the reverse order they were created.
//!   * `chafa_canvas_print` returns a glib `GString`. We copy its bytes into
//!     a Rust `Vec` and then call `g_string_free(s, TRUE)` to release both the
//!     struct and its backing buffer via glib's allocator. **Do not** wrap
//!     it in a `CString::from_raw` — that would have Rust's allocator attempt
//!     to free a glib-allocated buffer on drop (the upstream `chafa` safe
//!     wrapper crate has this bug; we don't).

use std::ffi::CString;
use std::ptr;

use chafa_sys as cs;
use nes::frame::RenderFrame;

use crate::ansi;
use crate::renderers::ChafaOpts;
use crate::renderers::Renderer;

const NEWLINE: &[u8] = b"\r\n";

pub struct ChafaLibRenderer {
  opts: ChafaOpts,
  // Cached chafa objects — built once at construction and rebuilt on resize
  // (since terminal geometry feeds into chafa_calc_canvas_geometry).
  symbol_map: *mut cs::ChafaSymbolMap,
  config: *mut cs::ChafaCanvasConfig,
  canvas: *mut cs::ChafaCanvas,
  // Terminal info — detected from the process env once. Drives which escape
  // sequences chafa_canvas_print emits.
  term_info: *mut cs::ChafaTermInfo,
  // Current terminal cell dimensions. Defaults to 80x24 until on_resize is
  // called (TuiHost calls it at startup and on every Resize event).
  cols: u16,
  rows: u16,
}

impl ChafaLibRenderer {
  pub fn new(opts: ChafaOpts) -> Self {
    // Detect terminal capabilities once. `chafa_term_db_get_default()` returns
    // a process-wide pointer (do not unref); `chafa_term_db_detect` walks the
    // env (TERM, COLORTERM, ...) and returns a fresh ChafaTermInfo* which we
    // own and must unref.
    //
    // Subtle: passing NULL for `envp` does **not** mean "use the process
    // environment" — it means "use an empty env", and chafa then falls back
    // to its most-conservative term_info (no color, no fancy glyphs), which
    // produces a wall of `█` characters with zero color escapes. chafa's own
    // CLI calls `g_get_environ()` to materialize the env into a glib-owned
    // `gchar**`; we do the same and `g_strfreev` it afterward.
    let term_info = unsafe {
      let db = cs::chafa_term_db_get_default();
      let envp = cs::g_get_environ();
      let ti = cs::chafa_term_db_detect(db, envp);
      cs::g_strfreev(envp);
      ti
    };
    let mut this = Self {
      opts,
      symbol_map: ptr::null_mut(),
      config: ptr::null_mut(),
      canvas: ptr::null_mut(),
      term_info,
      cols: 80,
      rows: 24,
    };
    this.rebuild_canvas();
    this
  }

  /// Recompute the canvas for current `cols`/`rows`/`opts`. Called at
  /// construction and on every resize. Cheap enough to do per-resize, too
  /// expensive to do per-frame.
  fn rebuild_canvas(&mut self) {
    unsafe {
      // Tear down any previous canvas/config/symbol-map (reverse creation
      // order). Drop also does this, but we need it here for resize.
      if !self.canvas.is_null() {
        cs::chafa_canvas_unref(self.canvas);
        self.canvas = ptr::null_mut();
      }
      if !self.config.is_null() {
        cs::chafa_canvas_config_unref(self.config);
        self.config = ptr::null_mut();
      }
      if !self.symbol_map.is_null() {
        cs::chafa_symbol_map_unref(self.symbol_map);
        self.symbol_map = ptr::null_mut();
      }

      // Symbol map. The chafa CLI's `--symbols X` semantics are "clear, then
      // add X" — `apply_selectors` alone is *additive* on whatever's already
      // in the map, so we need to either start empty or prepend `"none,"` to
      // the selector. We do the latter so users can still use combinators
      // like `"vhalf,octant"`.
      let symbol_map = cs::chafa_symbol_map_new();
      if let Some(sel) = &self.opts.symbols {
        let selector_str = format!("none,{sel}");
        if let Ok(c) = CString::new(selector_str) {
          let mut err: *mut cs::GError = ptr::null_mut();
          let ok = cs::chafa_symbol_map_apply_selectors(symbol_map, c.as_ptr(), &mut err);
          // Free any GError so we don't leak; on failure, fall back to ALL.
          if !err.is_null() {
            cs::g_error_free(err);
          }
          if ok == 0 {
            cs::chafa_symbol_map_add_by_tags(
              symbol_map,
              cs::ChafaSymbolTags_CHAFA_SYMBOL_TAG_ALL,
            );
          }
        }
      } else {
        // No user-supplied selector — match the safe-wrapper / CLI default of
        // "everything's allowed".
        cs::chafa_symbol_map_add_by_tags(
          symbol_map,
          cs::ChafaSymbolTags_CHAFA_SYMBOL_TAG_ALL,
        );
      }

      // Compute the canvas geometry that best fits our terminal. font_ratio
      // 0.5 = monospace cells are twice as tall as wide (standard 1:2).
      let mut dest_w = self.cols.max(1) as cs::gint;
      let mut dest_h = self.rows.max(1) as cs::gint;
      cs::chafa_calc_canvas_geometry(
        nes::frame::NTSC_WIDTH as cs::gint,
        nes::frame::NTSC_HEIGHT as cs::gint,
        &mut dest_w,
        &mut dest_h,
        0.5,
        0, // zoom
        0, // stretch
      );
      // Honor `scale=N`: shrink the fitted geometry by N. `scale=max` (and the
      // unspecified default) leave the fitted geometry alone, matching the
      // CLI's `--scale max` behavior.
      if let Some(s) = &self.opts.scale {
        if s != "max" {
          if let Ok(n) = s.parse::<f32>() {
            if n > 0.0 {
              dest_w = ((dest_w as f32) / n).max(1.0) as cs::gint;
              dest_h = ((dest_h as f32) / n).max(1.0) as cs::gint;
            }
          }
        }
      }

      // Build the canvas config.
      let config = cs::chafa_canvas_config_new();
      cs::chafa_canvas_config_set_geometry(config, dest_w, dest_h);
      cs::chafa_canvas_config_set_symbol_map(config, symbol_map);

      // Canvas mode: defer to chafa's detection of what the host terminal can
      // emit. Hardcoding TRUECOLOR was wrong — terminals whose detected
      // term_info can't emit truecolor (Terminal.app reports `indexed-240`;
      // default tmux reports `indexed-256`) then get NO color escapes at all
      // out of chafa_canvas_print, since the term_info doesn't know how to
      // serialize the requested colors. Letting chafa pick reproduces the
      // `CHAFA_CANVAS_MODE` line of `chafa --dump-detect`.
      let canvas_mode = cs::chafa_term_info_get_best_canvas_mode(self.term_info);
      cs::chafa_canvas_config_set_canvas_mode(config, canvas_mode);

      // Pixel mode. Default to SYMBOLS — the whole point of `-g chafa` is to
      // get chafa's cell-based rendering, distinct from the image-protocol
      // modes (`-g kitty`, `-g sixel`) we already have. Don't auto-switch
      // based on detection: a user picking `-g chafa` on a kitty terminal
      // explicitly *didn't* pick `-g kitty`. They can opt into the graphics
      // protocols via `f=kitty|sixels|iterm` if they want chafa to drive them.
      let pixel_mode = match self.opts.format.as_deref() {
        Some("symbols") => cs::ChafaPixelMode_CHAFA_PIXEL_MODE_SYMBOLS,
        Some("sixels") => cs::ChafaPixelMode_CHAFA_PIXEL_MODE_SIXELS,
        Some("kitty") => cs::ChafaPixelMode_CHAFA_PIXEL_MODE_KITTY,
        Some("iterm") | Some("iterm2") => cs::ChafaPixelMode_CHAFA_PIXEL_MODE_ITERM2,
        _ => cs::ChafaPixelMode_CHAFA_PIXEL_MODE_SYMBOLS,
      };
      cs::chafa_canvas_config_set_pixel_mode(config, pixel_mode);

      // Passthrough: image-protocol pixel modes (kitty/sixel/iterm2) need
      // tmux/screen wrapper guards when running inside a multiplexer, or the
      // escapes get eaten by the multiplexer instead of forwarded. chafa
      // detects whether we're inside tmux/screen and which guards to use.
      let passthrough = cs::chafa_term_info_get_passthrough_type(self.term_info);
      cs::chafa_canvas_config_set_passthrough(config, passthrough);

      // Work factor: higher = better quality, slower symbol selection. Unset
      // by default — chafa picks a sensible middle ground.
      if let Some(w) = self.opts.work {
        cs::chafa_canvas_config_set_work_factor(config, w);
      }

      let canvas = cs::chafa_canvas_new(config);

      self.symbol_map = symbol_map;
      self.config = config;
      self.canvas = canvas;
    }
  }
}

impl Drop for ChafaLibRenderer {
  fn drop(&mut self) {
    unsafe {
      if !self.canvas.is_null() {
        cs::chafa_canvas_unref(self.canvas);
      }
      if !self.config.is_null() {
        cs::chafa_canvas_config_unref(self.config);
      }
      if !self.symbol_map.is_null() {
        cs::chafa_symbol_map_unref(self.symbol_map);
      }
      if !self.term_info.is_null() {
        cs::chafa_term_info_unref(self.term_info);
      }
    }
  }
}

impl Renderer for ChafaLibRenderer {
  fn render(&mut self, frame: &RenderFrame) -> Vec<u8> {
    // Defensive: a previous on_resize on a degenerate geometry could have left
    // us without a canvas. Emit nothing rather than dereferencing null.
    if self.canvas.is_null() {
      return Vec::new();
    }

    // Hand chafa the tightly-packed RGB888 NES frame.
    let pixels: Vec<u8> = frame.pixels_ntsc().collect();
    let pw = nes::frame::NTSC_WIDTH as cs::gint;
    let ph = nes::frame::NTSC_HEIGHT as cs::gint;
    unsafe {
      cs::chafa_canvas_draw_all_pixels(
        self.canvas,
        cs::ChafaPixelType_CHAFA_PIXEL_RGB8,
        pixels.as_ptr(),
        pw,
        ph,
        pw * 3,
      );
    }

    // Produce escape stream. Returns a glib GString we must free.
    let gstring = unsafe { cs::chafa_canvas_print(self.canvas, self.term_info) };
    if gstring.is_null() {
      return Vec::new();
    }
    // Copy bytes out before freeing. Do NOT take ownership of `str_` with a
    // Rust type — it's glib-allocated, not Rust-allocated.
    let bytes = unsafe {
      let raw = (*gstring).str_ as *const u8;
      let len = (*gstring).len as usize;
      let slice = std::slice::from_raw_parts(raw, len);
      let owned = slice.to_vec();
      // TRUE: free the backing character buffer too (vs. just the GString
      // struct). We're keeping our own copy, so this is correct.
      cs::g_string_free(gstring, 1);
      owned
    };

    // Prefix with cursor-home; translate LF → CRLF for raw-mode terminals.
    let mut buf = Vec::with_capacity(ansi::CURSOR_HOME_BYTES.len() + bytes.len() + 32);
    buf.extend_from_slice(ansi::CURSOR_HOME_BYTES);
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
      if b == b'\n' {
        buf.extend_from_slice(&bytes[start..i]);
        buf.extend_from_slice(NEWLINE);
        start = i + 1;
      }
    }
    buf.extend_from_slice(&bytes[start..]);
    buf
  }

  fn on_resize(&mut self, cols: u16, rows: u16) {
    self.cols = cols.max(1);
    self.rows = rows.max(1);
    self.rebuild_canvas();
  }
}

#[cfg(test)]
mod tests {
  use nes::frame::PixelFormatRGB888;
  use nes::frame::RenderFrame;

  use super::ChafaLibRenderer;
  use super::ChafaOpts;
  use super::Renderer;

  fn fixture_frame() -> RenderFrame {
    let buf888 = include_bytes!("../tests/frame_888_pal.bin");
    let mut frame888 = RenderFrame::new::<PixelFormatRGB888>();
    frame888.replace_buf(buf888);
    frame888
  }

  #[test]
  fn renders_default_symbols() {
    let frame = fixture_frame();
    let mut r = ChafaLibRenderer::new(ChafaOpts::default());
    r.on_resize(120, 40);
    let bytes = r.render(&frame);
    // Smoke: non-empty output that starts with our cursor-home prefix.
    assert!(!bytes.is_empty(), "render produced empty output");
    assert!(
      bytes.starts_with(b"\x1b["),
      "expected ANSI escape prefix, got: {:?}",
      &bytes[..bytes.len().min(8)]
    );
  }

  /// Regression guard: chafa's `term_db_detect(db, NULL)` returns the most
  /// conservative term_info (no color, no glyphs), producing a wall of `█`
  /// with zero SGR codes. We pass `g_get_environ()` so chafa sees the real
  /// terminal env — assert that color escapes actually make it out.
  #[test]
  fn emits_color_escapes() {
    let frame = fixture_frame();
    let mut r = ChafaLibRenderer::new(ChafaOpts::default());
    r.on_resize(120, 40);
    let bytes = r.render(&frame);
    let text = String::from_utf8_lossy(&bytes);
    let truecolor = text.matches("\x1b[38;2;").count();
    let indexed = text.matches("\x1b[38;5;").count();
    // Either 24-bit or 256-color FG codes — either means chafa saw a usable
    // term_info and emitted color. The exact split depends on the CI
    // terminal's TERM/COLORTERM.
    assert!(
      truecolor + indexed > 100,
      "expected lots of color FG escapes, got truecolor={truecolor} indexed={indexed}"
    );
  }

  #[test]
  fn resize_rebuilds_without_panic() {
    let frame = fixture_frame();
    let mut r = ChafaLibRenderer::new(ChafaOpts {
      symbols: Some("octant".into()),
      scale: Some("max".into()),
      ..Default::default()
    });
    r.on_resize(80, 24);
    let small = r.render(&frame).len();
    r.on_resize(200, 60);
    let big = r.render(&frame).len();
    // Larger canvas should produce more cells → more bytes.
    assert!(big > small, "expected resize to grow output: {small} -> {big}");
  }

  #[test]
  fn scale_n_shrinks_output() {
    let frame = fixture_frame();
    let mut r = ChafaLibRenderer::new(ChafaOpts {
      scale: Some("max".into()),
      ..Default::default()
    });
    r.on_resize(160, 50);
    let at_max = r.render(&frame).len();
    let mut r2 = ChafaLibRenderer::new(ChafaOpts {
      scale: Some("2".into()),
      ..Default::default()
    });
    r2.on_resize(160, 50);
    let at_2 = r2.render(&frame).len();
    assert!(at_2 < at_max, "scale=2 should shrink vs scale=max: {at_2} vs {at_max}");
  }
}
