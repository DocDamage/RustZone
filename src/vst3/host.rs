use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use parking_lot::Mutex;

use super::types::*;

/// COM Object layout: a pointer to a vtable pointer, followed by struct data
#[repr(C)]
pub struct ComObject<T, V> {
    pub vtbl: *const V,
    pub ref_count: AtomicU32,
    pub data: T,
}

// ----------------------------------------------------------------------------
// HostAttributeList: the standard typed key/value store plugins attach to IMessages
// ----------------------------------------------------------------------------
pub enum AttrValue {
    Int(i64),
    Float(f64),
    /// UTF-16 without terminator
    String(Vec<u16>),
    Binary(Vec<u8>),
}

pub struct HostAttributeListData {
    pub values: Mutex<HashMap<String, AttrValue>>,
}

pub type HostAttributeListCom = ComObject<HostAttributeListData, IAttributeListVtbl>;

static ATTR_LIST_VTBL: IAttributeListVtbl = IAttributeListVtbl {
    base: FUnknownVtbl {
        query_interface: attr_list_qi,
        add_ref: attr_list_add_ref,
        release: attr_list_release,
    },
    set_int: attr_set_int,
    get_int: attr_get_int,
    set_float: attr_set_float,
    get_float: attr_get_float,
    set_string: attr_set_string,
    get_string: attr_get_string,
    set_binary: attr_set_binary,
    get_binary: attr_get_binary,
};

unsafe extern "system" fn attr_list_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> i32 {
    if this.is_null() || iid.is_null() || obj.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let requested = *iid;
    if requested == IID_IATTRIBUTE_LIST || requested == IID_FUNKNOWN {
        attr_list_add_ref(this);
        *obj = this;
        K_RESULT_OK
    } else {
        *obj = ptr::null_mut();
        K_NO_INTERFACE
    }
}

unsafe extern "system" fn attr_list_add_ref(this: *mut c_void) -> u32 {
    let com = &*(this as *mut HostAttributeListCom);
    com.ref_count.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn attr_list_release(this: *mut c_void) -> u32 {
    let com = &*(this as *mut HostAttributeListCom);
    let prev = com.ref_count.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        let _ = Box::from_raw(this as *mut HostAttributeListCom);
        0
    } else {
        prev - 1
    }
}

/// The list and the attribute key, or None for bad arguments
unsafe fn attr_key<'a>(this: *mut c_void, id: *const c_char) -> Option<(&'a HostAttributeListData, String)> {
    if this.is_null() || id.is_null() {
        return None;
    }
    let com = &*(this as *mut HostAttributeListCom);
    let key = CStr::from_ptr(id).to_str().ok()?.to_string();
    Some((&com.data, key))
}

unsafe extern "system" fn attr_set_int(this: *mut c_void, id: *const c_char, value: i64) -> i32 {
    let Some((d, key)) = attr_key(this, id) else { return K_INVALID_ARGUMENT };
    d.values.lock().insert(key, AttrValue::Int(value));
    K_RESULT_OK
}

unsafe extern "system" fn attr_get_int(this: *mut c_void, id: *const c_char, value: *mut i64) -> i32 {
    let Some((d, key)) = attr_key(this, id) else { return K_INVALID_ARGUMENT };
    if value.is_null() {
        return K_INVALID_ARGUMENT;
    }
    match d.values.lock().get(&key) {
        Some(AttrValue::Int(v)) => {
            *value = *v;
            K_RESULT_OK
        }
        _ => K_RESULT_FALSE,
    }
}

unsafe extern "system" fn attr_set_float(this: *mut c_void, id: *const c_char, value: f64) -> i32 {
    let Some((d, key)) = attr_key(this, id) else { return K_INVALID_ARGUMENT };
    d.values.lock().insert(key, AttrValue::Float(value));
    K_RESULT_OK
}

unsafe extern "system" fn attr_get_float(this: *mut c_void, id: *const c_char, value: *mut f64) -> i32 {
    let Some((d, key)) = attr_key(this, id) else { return K_INVALID_ARGUMENT };
    if value.is_null() {
        return K_INVALID_ARGUMENT;
    }
    match d.values.lock().get(&key) {
        Some(AttrValue::Float(v)) => {
            *value = *v;
            K_RESULT_OK
        }
        _ => K_RESULT_FALSE,
    }
}

unsafe extern "system" fn attr_set_string(this: *mut c_void, id: *const c_char, string: *const u16) -> i32 {
    let Some((d, key)) = attr_key(this, id) else { return K_INVALID_ARGUMENT };
    if string.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let mut len = 0usize;
    while *string.add(len) != 0 && len < (1 << 20) {
        len += 1;
    }
    let text = std::slice::from_raw_parts(string, len).to_vec();
    d.values.lock().insert(key, AttrValue::String(text));
    K_RESULT_OK
}

