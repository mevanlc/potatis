use std::fs::File;
use std::io::BufWriter;
use std::io::Read;
use std::io::Write;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use nes::frame::PixelFormat;
use nes::frame::PixelFormatRGB888;
use nes::frame::RenderFrame;

use crate::ansi;
use crate::ansi::Ansi;

const UPPER_BLOCK: &str = "\u{2580}";
// Row terminator. `\r\n` (not bare `\n`) so frames line up under a terminal in
// raw mode, where the kernel no longer maps NL->CRNL. Harmless on cooked TTYs.
const NEWLINE: &str = "\r\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
  /// Unicode upper-half-block (▀) with fg/bg colors. Two terminal rows per char.
  Halfblock,
  /// Luminance-mapped ASCII art. No color.
  Ascii,
  /// Sixel image (requires a Sixel-capable terminal).
  Sixel,
  /// Kitty graphics protocol image (kitty, Ghostty, WezTerm, ...).
  Kitty,
}

/// Color fidelity for the [`HalfblockRenderer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
  /// Indexed 256-color. Widest terminal/netcat compatibility.
  Ansi256,
  /// 24-bit truecolor. Best fidelity for local terminals.
  Truecolor,
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl ansi_colours::AsRGB for Rgb {
  fn as_u32(&self) -> u32 {
    let mut i = (self.0 as u32) << 16;
    i |= (self.1 as u32) << 8;
    i |= self.2 as u32;
    i
  }
}

pub trait Renderer {
  /// Render `frame` to terminal bytes. Output is prefixed with a cursor-home so
  /// successive frames overdraw in place.
  fn render(&mut self, frame: &RenderFrame) -> Vec<u8>;

  /// Notify the renderer of the current terminal size in cells. Called once at
  /// startup and again on every resize. Most renderers don't care; the chafa
  /// renderer uses it to drive `--view-size`. Default: no-op.
  fn on_resize(&mut self, _cols: u16, _rows: u16) {}
}

/// Construct a renderer for `mode`. Halfblock defaults to 256-color for maximum
/// compatibility; callers wanting truecolor should build [`HalfblockRenderer`]
/// directly.
pub fn create(mode: RenderMode) -> Box<dyn Renderer> {
  match mode {
    RenderMode::Halfblock => Box::new(HalfblockRenderer::new(ColorDepth::Ansi256)),
    RenderMode::Ascii => Box::new(AsciiRenderer::new()),
    RenderMode::Sixel => Box::new(SixelRenderer::new()),
    RenderMode::Kitty => Box::new(KittyRenderer::new()),
  }
}

pub struct SixelRenderer {
  sixel: sixel_rs::encoder::Encoder,
  buf: File,
}

impl SixelRenderer {
  /// Default 3x scale, matching the long-standing nes-cloud behavior.
  pub fn new() -> Self {
    Self::with_scale(3)
  }

  /// Integer pixel scale (1 = native NTSC 240x224, 3 = 720x672, ...). Applied
  /// by libsixel as a percent post-scale of the encoded PNG.
  pub fn with_scale(scale: u32) -> Self {
    let outfile = tempfile::Builder::new().prefix("sixel").tempfile().unwrap();
    let percent = (scale.max(1) as u64) * 100;

    let sixel = sixel_rs::encoder::Encoder::new().unwrap();
    sixel.set_quality(sixel_rs::optflags::Quality::Low).unwrap();
    sixel.set_output(outfile.path()).unwrap();
    sixel
      .set_height(sixel_rs::optflags::SizeSpecification::Percent(percent))
      .unwrap();
    sixel
      .set_width(sixel_rs::optflags::SizeSpecification::Percent(percent))
      .unwrap();

    Self {
      sixel,
      buf: outfile.into_file(),
    }
  }
}

impl Default for SixelRenderer {
  fn default() -> Self {
    Self::new()
  }
}

