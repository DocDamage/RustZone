use std::ffi::{c_void, CStr, CString};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use parking_lot::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, HMODULE, HWND};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW, SetDllDirectoryW};

#[link(name = "kernel32")]
extern "system" {
    fn FreeLibrary(hLibModule: HMODULE) -> BOOL;
}

use super::host::*;
use super::types::*;

/// A hosted VST3 plugin, shared between the UI thread and the audio thread.
///
/// Follows the VST3 threading model: `process_stereo` runs on the audio thread while the editor,
/// wheel and state calls run on the UI thread. `setup_audio` must only be called while nothing
/// is processing (the engine detaches the plugin from the audio thread first). Mutable host-side
/// fields are atomics so every method takes `&self`.
pub struct Vst3Plugin {
    hmodule: HMODULE,
    factory: *mut c_void,
    component: *mut c_void,
    processor: *mut c_void,
    controller: *mut c_void,
    view: AtomicPtr<c_void>,
    host_app: *mut HostAppCom,
    comp_handler: *mut HostComponentHandlerCom,
    plug_frame: AtomicPtr<HostPlugFrameCom>,
    pub name: String,
    sample_rate_bits: AtomicU64,
    max_block_size: AtomicUsize,
    latency: AtomicU32,
    view_size: Mutex<ViewRect>,
    sample_pos: AtomicI64,
    is_active: AtomicBool,
}

unsafe impl Send for Vst3Plugin {}
// The COM pointers are only used per the VST3 threading rules described above
unsafe impl Sync for Vst3Plugin {}

