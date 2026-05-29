//! Terminal renderers for the NES 256x240 frame buffer.
//!
//! Each [`Renderer`] turns a [`nes::frame::RenderFrame`] into a `Vec<u8>` of
//! bytes ready to be written to a terminal: ANSI escape sequences, a Sixel
//! image, or a Kitty graphics-protocol image. The output always begins by
//! homing the cursor so successive frames overdraw in place.

pub mod ansi;
mod renderers;

pub use renderers::create;
pub use renderers::AsciiRenderer;
pub use renderers::ChafaOpts;
pub use renderers::ChafaRenderer;
pub use renderers::ColorDepth;
pub use renderers::HalfblockRenderer;
pub use renderers::KittyOpts;
pub use renderers::KittyRenderer;
pub use renderers::RenderMode;
pub use renderers::Renderer;
pub use renderers::Rgb;
pub use renderers::SixelRenderer;
