use std::{collections::HashMap, path::PathBuf};
use lazy_static::lazy_static;
use libretro_rs::{ffi::{RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME, RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO}, prelude::*};
use holani::{cartridge::lnx_header::LNXRotation, mikey::video::{LYNX_SCREEN_HEIGHT, LYNX_SCREEN_WIDTH}, suzy::registers::{Joystick, Switches}, lynx::Lynx};
use ::log::warn;

const CRYSTAL_FREQUENCY: u32 = 16_000_000;
const SAMPLE_RATE: f64 = 22_050.;
const DEFAULT_FPS: f64 = 75.;
const TICKS_PER_AUDIO_SAMPLE: u64 = CRYSTAL_FREQUENCY as u64 / SAMPLE_RATE as u64;
const FRAME_BUFFER_LENGTH: usize = (LYNX_SCREEN_HEIGHT * LYNX_SCREEN_WIDTH) as usize;
const BUFFER_WIDTH: u16 = LYNX_SCREEN_WIDTH as u16;

struct LynxCore {
    lynx: Lynx,
    last_refresh_rate: f64,
    audio_ticks: u64,
    rendering_mode: SoftwareRenderEnabled,
    pixel_format: ActiveFormat<XRGB8888>,
    frame_buffer: ArrayFrameBuffer<XRGB8888, FRAME_BUFFER_LENGTH, BUFFER_WIDTH>,    
}

impl<'a> retro::Core<'a> for LynxCore {
    type Init = ();

    fn get_system_info() -> SystemInfo {
        SystemInfo::new( //TODO
            c_utf8!("holani"), 
            c_utf8!(env!("CARGO_PKG_VERSION")), 
            Extensions::new(c_utf8!("lnx|o")) 
        )
    }
    
    fn init(env: &mut impl env::Init) -> Self::Init {
        unsafe { let _ = env.set(RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME, &false); };
    }
    
    fn get_system_av_info(&self, _env: &mut impl env::GetAvInfo) -> SystemAVInfo {
        self.system_av_info()
    }

    fn run(&mut self, env: &mut impl env::Run, callbacks: &mut impl Callbacks) -> InputsPolled {
        let poll_inputs = self.buttons(callbacks); 
        
        while !self.lynx.redraw_requested() {
            self.lynx.tick();
            self.audio_ticks += 1;
            if self.audio_ticks == TICKS_PER_AUDIO_SAMPLE {
                let sample = self.lynx.audio_sample();
                callbacks.upload_audio_sample(sample.0, sample.1);
                self.audio_ticks = 0;                
            }
        }      

        self.blit_screen(callbacks);
        
        let rf = self.lynx.display_refresh_rate();
        if rf != self.last_refresh_rate {
            self.set_refresh_rate(rf);
            let avi = self.system_av_info();
            unsafe { 
                let _ = env.set(RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO, &avi);
            }            
        }

        poll_inputs
    }
    
    fn reset(&mut self, _env: &mut impl env::Reset) {
        self.lynx.reset();
    }
    
    fn unload_game(self, _env: &mut impl env::UnloadGame) -> Self::Init {
    }  

    fn load_game<E: env::LoadGame>(game: &GameInfo, args: LoadGameExtraArgs<'a, '_, E, Self::Init>) -> Result<Self, CoreError> {
        let LoadGameExtraArgs { env, pixel_format, rendering_mode, .. } = args;
        let pixel_format = env.set_pixel_format_xrgb8888(pixel_format)?;
        let data: Vec<u8> = game.as_data().ok_or(CoreError::new())?.data().to_vec();

        let mut lynx = Lynx::new();

        match env.get_system_directory() {
            Ok(d) => {
                let mut path = PathBuf::from(d.into_str().unwrap()); 
                if path.is_dir() {
                    path.push("lynxboot.img");
                    if path.exists() {
                        lynx.load_rom_from_slice(&std::fs::read(path).unwrap()).unwrap();
                    }
                } else {
                    warn!("'{:?}' is not a valid directory.", path);
                }
            },
            Err(_) => warn!("Couldn't get libretro system directory"),
        }

        if lynx.load_cart_from_slice(&data).is_err() {
            return Err(CoreError::new());
        }

        let rotation = match lynx.rotation() {
            LNXRotation::_270 => ScreenRotation::TwoSeventyDegrees,
            LNXRotation::_90 => ScreenRotation::NinetyDegrees,
            _ => ScreenRotation::ZeroDegrees
        };        
        env.set_rotation(rotation).unwrap();

        Ok(Self {
            lynx,
            last_refresh_rate: DEFAULT_FPS,
            audio_ticks: 0,
            rendering_mode,
            pixel_format,
            frame_buffer: ArrayFrameBuffer::new([XRGB8888::new_with_raw_value(0); FRAME_BUFFER_LENGTH])
        })
      }
}