impl Vst3Plugin {
    pub fn load(vst3_path: &Path) -> Result<Self, String> {
        let abs_path = std::fs::canonicalize(vst3_path)
            .map_err(|e| format!("Failed to canonicalize {:?}: {}", vst3_path, e))?;
        let clean_abs = abs_path.to_string_lossy().to_string();
        let clean_path = clean_abs.strip_prefix(r"\\?\").unwrap_or(&clean_abs).to_string();

        if let Some(parent) = Path::new(&clean_path).parent() {
            let wide_dir: Vec<u16> = parent.as_os_str().encode_wide().chain(Some(0)).collect();
            unsafe {
                let _ = SetDllDirectoryW(PCWSTR(wide_dir.as_ptr()));
            }
        }

        let wide_path: Vec<u16> = std::ffi::OsStr::new(&clean_path).encode_wide().chain(Some(0)).collect();

        let hmodule = unsafe {
            LoadLibraryW(PCWSTR(wide_path.as_ptr()))
                .map_err(|e| format!("Failed to LoadLibraryW {}: {:?}", clean_path, e))?
        };

        let gpf_name = CString::new("GetPluginFactory").unwrap();
        let gpf_addr = unsafe { GetProcAddress(hmodule, windows::core::PCSTR(gpf_name.as_ptr() as *const u8)) };
        let gpf_fn: unsafe extern "system" fn() -> *mut c_void = match gpf_addr {
            Some(func) => unsafe { std::mem::transmute(func) },
            None => {
                unsafe { let _ = FreeLibrary(hmodule); }
                return Err("GetPluginFactory export not found in VST3".to_string());
            }
        };

        let factory = unsafe { gpf_fn() };
        if factory.is_null() {
            unsafe { let _ = FreeLibrary(hmodule); }
            return Err("GetPluginFactory returned null".to_string());
        }

        let factory_vtbl = unsafe { &*(*(factory as *mut *const IPluginFactoryVtbl)) };

        // Enumerate classes in factory to find Component and Controller CIDs
        let class_count = unsafe { (factory_vtbl.count_classes)(factory) };
        if class_count <= 0 {
            unsafe { let _ = FreeLibrary(hmodule); }
            return Err("Plugin factory contains 0 classes".to_string());
        }

        let mut comp_cid: Option<TUID> = None;
        let mut ctrl_cid: Option<TUID> = None;
        let mut plugin_name = String::new();

        for i in 0..class_count {
            let mut info: PClassInfo = unsafe { std::mem::zeroed() };
            let r = unsafe { (factory_vtbl.get_class_info)(factory, i, &mut info) };
            if r == K_RESULT_OK {
                let cat = unsafe { CStr::from_ptr(info.category.as_ptr()) }.to_string_lossy();
                let name = unsafe { CStr::from_ptr(info.name.as_ptr()) }.to_string_lossy();

                if cat.contains("Audio Module Class") || cat.contains("Component") || i == 0 {
                    if comp_cid.is_none() {
                        comp_cid = Some(info.cid);
                        plugin_name = name.to_string();
                    }
                }
                if cat.contains("Component Controller Class") || cat.contains("Controller") {
                    if ctrl_cid.is_none() {
                        ctrl_cid = Some(info.cid);
                    }
                }
            }
        }

        let comp_cid = comp_cid.ok_or_else(|| "Could not find Audio Component Class CID".to_string())?;
        let ctrl_cid = ctrl_cid.unwrap_or(comp_cid);

        let host_app = create_host_application();
        let comp_handler = create_component_handler();

        // 1. Create Component
        println!("[1/5] Creating IComponent...");
        let mut component: *mut c_void = ptr::null_mut();
        let r = unsafe {
            (factory_vtbl.create_instance)(
                factory,
                &comp_cid,
                &IID_ICOMPONENT,
                &mut component,
            )
        };
        if r != K_RESULT_OK || component.is_null() {
            unsafe {
                let _ = FreeLibrary(hmodule);
            }
            return Err(format!("Failed to create IComponent instance: 0x{:08X}", r));
        }

        println!("[2/5] Initializing IComponent...");
        // Initialize Component with HostApplication
        let comp_vtbl = unsafe { &*(*(component as *mut *const IComponentVtbl)) };
        let r = unsafe { (comp_vtbl.initialize)(component, host_app as *mut c_void) };
        if r != K_RESULT_OK {
            return Err(format!("IComponent::initialize failed: 0x{:08X}", r));
        }

        println!("[3/5] Creating IEditController...");
        // 2. Create EditController
        let mut controller: *mut c_void = ptr::null_mut();
        let r = unsafe {
            (factory_vtbl.create_instance)(
                factory,
                &ctrl_cid,
                &IID_IEDIT_CONTROLLER,
                &mut controller,
            )
        };
        if r != K_RESULT_OK || controller.is_null() {
            return Err(format!("Failed to create IEditController instance: 0x{:08X}", r));
        }

        println!("[4/5] Initializing IEditController...");
        // Initialize Controller with HostApplication
        let ctrl_vtbl = unsafe { &*(*(controller as *mut *const IEditControllerVtbl)) };
        let r = unsafe { (ctrl_vtbl.initialize)(controller, host_app as *mut c_void) };
        if r != K_RESULT_OK {
            return Err(format!("IEditController::initialize failed: 0x{:08X}", r));
        }

        // Set Component Handler on Controller
        unsafe {
            (ctrl_vtbl.set_component_handler)(controller, comp_handler as *mut c_void);
        }

        println!("[5/5] Connecting IConnectionPoint...");
        // 3. Connect Component and Controller via IConnectionPoint
        let mut comp_cp: *mut c_void = ptr::null_mut();
        let r_cp1 = unsafe {
            (comp_vtbl.base.query_interface)(component, &IID_ICONNECTION_POINT, &mut comp_cp)
        };

        let mut ctrl_cp: *mut c_void = ptr::null_mut();
        let r_cp2 = unsafe {
            (ctrl_vtbl.base.query_interface)(controller, &IID_ICONNECTION_POINT, &mut ctrl_cp)
        };

        if r_cp1 == K_RESULT_OK && !comp_cp.is_null() && r_cp2 == K_RESULT_OK && !ctrl_cp.is_null() {
            let comp_cp_vtbl = unsafe { &*(*(comp_cp as *mut *const IConnectionPointVtbl)) };
            let ctrl_cp_vtbl = unsafe { &*(*(ctrl_cp as *mut *const IConnectionPointVtbl)) };

            println!("   Connecting comp -> ctrl...");
            unsafe {
                let _ = (comp_cp_vtbl.connect)(comp_cp, ctrl_cp);
            }
            println!("   Connecting ctrl -> comp...");
            unsafe {
                let _ = (ctrl_cp_vtbl.connect)(ctrl_cp, comp_cp);
            }
        }

        // 4. Synchronize Component State to Controller
        let mem_stream = create_memory_stream();
        unsafe {
            let _ = (comp_vtbl.get_state)(component, mem_stream as *mut c_void);
            let stream_vtbl = &*(*(mem_stream as *mut *const IBStreamVtbl));
            let mut res_pos = 0i64;
            let _ = (stream_vtbl.seek)(mem_stream as *mut c_void, 0, K_SEEK_SET, &mut res_pos);
            let _ = (ctrl_vtbl.set_component_state)(controller, mem_stream as *mut c_void);
            let _ = (stream_vtbl.base.release)(mem_stream as *mut c_void);
        }

        // 5. Query IAudioProcessor on Component
        let mut processor: *mut c_void = ptr::null_mut();
        let r = unsafe {
            (comp_vtbl.base.query_interface)(component, &IID_IAUDIO_PROCESSOR, &mut processor)
        };
        if r != K_RESULT_OK || processor.is_null() {
            return Err(format!("Failed to query IAudioProcessor: 0x{:08X}", r));
        }

        let plugin = Self {
            hmodule,
            factory,
            component,
            processor,
            controller,
            view: AtomicPtr::new(ptr::null_mut()),
            host_app,
            comp_handler,
            plug_frame: AtomicPtr::new(ptr::null_mut()),
            name: plugin_name,
            sample_rate_bits: AtomicU64::new(48000.0f64.to_bits()),
            max_block_size: AtomicUsize::new(512),
            latency: AtomicU32::new(0),
            view_size: Mutex::new(ViewRect::default()),
            sample_pos: AtomicI64::new(0),
            is_active: AtomicBool::new(false),
        };

        // Initialize audio engine parameters
        plugin.setup_audio(48000.0, 512)?;

        Ok(plugin)
    }

    pub fn sample_rate(&self) -> f64 {
        f64::from_bits(self.sample_rate_bits.load(Ordering::Acquire))
    }

    pub fn max_block_size(&self) -> usize {
        self.max_block_size.load(Ordering::Acquire)
    }

    /// Processing delay reported by the plugin, in samples
    pub fn latency(&self) -> u32 {
        self.latency.load(Ordering::Acquire)
    }

    pub fn view_size(&self) -> ViewRect {
        *self.view_size.lock()
    }

    /// Samples processed since the last `seek`
    pub fn sample_pos(&self) -> i64 {
        self.sample_pos.load(Ordering::Acquire)
    }

    /// Reconfigure rate and block size. Must not run concurrently with `process_stereo`.
    pub fn setup_audio(&self, sample_rate: f64, max_block_size: usize) -> Result<(), String> {
        let max_block_size = max_block_size.max(1);
        self.sample_rate_bits.store(sample_rate.to_bits(), Ordering::Release);
        self.max_block_size.store(max_block_size, Ordering::Release);

        let comp_vtbl = unsafe { &*(*(self.component as *mut *const IComponentVtbl)) };
        let proc_vtbl = unsafe { &*(*(self.processor as *mut *const IAudioProcessorVtbl)) };

        // VST3 requires bus arrangements and setupProcessing to happen while the component is inactive
        if self.is_active.swap(false, Ordering::AcqRel) {
            unsafe {
                let _ = (proc_vtbl.set_processing)(self.processor, 0);
                let _ = (comp_vtbl.set_active)(self.component, 0);
            }
        }

        // Stereo bus arrangement (Left + Right = 0x03)
        let stereo_speaker_arr: u64 = 0x03;
        unsafe {
            let _ = (proc_vtbl.set_bus_arrangements)(
                self.processor,
                &stereo_speaker_arr,
                1,
                &stereo_speaker_arr,
                1,
            );
        }

        let setup = ProcessSetup {
            process_mode: K_REALTIME,
            symbolic_sample_size: K_SAMPLE_32,
            max_samples_per_block: max_block_size as i32,
            sample_rate,
        };

        let r = unsafe { (proc_vtbl.setup_processing)(self.processor, &setup) };
        if r != K_RESULT_OK {
            return Err(format!("IAudioProcessor::setup_processing failed: 0x{:08X}", r));
        }

        // Activate Stereo Input Bus 0 and Output Bus 0
        unsafe {
            let _ = (comp_vtbl.activate_bus)(self.component, K_AUDIO, K_INPUT, 0, 1);
            let _ = (comp_vtbl.activate_bus)(self.component, K_AUDIO, K_OUTPUT, 0, 1);
            let _ = (comp_vtbl.set_active)(self.component, 1);
            let _ = (proc_vtbl.set_processing)(self.processor, 1);
            self.latency.store((proc_vtbl.get_latency_samples)(self.processor), Ordering::Release);
        }
        self.is_active.store(true, Ordering::Release);

        Ok(())
    }

    pub fn create_view(&self, parent_hwnd: HWND) -> Result<ViewRect, String> {
        self.destroy_view();

        let ctrl_vtbl = unsafe { &*(*(self.controller as *mut *const IEditControllerVtbl)) };
        let editor_name = CString::new("editor").unwrap();
        let view_ptr = unsafe { (ctrl_vtbl.create_view)(self.controller, editor_name.as_ptr()) };

        if view_ptr.is_null() {
            return Err("IEditController::create_view(\"editor\") returned null".to_string());
        }

        self.view.store(view_ptr, Ordering::Release);
        let view_vtbl = unsafe { &*(*(view_ptr as *mut *const IPlugViewVtbl)) };

        let platform_hwnd = CString::new("HWND").unwrap();
        let is_supp = unsafe { (view_vtbl.is_platform_type_supported)(view_ptr, platform_hwnd.as_ptr()) };
        if is_supp != K_RESULT_OK {
            return Err("IPlugView does not support platform HWND".to_string());
        }

        let mut rect = ViewRect::default();
        let r = unsafe { (view_vtbl.get_size)(view_ptr, &mut rect) };
        if r != K_RESULT_OK {
            rect = ViewRect { left: 0, top: 0, right: 1024, bottom: 680 };
        }
        *self.view_size.lock() = rect;

        // Set PlugFrame for dynamic resize notifications
        let parent_raw = parent_hwnd.0 as usize;
        let plug_frame = create_plug_frame(move |new_rect| {
            // Grow or shrink the host window so its client area fits the editor
            unsafe {
                use windows::Win32::Foundation::RECT;
                use windows::Win32::UI::WindowsAndMessaging::{
                    AdjustWindowRectEx, GetWindowLongW, SetWindowPos, GWL_EXSTYLE, GWL_STYLE, SWP_NOMOVE, SWP_NOZORDER,
                    WINDOW_EX_STYLE, WINDOW_STYLE,
                };
                let parent = HWND(parent_raw as *mut _);
                let mut r = RECT { left: 0, top: 0, right: new_rect.width(), bottom: new_rect.height() };
                let style = WINDOW_STYLE(GetWindowLongW(parent, GWL_STYLE) as u32);
                let ex = WINDOW_EX_STYLE(GetWindowLongW(parent, GWL_EXSTYLE) as u32);
                let _ = AdjustWindowRectEx(&mut r, style, false, ex);
                let _ = SetWindowPos(parent, None, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOMOVE);
            }
        });
        self.plug_frame.store(plug_frame, Ordering::Release);
        unsafe {
            let _ = (view_vtbl.set_frame)(view_ptr, plug_frame as *mut c_void);
        }

        // Attach view to parent HWND container
        let r_attach = unsafe {
            (view_vtbl.attached)(view_ptr, parent_hwnd.0 as *mut c_void, platform_hwnd.as_ptr())
        };
        if r_attach != K_RESULT_OK {
            return Err(format!("IPlugView::attached failed with code 0x{:08X}", r_attach));
        }

        Ok(rect)
    }

    /// Detach and release the editor (UI thread only)
    pub fn destroy_view(&self) {
        let view = self.view.swap(ptr::null_mut(), Ordering::AcqRel);
        if !view.is_null() {
            let view_vtbl = unsafe { &*(*(view as *mut *const IPlugViewVtbl)) };
            unsafe {
                let _ = (view_vtbl.removed)(view);
                let _ = (view_vtbl.base.release)(view);
            }
        }
        let frame = self.plug_frame.swap(ptr::null_mut(), Ordering::AcqRel);
        if !frame.is_null() {
            unsafe {
                let frame_vtbl = &*(*(frame as *mut *const IPlugFrameVtbl));
                let _ = (frame_vtbl.base.release)(frame as *mut c_void);
            }
        }
    }

    pub fn has_view(&self) -> bool {
        !self.view.load(Ordering::Acquire).is_null()
    }

    pub fn on_wheel(&self, distance: f32) {
        let view = self.view.load(Ordering::Acquire);
        if !view.is_null() {
            let view_vtbl = unsafe { &*(*(view as *mut *const IPlugViewVtbl)) };
            let _ = unsafe { (view_vtbl.on_wheel)(view, distance) };
        }
    }

    /// Process a stereo buffer of any length, split into blocks no larger than `max_block_size`
    pub fn process_stereo(
        &self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let num_samples = in_l.len().min(in_r.len()).min(out_l.len()).min(out_r.len());
        if num_samples == 0 || self.processor.is_null() {
            return;
        }
        let block = self.max_block_size().max(1);
        let mut offset = 0;
        while offset < num_samples {
            let end = (offset + block).min(num_samples);
            self.process_block(
                &in_l[offset..end],
                &in_r[offset..end],
                &mut out_l[offset..end],
                &mut out_r[offset..end],
            );
            offset = end;
        }
    }

    fn process_block(
        &self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let num_samples = in_l.len();

        let proc_vtbl = unsafe { &*(*(self.processor as *mut *const IAudioProcessorVtbl)) };

        let mut in_ptrs: [*mut f32; 2] = [in_l.as_ptr() as *mut f32, in_r.as_ptr() as *mut f32];
        let mut out_ptrs: [*mut f32; 2] = [out_l.as_mut_ptr(), out_r.as_mut_ptr()];

        let mut in_bus = AudioBusBuffers {
            num_channels: 2,
            silence_flags: 0,
            channel_buffers32: in_ptrs.as_mut_ptr(),
        };

        let mut out_bus = AudioBusBuffers {
            num_channels: 2,
            silence_flags: 0,
            channel_buffers32: out_ptrs.as_mut_ptr(),
        };

        let mut context = ProcessContext {
            state: (1 << 1) | (1 << 8) | (1 << 9) | (1 << 13) | (1 << 14), // kPlaying, kSystemTimeValid, kContTimeValid, kTempoValid, kTimeSigValid
            sample_rate: self.sample_rate(),
            project_time_samples: self.sample_pos(),
            system_time: 0,
            continuous_time_samples: self.sample_pos(),
            project_time_music: 0.0,
            bar_position_music: 0.0,
            cycle_start_music: 0.0,
            cycle_end_music: 0.0,
            tempo: 120.0,
            time_sig_numerator: 4,
            time_sig_denominator: 4,
            chord_key: 0,
            chord_mask: 0,
            reserved: [0; 6],
            frame_rate_frame_rate: 0,
            frame_rate_flags: 0,
            samples_to_next_clock: 0,
        };

        let mut process_data = ProcessData {
            process_mode: K_REALTIME,
            symbolic_sample_size: K_SAMPLE_32,
            num_samples: num_samples as i32,
            num_inputs: 1,
            num_outputs: 1,
            inputs: &mut in_bus,
            outputs: &mut out_bus,
            input_parameter_changes: ptr::null_mut(),
            output_parameter_changes: ptr::null_mut(),
            input_events: ptr::null_mut(),
            output_events: ptr::null_mut(),
            process_context: &mut context,
        };

        unsafe {
            let _ = (proc_vtbl.process)(self.processor, &mut process_data);
        }
        self.sample_pos.fetch_add(num_samples as i64, Ordering::AcqRel);
    }

    pub fn seek(&self, sample_pos: i64) {
        self.sample_pos.store(sample_pos, Ordering::Release);
    }

    /// Save the plugin's complete serialized state to bytes
    pub fn save_state(&self) -> Result<Vec<u8>, String> {
        if self.component.is_null() {
            return Err("Component is null".to_string());
        }
        let comp_vtbl = unsafe { &*(*(self.component as *mut *const IComponentVtbl)) };
        let mem_stream = create_memory_stream();

        unsafe {
            let r = (comp_vtbl.get_state)(self.component, mem_stream as *mut c_void);
            if r != K_RESULT_OK {
                let stream_vtbl = &*(*(mem_stream as *mut *const IBStreamVtbl));
                let _ = (stream_vtbl.base.release)(mem_stream as *mut c_void);
                return Err(format!("IComponent::get_state failed: 0x{:08X}", r));
            }

            let stream_com = &*(mem_stream as *mut MemoryStreamCom);
            let bytes = stream_com.data.buffer.lock().clone();
            let stream_vtbl = &*(*(mem_stream as *mut *const IBStreamVtbl));
            let _ = (stream_vtbl.base.release)(mem_stream as *mut c_void);
            Ok(bytes)
        }
    }

    /// Load a serialized state into the plugin and synchronize it to the controller
    pub fn load_state(&self, data: &[u8]) -> Result<(), String> {
        if self.component.is_null() || self.controller.is_null() {
            return Err("Component or Controller is null".to_string());
        }
        let comp_vtbl = unsafe { &*(*(self.component as *mut *const IComponentVtbl)) };
        let ctrl_vtbl = unsafe { &*(*(self.controller as *mut *const IEditControllerVtbl)) };

        let mem_stream = create_memory_stream();
        unsafe {
            let stream_com = &*(mem_stream as *mut MemoryStreamCom);
            *stream_com.data.buffer.lock() = data.to_vec();

            let stream_vtbl = &*(*(mem_stream as *mut *const IBStreamVtbl));
            let mut res_pos = 0i64;
            let _ = (stream_vtbl.seek)(mem_stream as *mut c_void, 0, K_SEEK_SET, &mut res_pos);

            let r_comp = (comp_vtbl.set_state)(self.component, mem_stream as *mut c_void);
            let _ = (stream_vtbl.seek)(mem_stream as *mut c_void, 0, K_SEEK_SET, &mut res_pos);
            let r_ctrl = (ctrl_vtbl.set_component_state)(self.controller, mem_stream as *mut c_void);
            let _ = (stream_vtbl.base.release)(mem_stream as *mut c_void);

            if r_comp != K_RESULT_OK {
                return Err(format!("IComponent::set_state failed: 0x{:08X}", r_comp));
            }
            if r_ctrl != K_RESULT_OK {
                return Err(format!("IEditController::set_component_state failed: 0x{:08X}", r_ctrl));
            }
        }
        Ok(())
    }
}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        self.destroy_view();