unsafe extern "system" fn attr_get_string(this: *mut c_void, id: *const c_char, string: *mut u16, size_in_bytes: u32) -> i32 {
    let Some((d, key)) = attr_key(this, id) else { return K_INVALID_ARGUMENT };
    if string.is_null() || size_in_bytes < 2 {
        return K_INVALID_ARGUMENT;
    }
    match d.values.lock().get(&key) {
        Some(AttrValue::String(text)) => {
            // Copy what fits, always NUL-terminated
            let capacity = (size_in_bytes / 2) as usize;
            let n = text.len().min(capacity - 1);
            ptr::copy_nonoverlapping(text.as_ptr(), string, n);
            *string.add(n) = 0;
            K_RESULT_OK
        }
        _ => K_RESULT_FALSE,
    }
}

unsafe extern "system" fn attr_set_binary(this: *mut c_void, id: *const c_char, data: *const c_void, size_in_bytes: u32) -> i32 {
    let Some((d, key)) = attr_key(this, id) else { return K_INVALID_ARGUMENT };
    if data.is_null() && size_in_bytes > 0 {
        return K_INVALID_ARGUMENT;
    }
    let bytes = if size_in_bytes == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(data as *const u8, size_in_bytes as usize).to_vec()
    };
    d.values.lock().insert(key, AttrValue::Binary(bytes));
    K_RESULT_OK
}

unsafe extern "system" fn attr_get_binary(this: *mut c_void, id: *const c_char, data: *mut *const c_void, size_in_bytes: *mut u32) -> i32 {
    let Some((d, key)) = attr_key(this, id) else { return K_INVALID_ARGUMENT };
    if data.is_null() || size_in_bytes.is_null() {
        return K_INVALID_ARGUMENT;
    }
    // The pointer stays valid for as long as the attribute list (and this entry) lives
    match d.values.lock().get(&key) {
        Some(AttrValue::Binary(bytes)) => {
            *data = bytes.as_ptr() as *const c_void;
            *size_in_bytes = bytes.len() as u32;
            K_RESULT_OK
        }
        _ => {
            *data = ptr::null();
            *size_in_bytes = 0;
            K_RESULT_FALSE
        }
    }
}

pub fn create_attribute_list() -> *mut HostAttributeListCom {
    Box::into_raw(Box::new(HostAttributeListCom {
        vtbl: &ATTR_LIST_VTBL,
        ref_count: AtomicU32::new(1),
        data: HostAttributeListData {
            values: Mutex::new(HashMap::new()),
        },
    }))
}

// ----------------------------------------------------------------------------
// HostMessage
// ----------------------------------------------------------------------------
pub struct HostMessageData {
    pub id: Mutex<CString>,
    pub attr: *mut HostAttributeListCom,
}

pub type HostMessageCom = ComObject<HostMessageData, IMessageVtbl>;

static MSG_VTBL: IMessageVtbl = IMessageVtbl {
    base: FUnknownVtbl {
        query_interface: msg_qi,
        add_ref: msg_add_ref,
        release: msg_release,
    },
    get_message_id: msg_get_id,
    set_message_id: msg_set_id,
    get_attributes: msg_get_attributes,
};

unsafe extern "system" fn msg_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> i32 {
    if this.is_null() || iid.is_null() || obj.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let requested = *iid;
    if requested == IID_IMESSAGE || requested == IID_FUNKNOWN {
        msg_add_ref(this);
        *obj = this;
        K_RESULT_OK
    } else {
        *obj = ptr::null_mut();
        K_NO_INTERFACE
    }
}

unsafe extern "system" fn msg_add_ref(this: *mut c_void) -> u32 {
    let com = &*(this as *mut HostMessageCom);
    com.ref_count.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn msg_release(this: *mut c_void) -> u32 {
    let com = &*(this as *mut HostMessageCom);
    let prev = com.ref_count.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        let boxed = Box::from_raw(this as *mut HostMessageCom);
        if !boxed.data.attr.is_null() {
            attr_list_release(boxed.data.attr as *mut c_void);
        }
        0
    } else {
        prev - 1
    }
}

unsafe extern "system" fn msg_get_id(this: *mut c_void) -> *const c_char {
    let com = &*(this as *mut HostMessageCom);
    com.data.id.lock().as_ptr()
}

