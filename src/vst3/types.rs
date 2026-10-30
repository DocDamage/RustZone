use std::ffi::c_void;
use std::os::raw::c_char;

pub type TUID = [u8; 16];

// Standard Steinberg VST3 Result Codes
pub const K_RESULT_OK: i32 = 0;
pub const K_RESULT_FALSE: i32 = 1;
pub const K_INVALID_ARGUMENT: i32 = -2147024809; // 0x80070057
pub const K_NOT_IMPLEMENTED: i32 = -2147467263; // 0x80004001
pub const K_NO_INTERFACE: i32 = -2147467262; // 0x80004002

// Standard VST3 interface IIDs
pub const IID_FUNKNOWN: TUID = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
];

pub const IID_IPLUGIN_FACTORY: TUID = [
    0x7A, 0x4D, 0x81, 0x70, 0x52, 0xAE, 0x4A, 0xB5, 0x9E, 0x73, 0x26, 0xB2, 0x18, 0xE7, 0x95, 0xA3,
];

pub const IID_IPLUGIN_BASE: TUID = [
    0xAE, 0x7A, 0x2B, 0xF5, 0x72, 0xDE, 0x6D, 0x41, 0x8A, 0xF1, 0x8A, 0xCE, 0x9D, 0xD7, 0xBD, 0x5E,
];

pub const IID_ICOMPONENT: TUID = [
    0x31, 0xFF, 0x31, 0xE8, 0xD5, 0xF2, 0x01, 0x43, 0x92, 0x8E, 0xBB, 0xEE, 0x25, 0x69, 0x78, 0x02,
];

pub const IID_IAUDIO_PROCESSOR: TUID = [
    0x99, 0x3F, 0x04, 0x42, 0xDA, 0xB7, 0x3C, 0x45, 0xA5, 0x69, 0xE7, 0x9D, 0x9A, 0xAE, 0xC3, 0x3D,
];

pub const IID_IEDIT_CONTROLLER: TUID = [
    0xE3, 0xBB, 0xD7, 0xDC, 0x42, 0x77, 0x8D, 0x44, 0xA8, 0x74, 0xAA, 0xCC, 0x97, 0x9C, 0x75, 0x9E,
];

pub const IID_ICONNECTION_POINT: TUID = [
    0x6F, 0x15, 0xA4, 0x70, 0x6E, 0x6E, 0x26, 0x40, 0x98, 0x91, 0x48, 0xBF, 0xAA, 0x60, 0xD8, 0xD1,
];

pub const IID_IMESSAGE: TUID = [
    0x3B, 0x03, 0x6F, 0x93, 0xC0, 0xC6, 0xDB, 0x47, 0xBB, 0x08, 0x82, 0xF8, 0x13, 0xC1, 0xE6, 0x13,
];

pub const IID_IATTRIBUTE_LIST: TUID = [
    0x22, 0x02, 0xCE, 0x66, 0x12, 0xEF, 0x67, 0x48, 0xA2, 0x9B, 0x88, 0xE1, 0xF6, 0x56, 0x5E, 0x61,
];

pub const IID_IHOST_APPLICATION: TUID = [
    0xCC, 0x95, 0xE5, 0x58, 0x2D, 0xDB, 0x69, 0x49, 0x8B, 0x6A, 0xAF, 0x8C, 0x36, 0xA6, 0x64, 0xE5,
];

pub const IID_IPLUG_VIEW: TUID = [
    0x5B, 0x36, 0x3B, 0xB9, 0x7B, 0x7C, 0x48, 0x9D, 0xBC, 0x3E, 0xA7, 0xB4, 0x7C, 0x87, 0xC6, 0xF7,
];

pub const IID_IPLUG_FRAME: TUID = [
    0x3F, 0xC4, 0x05, 0x64, 0xF8, 0x86, 0x48, 0x82, 0xBE, 0xE9, 0xB7, 0x84, 0x18, 0x98, 0xD0, 0x0F,
];

pub const IID_ICOMPONENT_HANDLER: TUID = [
    0x93, 0xA0, 0xBE, 0x4E, 0x1A, 0x01, 0x4F, 0x3E, 0x98, 0x43, 0x5D, 0x85, 0x8E, 0x4A, 0x3D, 0xC7,
];