        if !self.processor.is_null() {
            let proc_vtbl = unsafe { &*(*(self.processor as *mut *const IAudioProcessorVtbl)) };
            unsafe {
                if self.is_active.load(Ordering::Acquire) {
                    let _ = (proc_vtbl.set_processing)(self.processor, 0);
                }
                let _ = (proc_vtbl.base.release)(self.processor);
            }
            self.processor = ptr::null_mut();
        }

        if !self.component.is_null() {
            let comp_vtbl = unsafe { &*(*(self.component as *mut *const IComponentVtbl)) };
            unsafe {
                if self.is_active.load(Ordering::Acquire) {
                    let _ = (comp_vtbl.set_active)(self.component, 0);
                }
                let _ = (comp_vtbl.terminate)(self.component);
                let _ = (comp_vtbl.base.release)(self.component);
            }
            self.component = ptr::null_mut();
        }

        if !self.controller.is_null() {
            let ctrl_vtbl = unsafe { &*(*(self.controller as *mut *const IEditControllerVtbl)) };
            unsafe {
                let _ = (ctrl_vtbl.terminate)(self.controller);
                let _ = (ctrl_vtbl.base.release)(self.controller);
            }
            self.controller = ptr::null_mut();
        }

        if !self.host_app.is_null() {
            unsafe {
                let host_vtbl = &*(*(self.host_app as *mut *const IHostApplicationVtbl));
                let _ = (host_vtbl.base.release)(self.host_app as *mut c_void);
            }
            self.host_app = ptr::null_mut();
        }

        if !self.comp_handler.is_null() {
            unsafe {
                let h_vtbl = &*(*(self.comp_handler as *mut *const IComponentHandlerVtbl));
                let _ = (h_vtbl.base.release)(self.comp_handler as *mut c_void);
            }
            self.comp_handler = ptr::null_mut();
        }

        if !self.factory.is_null() {
            unsafe {
                let f_vtbl = &*(*(self.factory as *mut *const IPluginFactoryVtbl));
                let _ = (f_vtbl.base.release)(self.factory);
            }
            self.factory = ptr::null_mut();
        }

        if !self.hmodule.is_invalid() {
            unsafe {
                let _ = FreeLibrary(self.hmodule);
            }
        }
    }
}