unsafe extern "system" fn msg_set_id(this: *mut c_void, id: *const c_char) -> i32 {
    if this.is_null() || id.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let com = &*(this as *mut HostMessageCom);
    let cstr = CStr::from_ptr(id);
    *com.data.id.lock() = cstr.to_owned();
    K_RESULT_OK
}

unsafe extern "system" fn msg_get_attributes(this: *mut c_void) -> *mut c_void {
    let com = &*(this as *mut HostMessageCom);
    com.data.attr as *mut c_void
}

pub fn create_host_message() -> *mut HostMessageCom {
    let attr = create_attribute_list();
    Box::into_raw(Box::new(HostMessageCom {
        vtbl: &MSG_VTBL,
        ref_count: AtomicU32::new(1),
        data: HostMessageData {
            id: Mutex::new(CString::default()),
            attr,
        },
    }))
}

// ----------------------------------------------------------------------------
// HostApplication
// ----------------------------------------------------------------------------
pub struct HostAppData {
    pub app_name: Vec<u16>,
}

pub type HostAppCom = ComObject<HostAppData, IHostApplicationVtbl>;

static HOST_APP_VTBL: IHostApplicationVtbl = IHostApplicationVtbl {
    base: FUnknownVtbl {
        query_interface: host_app_qi,
        add_ref: host_app_add_ref,
        release: host_app_release,
    },
    get_name: host_app_get_name,
    create_instance: host_app_create_instance,
};

unsafe extern "system" fn host_app_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> i32 {
    if this.is_null() || iid.is_null() || obj.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let requested = *iid;
    if requested == IID_IHOST_APPLICATION || requested == IID_FUNKNOWN {
        host_app_add_ref(this);
        *obj = this;
        K_RESULT_OK
    } else {
        *obj = ptr::null_mut();
        K_NO_INTERFACE
    }
}

unsafe extern "system" fn host_app_add_ref(this: *mut c_void) -> u32 {
    let com = &*(this as *mut HostAppCom);
    com.ref_count.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn host_app_release(this: *mut c_void) -> u32 {
    let com = &*(this as *mut HostAppCom);
    let prev = com.ref_count.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        let _ = Box::from_raw(this as *mut HostAppCom);
        0
    } else {
        prev - 1
    }
}

unsafe extern "system" fn host_app_get_name(this: *mut c_void, name: *mut u16) -> i32 {
    if this.is_null() || name.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let com = &*(this as *mut HostAppCom);
    let len = com.data.app_name.len().min(127);
    ptr::copy_nonoverlapping(com.data.app_name.as_ptr(), name, len);
    *name.add(len) = 0;
    K_RESULT_OK
}

unsafe extern "system" fn host_app_create_instance(
    _this: *mut c_void,
    cid: *const TUID,
    iid: *const TUID,
    obj: *mut *mut c_void,
) -> i32 {
    if cid.is_null() || iid.is_null() || obj.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let requested_cid = *cid;
    let requested_iid = *iid;

    if requested_cid == IID_IMESSAGE || requested_iid == IID_IMESSAGE {
        let msg = create_host_message();
        *obj = msg as *mut c_void;
        K_RESULT_OK
    } else if requested_cid == IID_IATTRIBUTE_LIST || requested_iid == IID_IATTRIBUTE_LIST {
        let attr = create_attribute_list();
        *obj = attr as *mut c_void;
        K_RESULT_OK
    } else {
        *obj = ptr::null_mut();
        K_NOT_IMPLEMENTED
    }
}

pub fn create_host_application() -> *mut HostAppCom {
    let wide_name: Vec<u16> = "Mastering Studio\0".encode_utf16().collect();
    Box::into_raw(Box::new(HostAppCom {
        vtbl: &HOST_APP_VTBL,
        ref_count: AtomicU32::new(1),
        data: HostAppData {
            app_name: wide_name,
        },
    }))
}

// ----------------------------------------------------------------------------
// HostComponentHandler
// ----------------------------------------------------------------------------
pub struct HostComponentHandlerData;
pub type HostComponentHandlerCom = ComObject<HostComponentHandlerData, IComponentHandlerVtbl>;

static COMPONENT_HANDLER_VTBL: IComponentHandlerVtbl = IComponentHandlerVtbl {
    base: FUnknownVtbl {
        query_interface: comp_handler_qi,
        add_ref: comp_handler_add_ref,
        release: comp_handler_release,
    },
    begin_edit: comp_handler_begin_edit,
    perform_edit: comp_handler_perform_edit,
    end_edit: comp_handler_end_edit,
    restart_component: comp_handler_restart_component,
};

