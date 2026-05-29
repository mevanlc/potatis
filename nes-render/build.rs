//! Build-time guard: when the `chafa-lib` feature is on, require libchafa >=
//! 1.16 via pkg-config.
//!
//! `chafa-sys`'s own `build.rs` already runs first and will fail the build
//! with a pkg-config error if libchafa isn't installed at all. What it
//! *doesn't* check is the version — and `nes-render` calls into APIs
//! (notably `CHAFA_SYMBOL_TAG_OCTANT`) that only landed in chafa 1.16. On an
//! older libchafa the chafa-sys bindings would simply lack those variants,
//! and we'd surface that to the user as a baffling Rust compile error a few
//! seconds later. This script turns it into a clear pkg-config message.

fn main() {
  #[cfg(feature = "chafa-lib")]
  {
    if let Err(e) = pkg_config::Config::new()
      .atleast_version("1.16")
      .probe("chafa")
    {
      eprintln!();
      eprintln!("error: nes-render's `chafa-lib` feature requires libchafa >= 1.16.");
      eprintln!("       (Octant glyphs landed in 1.16, and nes-render uses them.)");
      eprintln!();
      eprintln!("Install:");
      eprintln!("  macOS:         brew install chafa");
      eprintln!("  Debian/Ubuntu: apt install libchafa-dev");
      eprintln!();
      eprintln!("Or disable the feature:");
      eprintln!("  cargo build --no-default-features ...");
      eprintln!();
      eprintln!("pkg-config error: {e}");
      std::process::exit(1);
    }
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
  }
}
