use std::time::Instant;

use nes::frame::RenderFrame;
use nes::joypad::Joypad;
use nes::joypad::JoypadButton;
use nes::joypad::JoypadEvent;
use nes::nes::HostEvent;
use nes::nes::{EmulationSpeed, HostPlatform};
use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::Canvas;
use sdl2::render::Texture;
use sdl2::render::TextureCreator;
use sdl2::video::Window;
use sdl2::video::WindowContext;
use sdl2::Sdl;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

type AudioBuffer = Arc<Mutex<VecDeque<f32>>>;

struct AudioCallbackData {
  buffer: AudioBuffer,
}

impl AudioCallback for AudioCallbackData {
  type Channel = f32;

  fn callback(&mut self, out: &mut [f32]) {
    if let Ok(mut buffer) = self.buffer.lock() {
      for sample in out.iter_mut() {
        *sample = buffer.pop_front().unwrap_or(0.0);
      }
    } else {
      // If we can't lock, just output silence
      for sample in out.iter_mut() {
        *sample = 0.0;
      }
    }
  }
}

pub struct SdlHostPlatform<'a> {
  context: Sdl,
  canvas: Canvas<Window>,
  texture: Texture<'a>,
  _creator: TextureCreator<WindowContext>,
  time: Instant,
  vsync_enabled: bool,
  _audio_device: AudioDevice<AudioCallbackData>,
  audio_buffer: AudioBuffer,
}

impl SdlHostPlatform<'_> {
  pub fn new() -> Self {
    let sdl_context = sdl2::init().unwrap();
    let (canvas, texture, creator) = Self::create_canvas(&sdl_context, true);
    let (audio_device, audio_buffer) = Self::create_audio(&sdl_context);

    Self {
      _creator: creator,
      context: sdl_context,
      canvas,
      texture,
      time: Instant::now(),
      vsync_enabled: true,
      _audio_device: audio_device,
      audio_buffer,
    }
  }

  fn set_vsync(&mut self, enabled: bool) {
    let (canvas, texture, creator) = Self::create_canvas(&self.context, enabled);
    self.canvas = canvas;
    self.texture = texture;
    self._creator = creator;
    self.vsync_enabled = enabled;
  }

  fn create_canvas<'a>(
    sdl_context: &Sdl,
    vsync: bool,
  ) -> (Canvas<Window>, Texture<'a>, TextureCreator<WindowContext>) {
    let scale = 4;
    let w = nes::frame::NTSC_WIDTH as u32;
    let h = nes::frame::NTSC_HEIGHT as u32;
    let window_width = w * scale;
    let window_height = h * scale;
    let video_subsystem = sdl_context.video().unwrap();

    let window = video_subsystem
      .window("Potatis", window_width, window_height)
      .position_centered()
      .build()
      .unwrap();

    let canvas = if vsync {
      window
        .into_canvas()
        .accelerated()
        .present_vsync()
        .build()
        .unwrap()
    } else {
      window.into_canvas().accelerated().build().unwrap()
    };

    let mut creator = canvas.texture_creator();
    let texture: Texture = unsafe {
      let ptr = &mut creator as *mut TextureCreator<WindowContext>;
      (*ptr)
        .create_texture_target(PixelFormatEnum::RGB24, w, h)
        .unwrap()
    };

    (canvas, texture, creator)
  }

  fn create_audio(sdl_context: &Sdl) -> (AudioDevice<AudioCallbackData>, AudioBuffer) {
    let audio_subsystem = sdl_context.audio().unwrap();

    let desired_spec = AudioSpecDesired {
      freq: Some(44100),
      channels: Some(1),   // Mono
      samples: Some(1024), // Smaller buffer for lower latency
    };

    let audio_buffer = Arc::new(Mutex::new(VecDeque::new()));
    let callback_data = AudioCallbackData {
      buffer: audio_buffer.clone(),
    };

    let audio_device = audio_subsystem
      .open_playback(None, &desired_spec, |spec| {
        println!("Audio spec: {:?}", spec);
        callback_data
      })
      .unwrap();

    audio_device.resume();

    (audio_device, audio_buffer)
  }
}