unsafe extern "system" fn comp_handler_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> i32 {
    if this.is_null() || iid.is_null() || obj.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let requested = *iid;
    if requested == IID_ICOMPONENT_HANDLER || requested == IID_FUNKNOWN {
        comp_handler_add_ref(this);
        *obj = this;
        K_RESULT_OK
    } else {
        *obj = ptr::null_mut();
        K_NO_INTERFACE
    }
}

unsafe extern "system" fn comp_handler_add_ref(this: *mut c_void) -> u32 {
    let com = &*(this as *mut HostComponentHandlerCom);
    com.ref_count.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn comp_handler_release(this: *mut c_void) -> u32 {
    let com = &*(this as *mut HostComponentHandlerCom);
    let prev = com.ref_count.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        let _ = Box::from_raw(this as *mut HostComponentHandlerCom);
        0
    } else {
        prev - 1
    }
}

unsafe extern "system" fn comp_handler_begin_edit(_this: *mut c_void, _id: u32) -> i32 {
    K_RESULT_OK
}

unsafe extern "system" fn comp_handler_perform_edit(_this: *mut c_void, _id: u32, _value_norm: f64) -> i32 {
    K_RESULT_OK
}

unsafe extern "system" fn comp_handler_end_edit(_this: *mut c_void, _id: u32) -> i32 {
    K_RESULT_OK
}

unsafe extern "system" fn comp_handler_restart_component(_this: *mut c_void, _flags: i32) -> i32 {
    K_RESULT_OK
}

pub fn create_component_handler() -> *mut HostComponentHandlerCom {
    Box::into_raw(Box::new(HostComponentHandlerCom {
        vtbl: &COMPONENT_HANDLER_VTBL,
        ref_count: AtomicU32::new(1),
        data: HostComponentHandlerData,
    }))
}

// ----------------------------------------------------------------------------
// HostPlugFrame
// ----------------------------------------------------------------------------
pub struct HostPlugFrameData {
    pub on_resize: Box<dyn Fn(ViewRect) + Send + Sync>,
}

pub type HostPlugFrameCom = ComObject<HostPlugFrameData, IPlugFrameVtbl>;

static PLUG_FRAME_VTBL: IPlugFrameVtbl = IPlugFrameVtbl {
    base: FUnknownVtbl {
        query_interface: plug_frame_qi,
        add_ref: plug_frame_add_ref,
        release: plug_frame_release,
    },
    resize_view: plug_frame_resize_view,
};

unsafe extern "system" fn plug_frame_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> i32 {
    if this.is_null() || iid.is_null() || obj.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let requested = *iid;
    if requested == IID_IPLUG_FRAME || requested == IID_FUNKNOWN {
        plug_frame_add_ref(this);
        *obj = this;
        K_RESULT_OK
    } else {
        *obj = ptr::null_mut();
        K_NO_INTERFACE
    }
}

unsafe extern "system" fn plug_frame_add_ref(this: *mut c_void) -> u32 {
    let com = &*(this as *mut HostPlugFrameCom);
    com.ref_count.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn plug_frame_release(this: *mut c_void) -> u32 {
    let com = &*(this as *mut HostPlugFrameCom);
    let prev = com.ref_count.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        let _ = Box::from_raw(this as *mut HostPlugFrameCom);
        0
    } else {
        prev - 1
    }
}

unsafe extern "system" fn plug_frame_resize_view(
    this: *mut c_void,
    view: *mut c_void,
    new_size: *mut ViewRect,
) -> i32 {
    if this.is_null() || new_size.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let com = &*(this as *mut HostPlugFrameCom);
    let mut rect = *new_size;
    if !view.is_null() {
        let view_vtbl = &*(*(view as *mut *const IPlugViewVtbl));
        let _ = (view_vtbl.on_size)(view, &mut rect);
    }
    (com.data.on_resize)(rect);
    K_RESULT_OK
}

pub fn create_plug_frame<F>(on_resize: F) -> *mut HostPlugFrameCom
where
    F: Fn(ViewRect) + Send + Sync + 'static,
{
    Box::into_raw(Box::new(HostPlugFrameCom {
        vtbl: &PLUG_FRAME_VTBL,
        ref_count: AtomicU32::new(1),
        data: HostPlugFrameData {
            on_resize: Box::new(on_resize),
        },
    }))
}

// ----------------------------------------------------------------------------
// MemoryStream (IBStream implementation)
// ----------------------------------------------------------------------------
pub struct MemoryStreamData {
    pub buffer: Mutex<Vec<u8>>,
    pub position: Mutex<usize>,
}

pub type MemoryStreamCom = ComObject<MemoryStreamData, IBStreamVtbl>;