impl Renderer for SixelRenderer {
  fn render(&mut self, frame: &RenderFrame) -> Vec<u8> {
    self.buf.set_len(0).unwrap();

    // TODO: Avoid creating a new file here. Reuse old tmp.
    let infile = tempfile::Builder::new().prefix("frame").tempfile().unwrap();
    let inpath = infile.path().to_owned();

    let w = &mut BufWriter::new(infile);
    let mut png = png::Encoder::new(
      w,
      nes::frame::NTSC_WIDTH as u32,
      nes::frame::NTSC_HEIGHT as u32,
    );
    png.set_color(png::ColorType::Rgb);
    png.set_depth(png::BitDepth::Eight);
    let mut writer = png.write_header().unwrap();
    let pixels: Vec<u8> = frame.pixels_ntsc().collect();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();

    self.sixel.encode_file(&inpath).unwrap();

    let mut buf = ansi::CURSOR_HOME_BYTES.to_vec();
    self.buf.read_to_end(&mut buf).unwrap();
    buf
  }
}

pub struct HalfblockRenderer {
  buf: String,
  depth: ColorDepth,
}

impl HalfblockRenderer {
  const COLS: usize = nes::frame::NTSC_WIDTH;
  const ROWS: usize = nes::frame::NTSC_HEIGHT;

  pub fn new(depth: ColorDepth) -> Self {
    HalfblockRenderer {
      buf: String::with_capacity(160000),
      depth,
    }
  }

  fn open_fg(&self, rgb: Rgb) -> String {
    match self.depth {
      ColorDepth::Ansi256 => Ansi::open_fg_256(rgb),
      ColorDepth::Truecolor => Ansi::open_fg_true(rgb),
    }
  }

  fn open_bg(&self, rgb: Rgb) -> String {
    match self.depth {
      ColorDepth::Ansi256 => Ansi::open_bg_256(rgb),
      ColorDepth::Truecolor => Ansi::open_bg_true(rgb),
    }
  }
}

impl Renderer for HalfblockRenderer {
  fn render(&mut self, frame: &RenderFrame) -> Vec<u8> {
    self.buf.clear();
    self.buf.push_str(ansi::CURSOR_HOME);

    let p: Vec<u8> = frame.pixels_ntsc().collect();
    let mut c_upper: Option<Rgb> = None;
    let mut c_lower: Option<Rgb> = None;
    for row in (0..Self::ROWS).step_by(2) {
      for col in 0..Self::COLS {
        let upper_i = ((row * Self::COLS) + col) * PixelFormatRGB888::BYTES_PER_PIXEL;
        let upper = Rgb(p[upper_i], p[upper_i + 1], p[upper_i + 2]);

        let lower_i = (((row + 1) * Self::COLS) + col) * PixelFormatRGB888::BYTES_PER_PIXEL;
        let lower = Rgb(p[lower_i], p[lower_i + 1], p[lower_i + 2]);

        if Some(upper) != c_upper {
          self.buf.push_str(&self.open_fg(upper));
          c_upper = Some(upper);
        }

        if Some(lower) != c_lower {
          self.buf.push_str(&self.open_bg(lower));
          c_lower = Some(lower);
        }

        self.buf.push_str(UPPER_BLOCK);
      }

      self.buf.push_str(NEWLINE);
    }

    self.buf.as_bytes().to_vec()
  }
}

pub struct AsciiRenderer {
  buf: String,
}

impl AsciiRenderer {
  const CHARSET: &'static str = " .-`',:_;~\"/!|\\i^trc*v?s()+lj1=e{[]z}<xo7f>aJy3Iun542b6Lw9k#dghq80VpT$YACSFPUZ%mEGXNO&DKBR@HQWM";
  const MAX: f64 = Self::CHARSET.len() as f64;

  pub fn new() -> Self {
    Self {
      buf: String::with_capacity(50000),
    }
  }
}

impl Default for AsciiRenderer {
  fn default() -> Self {
    Self::new()
  }
}