impl HostPlatform for SdlHostPlatform<'_> {
  fn render(&mut self, frame: &RenderFrame) {
    let pixels: Vec<u8> = frame.pixels_ntsc().collect();
    self
      .texture
      .update(None, &pixels, frame.pitch_ntsc())
      .unwrap();
    self.canvas.copy(&self.texture, None, None).unwrap();
    self.canvas.present();
  }

  fn poll_events(&mut self, joypad: &mut Joypad) -> HostEvent {
    for event in self.context.event_pump().unwrap().poll_iter() {
      if let Some(joypad_ev) = map_joypad(&event) {
        joypad.on_event(joypad_ev);
        continue;
      }

      match event {
        Event::Quit { .. }
        | Event::KeyDown {
          keycode: Some(Keycode::Q),
          ..
        }
        | Event::KeyDown {
          keycode: Some(Keycode::Escape),
          ..
        } => return HostEvent::Shutdown,
        Event::KeyDown {
          keycode: Some(Keycode::R),
          ..
        } => return HostEvent::Reset,
        Event::KeyDown {
          keycode: Some(Keycode::Num1),
          ..
        } => {
          println!(
            "Speed: Normal (authentic NES timing, VSync {})",
            self.vsync_enabled
          );
          return HostEvent::ChangeSpeed(EmulationSpeed::Normal);
        }
        Event::KeyDown {
          keycode: Some(Keycode::Num2),
          ..
        } => {
          println!("Speed: 2x (VSync {})", self.vsync_enabled);
          return HostEvent::ChangeSpeed(EmulationSpeed::Fast(2));
        }
        Event::KeyDown {
          keycode: Some(Keycode::Num3),
          ..
        } => {
          println!("Speed: 3x (VSync {})", self.vsync_enabled);
          return HostEvent::ChangeSpeed(EmulationSpeed::Fast(3));
        }
        Event::KeyDown {
          keycode: Some(Keycode::Num0),
          ..
        } => {
          println!(
            "Speed: Uncapped (max performance, VSync {})",
            self.vsync_enabled
          );
          return HostEvent::ChangeSpeed(EmulationSpeed::Uncapped);
        }
        Event::KeyDown {
          keycode: Some(Keycode::V),
          ..
        } => {
          self.set_vsync(!self.vsync_enabled);
          println!("VSync: {}", self.vsync_enabled);
        }
        _ => (),
      }
    }
    HostEvent::Nothing
  }

  fn elapsed_millis(&self) -> usize {
    self.time.elapsed().as_millis() as usize
  }

  fn delay(&self, d: std::time::Duration) {
    std::thread::sleep(d)
  }

  fn audio_sample(&mut self, sample: f32) {
    if let Ok(mut buffer) = self.audio_buffer.lock() {
      // Keep buffer size reasonable to prevent latency but don't drop samples
      if buffer.len() > 8820 {
        // ~0.2 seconds at 44.1kHz - remove oldest sample to make room
        buffer.pop_front();
      }
      buffer.push_back(sample);
    }
  }
}

fn map_joypad(sdlev: &Event) -> Option<JoypadEvent> {
  match sdlev {
    Event::KeyDown {
      keycode: Some(keycode),
      ..
    } => map_button(keycode).map(JoypadEvent::Press),
    Event::KeyUp {
      keycode: Some(keycode),
      ..
    } => map_button(keycode).map(JoypadEvent::Release),
    _ => None,
  }
}

fn map_button(keycode: &Keycode) -> Option<JoypadButton> {
  match keycode {
    Keycode::W => Some(JoypadButton::UP),
    Keycode::A => Some(JoypadButton::LEFT),
    Keycode::S => Some(JoypadButton::DOWN),
    Keycode::D => Some(JoypadButton::RIGHT),
    Keycode::K => Some(JoypadButton::B),
    Keycode::L => Some(JoypadButton::A),
    Keycode::Return => Some(JoypadButton::START),
    Keycode::Space => Some(JoypadButton::SELECT),
    _ => None,
  }
}