static MEM_STREAM_VTBL: IBStreamVtbl = IBStreamVtbl {
    base: FUnknownVtbl {
        query_interface: mem_stream_qi,
        add_ref: mem_stream_add_ref,
        release: mem_stream_release,
    },
    read: mem_stream_read,
    write: mem_stream_write,
    seek: mem_stream_seek,
    tell: mem_stream_tell,
};

unsafe extern "system" fn mem_stream_qi(this: *mut c_void, iid: *const TUID, obj: *mut *mut c_void) -> i32 {
    if this.is_null() || iid.is_null() || obj.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let requested = *iid;
    if requested == IID_IBSTREAM || requested == IID_FUNKNOWN {
        mem_stream_add_ref(this);
        *obj = this;
        K_RESULT_OK
    } else {
        *obj = ptr::null_mut();
        K_NO_INTERFACE
    }
}

unsafe extern "system" fn mem_stream_add_ref(this: *mut c_void) -> u32 {
    let com = &*(this as *mut MemoryStreamCom);
    com.ref_count.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn mem_stream_release(this: *mut c_void) -> u32 {
    let com = &*(this as *mut MemoryStreamCom);
    let prev = com.ref_count.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        let _ = Box::from_raw(this as *mut MemoryStreamCom);
        0
    } else {
        prev - 1
    }
}

unsafe extern "system" fn mem_stream_read(
    this: *mut c_void,
    buffer: *mut c_void,
    num_bytes: i32,
    num_bytes_read: *mut i32,
) -> i32 {
    if this.is_null() || buffer.is_null() || num_bytes < 0 {
        return K_INVALID_ARGUMENT;
    }
    let com = &*(this as *mut MemoryStreamCom);
    let buf = com.data.buffer.lock();
    let mut pos = com.data.position.lock();

    let avail = buf.len().saturating_sub(*pos);
    let to_read = (num_bytes as usize).min(avail);

    if to_read > 0 {
        ptr::copy_nonoverlapping(buf.as_ptr().add(*pos), buffer as *mut u8, to_read);
        *pos += to_read;
    }

    if !num_bytes_read.is_null() {
        *num_bytes_read = to_read as i32;
    }

    K_RESULT_OK
}

unsafe extern "system" fn mem_stream_write(
    this: *mut c_void,
    buffer: *const c_void,
    num_bytes: i32,
    num_bytes_written: *mut i32,
) -> i32 {
    if this.is_null() || buffer.is_null() || num_bytes < 0 {
        return K_INVALID_ARGUMENT;
    }
    let com = &*(this as *mut MemoryStreamCom);
    let mut buf = com.data.buffer.lock();
    let mut pos = com.data.position.lock();

    let n = num_bytes as usize;
    let end = *pos + n;

    if end > buf.len() {
        buf.resize(end, 0);
    }

    ptr::copy_nonoverlapping(buffer as *const u8, buf.as_mut_ptr().add(*pos), n);
    *pos = end;

    if !num_bytes_written.is_null() {
        *num_bytes_written = num_bytes;
    }

    K_RESULT_OK
}

unsafe extern "system" fn mem_stream_seek(
    this: *mut c_void,
    pos: i64,
    mode: i32,
    result: *mut i64,
) -> i32 {
    if this.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let com = &*(this as *mut MemoryStreamCom);
    let buf = com.data.buffer.lock();
    let mut current_pos = com.data.position.lock();

    let new_pos: i64 = match mode {
        K_SEEK_SET => pos,
        K_SEEK_CUR => (*current_pos as i64) + pos,
        K_SEEK_END => (buf.len() as i64) + pos,
        _ => return K_INVALID_ARGUMENT,
    };

    if new_pos < 0 {
        return K_INVALID_ARGUMENT;
    }

    *current_pos = new_pos as usize;
    if !result.is_null() {
        *result = new_pos;
    }

    K_RESULT_OK
}

unsafe extern "system" fn mem_stream_tell(this: *mut c_void, result: *mut i64) -> i32 {
    if this.is_null() || result.is_null() {
        return K_INVALID_ARGUMENT;
    }
    let com = &*(this as *mut MemoryStreamCom);
    let pos = *com.data.position.lock();
    *result = pos as i64;
    K_RESULT_OK
}

pub fn create_memory_stream() -> *mut MemoryStreamCom {
    Box::into_raw(Box::new(MemoryStreamCom {
        vtbl: &MEM_STREAM_VTBL,
        ref_count: AtomicU32::new(1),
        data: MemoryStreamData {
            buffer: Mutex::new(Vec::new()),
            position: Mutex::new(0),
        },
    }))
}