impl Renderer for AsciiRenderer {
  fn render(&mut self, frame: &RenderFrame) -> Vec<u8> {
    self.buf.clear();
    self.buf.push_str(ansi::CURSOR_HOME);

    frame
      .pixels_ntsc()
      .collect::<Vec<u8>>()
      .chunks_exact(nes::frame::PixelFormatRGB888::BYTES_PER_PIXEL)
      .enumerate()
      .for_each(|(n, p)| {
        // https://stackoverflow.com/questions/596216/formula-to-determine-perceived-brightness-of-rgb-color
        let g: f64 =
          ((0.2126 * p[0] as f64) + (0.7152 * p[1] as f64) + (0.0722 * p[2] as f64)) / 255.0;
        let i = ((Self::MAX * g) + 0.5).floor();
        let c = Self::CHARSET.chars().nth(i as usize).unwrap_or('.');

        if n % nes::frame::NTSC_WIDTH == 0 {
          self.buf.push_str(NEWLINE);
        }
        self.buf.push(c);
      });

    self.buf.as_bytes().to_vec()
  }
}

/// Renders frames using the Kitty graphics protocol.
///
/// Each frame is transmitted as a 24-bit RGB image under a fixed image id
/// (`i=1`) with `a=T` (transmit + display). Re-using the id makes the terminal
/// replace the previous image in place rather than accumulating placements, and
/// `q=2` suppresses the terminal's acknowledgement so we don't have to drain it
/// from stdin.
/// Tunable knobs for [`KittyRenderer`].
#[derive(Debug, Clone, Copy)]
pub struct KittyOpts {
  /// Integer pixel scale (nearest-neighbor upscaled before transmit).
  pub scale: u32,
  /// Deflate the pixel buffer before base64. NES frames are mostly flat colors
  /// and compress very well, which cuts per-frame bytes — especially helpful
  /// at high `scale` where the payload would otherwise overwhelm the terminal.
  pub zlib: bool,
}

impl Default for KittyOpts {
  fn default() -> Self {
    Self {
      scale: 1,
      zlib: false,
    }
  }
}

pub struct KittyRenderer {
  buf: Vec<u8>,
  opts: KittyOpts,
  // Double-buffered image ids that alternate every frame. Each render transmits
  // the new image under one id, then deletes the previous frame's image (with
  // its placement) under the *other* id — so there is never a moment with no
  // placement on screen. Re-using a single id, by contrast, gives the terminal
  // a window where the old image is being replaced and nothing is displayed,
  // which manifests as visible flashing once frames are large enough that the
  // multi-chunk transmit straddles a redraw (≥~scale 2).
  next_id: u32,
  prev_id: Option<u32>,
}

impl KittyRenderer {
  // Kitty requires the base64 payload be split into chunks of at most 4096 bytes.
  const CHUNK: usize = 4096;

  /// Default options (1x scale, no compression). At 1x the image is tiny on
  /// hi-DPI displays — most callers want `with_scale(3)` or higher.
  pub fn new() -> Self {
    Self::with_opts(KittyOpts::default())
  }

  /// Convenience for the common "just change the scale" case.
  pub fn with_scale(scale: u32) -> Self {
    Self::with_opts(KittyOpts {
      scale,
      ..KittyOpts::default()
    })
  }

  pub fn with_opts(opts: KittyOpts) -> Self {
    Self {
      buf: Vec::with_capacity(256 * 1024),
      opts: KittyOpts {
        scale: opts.scale.max(1),
        zlib: opts.zlib,
      },
      next_id: 1,
      prev_id: None,
    }
  }
}

impl Default for KittyRenderer {
  fn default() -> Self {
    Self::new()
  }
}