impl<'a> retro::SaveStateCore<'a> for LynxCore {
    fn serialize_size(&self, _env: &mut impl env::SerializeSize) -> core::num::NonZeroUsize {
        let size = self.lynx.serialize_size();
        unsafe { core::num::NonZeroUsize::new_unchecked(size) }
    }

    fn serialize(&self, _env: &mut impl env::Serialize, data: &mut [u8]) -> Result<(), CoreError> {
        match holani::serialize(&self.lynx, data) {
            Err(_) => Err(CoreError::new()),
            Ok(_) => Ok(())
        }
    }

    fn unserialize(&mut self, _env: &mut impl env::Unserialize, data: &[u8]) -> Result<(), CoreError> {
        match holani::deserialize(data, &self.lynx) {
            Err(_) => Err(CoreError::new()),
            Ok(lynx) => {
                self.lynx = lynx;
                Ok(())
            }
        }        
    }
}

impl<'a> retro::GetMemoryRegionCore<'a> for LynxCore {
    fn get_memory_size(&self, _env: &mut impl env::GetMemorySize, id: MemoryType) -> usize {
        let mem_type = StandardMemoryType::try_from(id).unwrap_or(StandardMemoryType::RTC);
        match mem_type {
            StandardMemoryType::RTC => 0,
            StandardMemoryType::SaveRam => 0,
            StandardMemoryType::SystemRam => self.lynx.ram_size(),
            StandardMemoryType::VideoRam => 0,
        }
    }

    fn get_memory_data(&self, _env: &mut impl env::GetMemoryData, id: MemoryType) -> Option<&mut [u8]> {
        let mem_type = StandardMemoryType::try_from(id).unwrap_or(StandardMemoryType::RTC);
        match mem_type {
            StandardMemoryType::RTC => None,
            StandardMemoryType::SaveRam => None,
            StandardMemoryType::SystemRam => Some(unsafe { self.lynx.ram_data().as_mut_slice() }),
            StandardMemoryType::VideoRam => None,
        }
    }
}

impl LynxCore {
    fn buttons(&mut self, callbacks: &mut impl Callbacks) -> InputsPolled {
        let inputs_polled = callbacks.poll_inputs();

        let mut j = self.lynx.joystick();
        let mut s = self.lynx.switches();
        
        let jo = j;
        let so = s;

        let device = DevicePort::from(0);

        let buttons_map = &NO_ROTATION;

        for btn in Joystick::iter(&Joystick::all()) {
            j.set(btn, callbacks.is_joypad_button_pressed(device, *buttons_map.get(&btn).unwrap()));
        }
        s.set(Switches::pause, callbacks.is_joypad_button_pressed(device, JoypadButton::Start));
        if j != jo {
            self.lynx.set_joystick_u8(j.bits());
        }
        if s != so {
            self.lynx.set_switches_u8(s.bits());
        }

        inputs_polled
    }

    fn blit_screen(&mut self, callbacks: &mut impl Callbacks) {
        let screen = self.lynx.screen_rgba();
        for (src, dst) in screen.chunks_exact(4).zip(self.frame_buffer.iter_mut()) {
            *dst = XRGB8888::DEFAULT.with_r(src[0]).with_g(src[1]).with_b(src[2]);
        }
        callbacks.upload_video_frame(&self.rendering_mode, &self.pixel_format, &self.frame_buffer);
    }
  
    fn system_av_info(&self) -> SystemAVInfo {
        let (w, h) = self.lynx.screen_size();
        SystemAVInfo::new(
            GameGeometry::fixed(w as u16, h as u16), 
            SystemTiming::new(self.last_refresh_rate, SAMPLE_RATE)
        )
    }

    fn set_refresh_rate(&mut self, rate: f64) {
        self.last_refresh_rate = rate;
    }
}

lazy_static! {
    static ref NO_ROTATION: HashMap<Joystick, JoypadButton> = HashMap::from([
        (Joystick::up, JoypadButton::Up),
        (Joystick::down, JoypadButton::Down),
        (Joystick::left, JoypadButton::Left),
        (Joystick::right, JoypadButton::Right),
        (Joystick::option_1, JoypadButton::L1),
        (Joystick::option_2, JoypadButton::R1),
        (Joystick::inside, JoypadButton::A),
        (Joystick::outside, JoypadButton::B),
    ]);
}

libretro_core!(crate::LynxCore);