pub const IID_IBSTREAM: TUID = [
    0xC3, 0x53, 0xD8, 0x95, 0x50, 0x49, 0x42, 0xBB, 0xAC, 0x7A, 0x31, 0x6A, 0xD0, 0x0E, 0x5D, 0x4B,
];

// Factory structures
#[repr(C)]
pub struct PFactoryInfo {
    pub vendor: [c_char; 64],
    pub url: [c_char; 256],
    pub email: [c_char; 128],
    pub flags: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PClassInfo {
    pub cid: TUID,
    pub cardinality: i32,
    pub category: [c_char; 32],
    pub name: [c_char; 64],
}

// Processing structures
pub const K_REALTIME: i32 = 0;
pub const K_PREFETCH: i32 = 1;
pub const K_OFFLINE: i32 = 2;

pub const K_SAMPLE_32: i32 = 0;
pub const K_SAMPLE_64: i32 = 1;

pub const K_AUDIO: i32 = 0;
pub const K_EVENT: i32 = 1;

pub const K_INPUT: i32 = 0;
pub const K_OUTPUT: i32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ProcessSetup {
    pub process_mode: i32,
    pub symbolic_sample_size: i32,
    pub max_samples_per_block: i32,
    pub sample_rate: f64,
}

#[repr(C)]
pub struct AudioBusBuffers {
    pub num_channels: i32,
    pub silence_flags: u64,
    pub channel_buffers32: *mut *mut f32,
}

#[repr(C)]
pub struct ProcessContext {
    pub state: u32,
    pub sample_rate: f64,
    pub project_time_samples: i64,
    pub system_time: i64,
    pub continuous_time_samples: i64,
    pub project_time_music: f64,
    pub bar_position_music: f64,
    pub cycle_start_music: f64,
    pub cycle_end_music: f64,
    pub tempo: f64,
    pub time_sig_numerator: i32,
    pub time_sig_denominator: i32,
    pub chord_key: u8,
    pub chord_mask: u8,
    pub reserved: [u8; 6],
    pub frame_rate_frame_rate: u32,
    pub frame_rate_flags: u32,
    pub samples_to_next_clock: i32,
}

#[repr(C)]
pub struct ProcessData {
    pub process_mode: i32,
    pub symbolic_sample_size: i32,
    pub num_samples: i32,
    pub num_inputs: i32,
    pub num_outputs: i32,
    pub inputs: *mut AudioBusBuffers,
    pub outputs: *mut AudioBusBuffers,
    pub input_parameter_changes: *mut c_void,
    pub output_parameter_changes: *mut c_void,
    pub input_events: *mut c_void,
    pub output_events: *mut c_void,
    pub process_context: *mut ProcessContext,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ViewRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl ViewRect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

// COM VTables
#[repr(C)]
pub struct FUnknownVtbl {
    pub query_interface: unsafe extern "system" fn(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

#[repr(C)]
pub struct IPluginFactoryVtbl {
    pub base: FUnknownVtbl,
    pub get_factory_info: unsafe extern "system" fn(this: *mut c_void, info: *mut PFactoryInfo) -> i32,
    pub count_classes: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub get_class_info: unsafe extern "system" fn(this: *mut c_void, index: i32, info: *mut PClassInfo) -> i32,
    pub create_instance: unsafe extern "system" fn(this: *mut c_void, cid: *const TUID, iid: *const TUID, obj: *mut *mut c_void) -> i32,
}

#[repr(C)]
pub struct IComponentVtbl {
    pub base: FUnknownVtbl,
    pub initialize: unsafe extern "system" fn(this: *mut c_void, context: *mut c_void) -> i32,
    pub terminate: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub get_controller_class_id: unsafe extern "system" fn(this: *mut c_void, class_id: *mut TUID) -> i32,
    pub set_io_mode: unsafe extern "system" fn(this: *mut c_void, mode: i32) -> i32,
    pub get_bus_count: unsafe extern "system" fn(this: *mut c_void, type_: i32, dir: i32) -> i32,
    pub get_bus_info: unsafe extern "system" fn(this: *mut c_void, type_: i32, dir: i32, index: i32, info: *mut c_void) -> i32,
    pub get_routing_info: unsafe extern "system" fn(this: *mut c_void, in_info: *mut c_void, out_info: *mut c_void) -> i32,
    pub activate_bus: unsafe extern "system" fn(this: *mut c_void, type_: i32, dir: i32, index: i32, state: u8) -> i32,
    pub set_active: unsafe extern "system" fn(this: *mut c_void, state: u8) -> i32,
    pub set_state: unsafe extern "system" fn(this: *mut c_void, state: *mut c_void) -> i32,
    pub get_state: unsafe extern "system" fn(this: *mut c_void, state: *mut c_void) -> i32,
}

#[repr(C)]
pub struct IAudioProcessorVtbl {
    pub base: FUnknownVtbl,
    pub set_bus_arrangements: unsafe extern "system" fn(this: *mut c_void, inputs: *const u64, num_ins: i32, outputs: *const u64, num_outs: i32) -> i32,
    pub get_bus_arrangement: unsafe extern "system" fn(this: *mut c_void, dir: i32, index: i32, arr: *mut u64) -> i32,
    pub can_process_sample_size: unsafe extern "system" fn(this: *mut c_void, symbolic_sample_size: i32) -> i32,
    pub get_latency_samples: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub setup_processing: unsafe extern "system" fn(this: *mut c_void, setup: *const ProcessSetup) -> i32,
    pub set_processing: unsafe extern "system" fn(this: *mut c_void, state: u8) -> i32,
    pub process: unsafe extern "system" fn(this: *mut c_void, data: *mut ProcessData) -> i32,
    pub get_tail_samples: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

#[repr(C)]
pub struct IEditControllerVtbl {
    pub base: FUnknownVtbl,
    pub initialize: unsafe extern "system" fn(this: *mut c_void, context: *mut c_void) -> i32,
    pub terminate: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub set_component_state: unsafe extern "system" fn(this: *mut c_void, state: *mut c_void) -> i32,
    pub set_state: unsafe extern "system" fn(this: *mut c_void, state: *mut c_void) -> i32,
    pub get_state: unsafe extern "system" fn(this: *mut c_void, state: *mut c_void) -> i32,
    pub get_parameter_count: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub get_parameter_info: unsafe extern "system" fn(this: *mut c_void, param_index: i32, info: *mut c_void) -> i32,
    pub get_param_string_by_value: unsafe extern "system" fn(this: *mut c_void, id: u32, value_norm: f64, string: *mut u16) -> i32,
    pub get_param_value_by_string: unsafe extern "system" fn(this: *mut c_void, id: u32, string: *const u16, value_norm: *mut f64) -> i32,
    pub normalized_param_to_plain: unsafe extern "system" fn(this: *mut c_void, id: u32, value_norm: f64) -> f64,
    pub plain_param_to_normalized: unsafe extern "system" fn(this: *mut c_void, id: u32, plain_value: f64) -> f64,
    pub get_param_normalized: unsafe extern "system" fn(this: *mut c_void, id: u32) -> f64,
    pub set_param_normalized: unsafe extern "system" fn(this: *mut c_void, id: u32, value: f64) -> i32,
    pub set_component_handler: unsafe extern "system" fn(this: *mut c_void, handler: *mut c_void) -> i32,
    pub create_view: unsafe extern "system" fn(this: *mut c_void, name: *const c_char) -> *mut c_void,
}

#[repr(C)]
pub struct IPlugViewVtbl {
    pub base: FUnknownVtbl,
    pub is_platform_type_supported: unsafe extern "system" fn(this: *mut c_void, type_: *const c_char) -> i32,
    pub attached: unsafe extern "system" fn(this: *mut c_void, parent: *mut c_void, type_: *const c_char) -> i32,
    pub removed: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub on_wheel: unsafe extern "system" fn(this: *mut c_void, distance: f32) -> i32,
    pub on_key_down: unsafe extern "system" fn(this: *mut c_void, key: u16, code: i16, modifiers: i16) -> i32,
    pub on_key_up: unsafe extern "system" fn(this: *mut c_void, key: u16, code: i16, modifiers: i16) -> i32,
    pub get_size: unsafe extern "system" fn(this: *mut c_void, size: *mut ViewRect) -> i32,
    pub on_size: unsafe extern "system" fn(this: *mut c_void, new_size: *mut ViewRect) -> i32,
    pub on_focus: unsafe extern "system" fn(this: *mut c_void, state: u8) -> i32,
    pub set_frame: unsafe extern "system" fn(this: *mut c_void, frame: *mut c_void) -> i32,
    pub can_resize: unsafe extern "system" fn(this: *mut c_void) -> i32,
    pub check_size_constraint: unsafe extern "system" fn(this: *mut c_void, rect: *mut ViewRect) -> i32,
}

#[repr(C)]
pub struct IConnectionPointVtbl {
    pub base: FUnknownVtbl,
    pub connect: unsafe extern "system" fn(this: *mut c_void, other: *mut c_void) -> i32,
    pub disconnect: unsafe extern "system" fn(this: *mut c_void, other: *mut c_void) -> i32,
    pub notify: unsafe extern "system" fn(this: *mut c_void, message: *mut c_void) -> i32,
}

/// Standard VST3 `Vst::IAttributeList` (ivstattributes.h), in vtable order
#[repr(C)]
pub struct IAttributeListVtbl {
    pub base: FUnknownVtbl,
    pub set_int: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, value: i64) -> i32,
    pub get_int: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, value: *mut i64) -> i32,
    pub set_float: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, value: f64) -> i32,
    pub get_float: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, value: *mut f64) -> i32,
    pub set_string: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, string: *const u16) -> i32,
    pub get_string: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, string: *mut u16, size_in_bytes: u32) -> i32,
    pub set_binary: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, data: *const c_void, size_in_bytes: u32) -> i32,
    pub get_binary: unsafe extern "system" fn(this: *mut c_void, id: *const c_char, data: *mut *const c_void, size_in_bytes: *mut u32) -> i32,
}