impl Renderer for KittyRenderer {
  fn render(&mut self, frame: &RenderFrame) -> Vec<u8> {
    let src: Vec<u8> = frame.pixels_ntsc().collect();
    let src_w = nes::frame::NTSC_WIDTH;
    let src_h = nes::frame::NTSC_HEIGHT;
    let scale = self.opts.scale as usize;
    let (pixels, w, h) = if scale == 1 {
      (src, src_w, src_h)
    } else {
      (
        nearest_neighbor_upscale_rgb(&src, src_w, src_h, scale),
        src_w * scale,
        src_h * scale,
      )
    };

    // Optionally compress before base64. The terminal sees `o=z` in the first
    // chunk's control segment and inflates after base64-decoding.
    let to_encode = if self.opts.zlib {
      deflate(&pixels)
    } else {
      pixels
    };
    let encoded = BASE64.encode(&to_encode);
    let payload = encoded.as_bytes();

    let id = self.next_id;
    let prev = self.prev_id;

    self.buf.clear();
    self.buf.extend_from_slice(ansi::CURSOR_HOME_BYTES);

    let chunks = payload.chunks(Self::CHUNK);
    let last_index = (payload.len().saturating_sub(1)) / Self::CHUNK;
    let compress = if self.opts.zlib { ",o=z" } else { "" };

    for (i, chunk) in chunks.enumerate() {
      let more = if i == last_index { 0 } else { 1 };
      if i == 0 {
        // Control keys are only sent on the first chunk.
        self.buf.extend_from_slice(
          format!("\x1b_Ga=T,f=24,s={w},v={h},i={id},q=2{compress},m={more};").as_bytes(),
        );
      } else {
        self
          .buf
          .extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
      }
      self.buf.extend_from_slice(chunk);
      self.buf.extend_from_slice(b"\x1b\\");
    }

    // The new placement is now on top (kitty orders same-z placements by
    // creation time). Tell the terminal to garbage-collect the previous frame's
    // image and its placement. This happens *after* the new placement is
    // visible, so the screen is never blank.
    if let Some(p) = prev {
      self
        .buf
        .extend_from_slice(format!("\x1b_Ga=d,d=I,i={p},q=2;\x1b\\").as_bytes());
    }

    self.prev_id = Some(id);
    self.next_id = if id == 1 { 2 } else { 1 };

    self.buf.clone()
  }
}

/// Tunable knobs for [`ChafaRenderer`]. These are translated to the matching
/// `chafa` CLI flags by the renderer — not a literal passthrough, just a
/// curated subset. `None` means "let chafa pick its own default."
#[derive(Debug, Clone, Default)]
pub struct ChafaOpts {
  /// chafa `--format`. Recognized: `symbols`, `sixels`, `kitty`, `iterm`.
  pub format: Option<String>,
  /// chafa `--symbols`. E.g. `octant`, `sextant`, `vhalf`, `block`, etc.
  pub symbols: Option<String>,
  /// chafa `--scale`. A number (e.g. `2`, `1.5`) or `max` (fit to view).
  pub scale: Option<String>,
}

/// Renders frames by shelling out to the `chafa` CLI per frame.
///
/// chafa is a sophisticated terminal image renderer (gamma-correct downscaling,
/// 2-color per-cell quantization, sub-cell glyph selection across half-blocks /
/// quadrants / sextants / octants / braille). It's far higher fidelity than
/// our halfblock renderer for arbitrary terminal sizes, at the cost of process
/// spawn + PNG encode per frame — so cap fps modestly when using it.
///
/// We capture chafa's stdout via a pipe (rather than letting it inherit the
/// TUI's TTY). That has two important side effects: chafa skips its terminal
/// capability probes (whose replies would otherwise land in our crossterm
/// input stream), and it falls back to `--view-size` for sizing — which we
/// supply explicitly from [`Renderer::on_resize`].
pub struct ChafaRenderer {
  opts: ChafaOpts,
  // Current terminal cell dimensions. Defaults to 80x24 until on_resize is
  // called (which TuiHost does at startup and on every Resize event).
  cols: u16,
  rows: u16,
}

impl ChafaRenderer {
  pub fn new(opts: ChafaOpts) -> Self {
    Self {
      opts,
      cols: 80,
      rows: 24,
    }
  }

