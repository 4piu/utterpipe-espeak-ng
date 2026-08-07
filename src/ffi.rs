#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_short, c_uint, c_void};

#[repr(C)]
pub struct Event {
    pub kind: c_int,
    pub unique_identifier: c_uint,
    pub text_position: c_int,
    pub length: c_int,
    pub audio_position: c_int,
    pub sample: c_int,
    pub user_data: *mut c_void,
    pub id: EventId,
}

#[repr(C)]
pub union EventId {
    pub number: c_int,
    pub name: *const c_char,
    pub string: [c_char; 8],
}

#[repr(C)]
pub struct Voice {
    pub name: *const c_char,
    pub languages: *const c_char,
    pub identifier: *const c_char,
    pub gender: u8,
    pub age: u8,
    pub variant: u8,
    pub reserved: u8,
    pub score: c_int,
    pub spare: *mut c_void,
}

pub type SynthCallback = unsafe extern "C" fn(*mut c_short, c_int, *mut Event) -> c_int;

pub const AUDIO_OUTPUT_SYNCHRONOUS: c_int = 2;
pub const POSITION_CHARACTER: c_int = 1;
pub const CHARS_UTF8: c_uint = 1;
pub const END_PAUSE: c_uint = 0x1000;
pub const PARAMETER_RATE: c_int = 1;
pub const PARAMETER_VOLUME: c_int = 2;
pub const PARAMETER_PITCH: c_int = 3;
pub const OK: c_int = 0;

unsafe extern "C" {
    pub fn espeak_Initialize(
        output: c_int,
        buffer_length_ms: c_int,
        path: *const c_char,
        options: c_int,
    ) -> c_int;
    pub fn espeak_SetSynthCallback(callback: Option<SynthCallback>);
    pub fn espeak_Synth(
        text: *const c_void,
        size: usize,
        position: c_uint,
        position_type: c_int,
        end_position: c_uint,
        flags: c_uint,
        unique_identifier: *mut c_uint,
        user_data: *mut c_void,
    ) -> c_int;
    pub fn espeak_SetParameter(parameter: c_int, value: c_int, relative: c_int) -> c_int;
    pub fn espeak_SetVoiceByName(name: *const c_char) -> c_int;
    pub fn espeak_ListVoices(specification: *mut Voice) -> *const *const Voice;
    pub fn espeak_Info(data_path: *mut *const c_char) -> *const c_char;
    pub fn espeak_Terminate() -> c_int;
}