#[repr(C)]
pub struct IMessageVtbl {
    pub base: FUnknownVtbl,
    pub get_message_id: unsafe extern "system" fn(this: *mut c_void) -> *const c_char,
    pub set_message_id: unsafe extern "system" fn(this: *mut c_void, id: *const c_char) -> i32,
    pub get_attributes: unsafe extern "system" fn(this: *mut c_void) -> *mut c_void,
}

#[repr(C)]
pub struct IHostApplicationVtbl {
    pub base: FUnknownVtbl,
    pub get_name: unsafe extern "system" fn(this: *mut c_void, name: *mut u16) -> i32,
    pub create_instance: unsafe extern "system" fn(this: *mut c_void, cid: *const TUID, iid: *const TUID, obj: *mut *mut c_void) -> i32,
}

#[repr(C)]
pub struct IPlugFrameVtbl {
    pub base: FUnknownVtbl,
    pub resize_view: unsafe extern "system" fn(this: *mut c_void, view: *mut c_void, new_size: *mut ViewRect) -> i32,
}

#[repr(C)]
pub struct IComponentHandlerVtbl {
    pub base: FUnknownVtbl,
    pub begin_edit: unsafe extern "system" fn(this: *mut c_void, id: u32) -> i32,
    pub perform_edit: unsafe extern "system" fn(this: *mut c_void, id: u32, value_norm: f64) -> i32,
    pub end_edit: unsafe extern "system" fn(this: *mut c_void, id: u32) -> i32,
    pub restart_component: unsafe extern "system" fn(this: *mut c_void, flags: i32) -> i32,
}

pub const K_SEEK_SET: i32 = 0;
pub const K_SEEK_CUR: i32 = 1;
pub const K_SEEK_END: i32 = 2;

#[repr(C)]
pub struct IBStreamVtbl {
    pub base: FUnknownVtbl,
    pub read: unsafe extern "system" fn(this: *mut c_void, buffer: *mut c_void, num_bytes: i32, num_bytes_read: *mut i32) -> i32,
    pub write: unsafe extern "system" fn(this: *mut c_void, buffer: *const c_void, num_bytes: i32, num_bytes_written: *mut i32) -> i32,
    pub seek: unsafe extern "system" fn(this: *mut c_void, pos: i64, mode: i32, result: *mut i64) -> i32,
    pub tell: unsafe extern "system" fn(this: *mut c_void, result: *mut i64) -> i32,
}