  /// Confirm the `chafa` CLI is on the PATH and runnable. Call this *before*
  /// entering raw mode so a missing binary surfaces as an ordinary error
  /// message instead of a cleared alt-screen.
  pub fn probe() -> std::io::Result<()> {
    let out = std::process::Command::new("chafa")
      .arg("--version")
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .output()?;
    if out.status.success() {
      Ok(())
    } else {
      Err(std::io::Error::other("`chafa --version` returned non-zero"))
    }
  }

  /// Compose the chafa command for one frame. Split out for testability.
  fn build_args(&self) -> Vec<String> {
    let mut args: Vec<String> = vec![
      // Polite mode strips chafa's own cursor hide/show + alt-screen escapes
      // so the output composes cleanly with TuiHost's own terminal control.
      "--polite".into(),
      "on".into(),
      // Disable terminal capability probes. Despite our piped stdio, chafa
      // would otherwise open /dev/tty (the controlling terminal) and send
      // device-attribute queries, then wait up to its --probe timeout (5s by
      // default!) for replies. The replies that *do* come back land in our
      // own stdin — which crossterm is parsing for keypresses — and the
      // long wait turns shutdown into a multi-second affair when several
      // frames are in flight. We supply --view-size explicitly so chafa
      // doesn't actually need to probe.
      "--probe".into(),
      "off".into(),
      // Explicit view size — required since our piped stdout means chafa
      // can't probe the TTY for it (and we just turned probing off anyway).
      "--view-size".into(),
      format!("{}x{}", self.cols.max(1), self.rows.max(1)),
    ];
    if let Some(f) = &self.opts.format {
      args.push("--format".into());
      args.push(f.clone());
    }
    if let Some(s) = &self.opts.symbols {
      args.push("--symbols".into());
      args.push(s.clone());
    }
    if let Some(sc) = &self.opts.scale {
      args.push("--scale".into());
      args.push(sc.clone());
    }
    // Read image from stdin.
    args.push("-".into());
    args
  }
}

impl Renderer for ChafaRenderer {
  fn render(&mut self, frame: &RenderFrame) -> Vec<u8> {
    // 1. Encode the NES NTSC frame to an in-memory PNG. chafa reads PNG from
    // stdin and figures out the input dimensions from it.
    let pixels: Vec<u8> = frame.pixels_ntsc().collect();
    let mut png_buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    {
      let mut enc = png::Encoder::new(
        &mut png_buf,
        nes::frame::NTSC_WIDTH as u32,
        nes::frame::NTSC_HEIGHT as u32,
      );
      enc.set_color(png::ColorType::Rgb);
      enc.set_depth(png::BitDepth::Eight);
      let Ok(mut w) = enc.write_header() else {
        return Vec::new();
      };
      if w.write_image_data(&pixels).is_err() || w.finish().is_err() {
        return Vec::new();
      }
    }

    // 2. Spawn chafa, feed the PNG to its stdin, capture stdout.
    let mut cmd = std::process::Command::new("chafa");
    cmd
      .args(self.build_args())
      .stdin(std::process::Stdio::piped())
      .stdout(std::process::Stdio::piped())
      .stderr(std::process::Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
      return Vec::new();
    };
    if let Some(mut stdin) = child.stdin.take() {
      // Best-effort: any error here just means chafa will see truncated input
      // and produce a partial frame, which we'd rather discard than spam.
      let _ = stdin.write_all(&png_buf);
      // Stdin closes on drop, signaling EOF to chafa.
    }
    let Ok(output) = child.wait_with_output() else {
      return Vec::new();
    };

    // 3. Build the final byte stream: cursor home + chafa output with LF→CRLF
    // translation (raw-mode terminals don't map NL→CRNL).
    let mut buf = Vec::with_capacity(ansi::CURSOR_HOME_BYTES.len() + output.stdout.len() + 64);
    buf.extend_from_slice(ansi::CURSOR_HOME_BYTES);
    let mut start = 0;
    for (i, &b) in output.stdout.iter().enumerate() {
      if b == b'\n' {
        buf.extend_from_slice(&output.stdout[start..i]);
        buf.extend_from_slice(b"\r\n");
        start = i + 1;
      }
    }
    buf.extend_from_slice(&output.stdout[start..]);
    buf
  }

  fn on_resize(&mut self, cols: u16, rows: u16) {
    self.cols = cols.max(1);
    self.rows = rows.max(1);
  }
}

/// RFC 1950 zlib-deflate `src`. Used to feed kitty's `o=z` decompression. Fast
/// compression level — encode speed matters more than ratio at 60 fps, and NES
/// content is so flat that even the fast preset gives big savings.
fn deflate(src: &[u8]) -> Vec<u8> {
  let mut enc = ZlibEncoder::new(Vec::with_capacity(src.len() / 4), Compression::fast());
  enc.write_all(src).expect("zlib write to Vec cannot fail");
  enc.finish().expect("zlib finish to Vec cannot fail")
}

/// Nearest-neighbor upscale of a tightly-packed RGB888 image. Each source
/// pixel becomes a `scale`x`scale` block of identical pixels in the output.
fn nearest_neighbor_upscale_rgb(src: &[u8], w: usize, h: usize, scale: usize) -> Vec<u8> {
  debug_assert!(scale >= 1);
  let sw = w * scale;
  let mut out = Vec::with_capacity(sw * h * scale * 3);
  let mut scaled_row: Vec<u8> = Vec::with_capacity(sw * 3);
  for y in 0..h {
    scaled_row.clear();
    let row = &src[y * w * 3..(y + 1) * w * 3];
    for x in 0..w {
      let pix = &row[x * 3..x * 3 + 3];
      for _ in 0..scale {
        scaled_row.extend_from_slice(pix);
      }
    }
    for _ in 0..scale {
      out.extend_from_slice(&scaled_row);
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use nes::frame::PixelFormatRGB888;
  use nes::frame::RenderFrame;

  use super::AsciiRenderer;
  use super::ColorDepth;
  use super::HalfblockRenderer;
  use super::KittyRenderer;
  use super::Renderer;
  use super::SixelRenderer;

  fn fixture_frame() -> RenderFrame {
    let buf888 = include_bytes!("../tests/frame_888_pal.bin");
    let mut frame888 = RenderFrame::new::<PixelFormatRGB888>();
    frame888.replace_buf(buf888);
    frame888
  }

  #[test]
  fn frame_sizes() {
    let frame888 = fixture_frame();

    let sixel888 = SixelRenderer::new().render(&frame888).len();
    let color = HalfblockRenderer::new(ColorDepth::Ansi256)
      .render(&frame888)
      .len();
    let ascii = AsciiRenderer::new().render(&frame888).len();

    assert!(8_000 <= sixel888, "sixel 888 too big: {sixel888}kb"); // 0.24mb/s at 30 fps
    assert!(153_000 <= color, "color too big: {color}kb"); // 1.5mb/s at 10
    assert!(40_000 <= ascii, "ascii too big: {ascii}kb"); // 0.8mb/s at 20
  }

  #[test]
  fn chafa_builds_expected_argv() {
    use super::ChafaOpts;
    use super::ChafaRenderer;
    let mut r = ChafaRenderer::new(ChafaOpts {
      format: Some("symbols".into()),
      symbols: Some("octant".into()),
      scale: Some("2".into()),
    });
    r.on_resize(160, 40);
    let argv = r.build_args();
    let joined = argv.join(" ");
    assert!(joined.contains("--polite on"), "always polite: {joined}");
    assert!(
      joined.contains("--probe off"),
      "probing must be off so terminal-query replies don't pollute stdin: {joined}"
    );
    assert!(
      joined.contains("--view-size 160x40"),
      "view-size from on_resize: {joined}"
    );
    assert!(joined.contains("--format symbols"), "format flag: {joined}");
    assert!(
      joined.contains("--symbols octant"),
      "symbols flag: {joined}"
    );
    assert!(joined.contains("--scale 2"), "scale flag: {joined}");
    assert_eq!(argv.last().map(String::as_str), Some("-"), "reads stdin");
  }

  #[test]
  fn chafa_omits_unset_opts() {
    use super::ChafaOpts;
    use super::ChafaRenderer;
    let r = ChafaRenderer::new(ChafaOpts::default());
    let joined = r.build_args().join(" ");
    assert!(!joined.contains("--format"));
    assert!(!joined.contains("--symbols"));
    assert!(!joined.contains("--scale"));
  }

  #[test]
  fn kitty_zlib_emits_o_z_and_shrinks_payload() {
    use super::KittyOpts;
    let frame = fixture_frame();
    let plain = KittyRenderer::with_scale(3).render(&frame).len();
    let zipped = KittyRenderer::with_opts(KittyOpts {
      scale: 3,
      zlib: true,
    })
    .render(&frame)
    .len();

    let bytes = KittyRenderer::with_opts(KittyOpts {
      scale: 1,
      zlib: true,
    })
    .render(&frame);
    let s = std::str::from_utf8(&bytes).unwrap();
    assert!(s.contains(",o=z,"), "first chunk header must advertise o=z");

    // The PAL fixture is mostly flat color — easy 2x or better at scale 3, and
    // the compressed payload is fully consumed by the encoded image.
    assert!(
      zipped * 2 < plain,
      "zlib must roughly halve the payload (plain={plain}, zipped={zipped})"
    );
  }

  #[test]
  fn kitty_alternates_ids_and_deletes_previous() {
    let frame = fixture_frame();
    let mut r = KittyRenderer::new();

    let f1 = r.render(&frame);
    // First frame: new image at id=1, no previous to delete.
    let f1s = std::str::from_utf8(&f1).unwrap();
    assert!(f1s.contains(",i=1,"), "first frame should transmit at id=1");
    assert!(
      !f1s.contains("a=d,"),
      "first frame must not delete (nothing yet)"
    );

    let f2 = r.render(&frame);
    // Second frame: new image at id=2, *then* delete id=1.
    let f2s = std::str::from_utf8(&f2).unwrap();
    assert!(
      f2s.contains(",i=2,"),
      "second frame should transmit at id=2"
    );
    let place = f2s.find(",i=2,").unwrap();
    let delete = f2s
      .find("a=d,d=I,i=1")
      .expect("second frame should delete id=1");
    assert!(
      delete > place,
      "delete must come after the new placement so there's no blank window"
    );

    let f3 = r.render(&frame);
    // Third frame: id=1 again (alternation), delete id=2.
    let f3s = std::str::from_utf8(&f3).unwrap();
    assert!(f3s.contains(",i=1,"));
    assert!(f3s.contains("a=d,d=I,i=2"));
  }

  #[test]
  fn kitty_scale_grows_payload_quadratically() {
    let frame = fixture_frame();
    let small = KittyRenderer::with_scale(1).render(&frame).len();
    let big = KittyRenderer::with_scale(3).render(&frame).len();
    // 3x scale -> ~9x pixel area, so the (mostly base64) payload should grow
    // by roughly the same factor. Allow generous slack for framing overhead.
    assert!(
      big > small * 7 && big < small * 11,
      "scale 3 payload {big} not ~9x of scale 1 {small}"
    );
  }

  #[test]
  fn kitty_is_well_formed() {
    let frame888 = fixture_frame();
    let out = KittyRenderer::new().render(&frame888);

    // Cursor-home prefix, at least one graphics escape, terminated correctly.
    assert!(out.starts_with(b"\x1b[H"));
    assert!(out.windows(4).any(|w| w == b"\x1b_Ga"));
    assert!(out.ends_with(b"\x1b\\"));
    // Exactly one terminator per chunk; first chunk carries the control keys.
    let starts = out.windows(3).filter(|w| *w == b"\x1b_G").count();
    let ends = out.windows(2).filter(|w| *w == b"\x1b\\").count();
    assert_eq!(starts, ends);
  }
}
