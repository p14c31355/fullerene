#![allow(dead_code)]

use core::fmt::{self, Write};

// UEFI の基本的なデータ型
pub type EfiHandle = *mut core::ffi::c_void;
pub type EfiStatus = usize;
pub type EfiEvent = *mut core::ffi::c_void;
pub type EfiLba = u64;
pub type EfiTpl = usize;
pub type EfiPhysicalAddress = u64;
pub type EfiVirtualAddress = u64;

// GUID 構造体
#[repr(C)]
pub struct EfiGuid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

// テーブルヘッダ
#[repr(C)]
pub struct EfiTableHeader {
    pub signature: u64,
    pub revision: u32,
    pub header_size: u32,
    pub crc32: u32,
    pub reserved: u32,
}

// EFI_TIME 構造体
#[repr(C)]
pub struct EfiTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub pad1: u8,
    pub nanosecond: u32,
    pub time_zone: i16,
    pub daylight: u8,
    pub pad2: u8,
}

// EFI_BOOT_SERVICES 構造体 (一部のみ定義)
#[repr(C)]
pub struct EfiBootServices {
    pub hdr: EfiTableHeader,
    // 以下、ブートサービス関数のポインタが続く
    // ここでは、必要なものだけを定義する
    pub raise_tpl: extern "win64" fn(new_tpl: EfiTpl) -> EfiTpl,
    pub restore_tpl: extern "win64" fn(old_tpl: EfiTpl),
    pub allocate_pages: extern "win64" fn(
        _type: EfiAllocateType,
        _memory_type: EfiMemoryType,
        _pages: usize,
        _physical_address: &mut EfiPhysicalAddress,
    ) -> EfiStatus,
    pub free_pages: extern "win64" fn(
        _physical_address: EfiPhysicalAddress,
        _pages: usize,
    ) -> EfiStatus,
    pub get_memory_map: extern "win64" fn(
        _memory_map_size: &mut usize,
        _memory_map: *mut EfiMemoryDescriptor,
        _map_key: &mut usize,
        _descriptor_size: &mut usize,
        _descriptor_version: &mut u32,
    ) -> EfiStatus,
    pub allocate_pool: extern "win64" fn(
        _pool_type: EfiMemoryType,
        _size: usize,
        _buffer: &mut *mut core::ffi::c_void,
    ) -> EfiStatus,
    pub free_pool: extern "win64" fn(
        _buffer: *mut core::ffi::c_void,
    ) -> EfiStatus,
    pub create_event: extern "win64" fn(
        _type: u32,
        _notify_tpl: EfiTpl,
        _notify_function: EfiEventNotify,
        _notify_context: *mut core::ffi::c_void,
        _event: &mut EfiEvent,
    ) -> EfiStatus,
    pub set_timer: extern "win64" fn(
        _event: EfiEvent,
        _type: EfiTimerDelay,
        _trigger_time: u64,
    ) -> EfiStatus,
    pub wait_for_event: extern "win64" fn(
        _number_of_events: usize,
        _event: *mut EfiEvent,
        _index: &mut usize,
    ) -> EfiStatus,
    pub signal_event: extern "win64" fn(
        _event: EfiEvent,
    ) -> EfiStatus,
    pub close_event: extern "win64" fn(
        _event: EfiEvent,
    ) -> EfiStatus,
    pub check_event: extern "win64" fn(
        _event: EfiEvent,
    ) -> EfiStatus,
    pub install_protocol_interface: extern "win64" fn(
        _handle: &mut EfiHandle,
        _protocol: &EfiGuid,
        _interface_type: EfiInterfaceType,
        _interface: *mut core::ffi::c_void,
    ) -> EfiStatus,
    pub uninstall_protocol_interface: extern "win64" fn(
        _handle: EfiHandle,
        _protocol: &EfiGuid,
        _interface: *mut core::ffi::c_void,
    ) -> EfiStatus,
    pub handle_protocol: extern "win64" fn(
        _handle: EfiHandle,
        _protocol: &EfiGuid,
        _interface: &mut *mut core::ffi::c_void,
    ) -> EfiStatus,
    pub register_protocol_notify: extern "win64" fn(
        _protocol: &EfiGuid,
        _event: EfiEvent,
        _registration: &mut *mut core::ffi::c_void,
    ) -> EfiStatus,
    pub locate_handle: extern "win64" fn(
        _search_type: EfiLocateSearchType,
        _protocol: &EfiGuid,
        _search_key: *mut core::ffi::c_void,
        _buffer_size: &mut usize,
        _buffer: &mut EfiHandle,
    ) -> EfiStatus,
    pub locate_device_path: extern "win64" fn(
        _protocol: &EfiGuid,
        _device_path: &mut *mut EfiDevicePathProtocol,
        _handle: &mut EfiHandle,
    ) -> EfiStatus,
    pub install_configuration_table: extern "win64" fn(
        _guid: &EfiGuid,
        _table: *mut core::ffi::c_void,
    ) -> EfiStatus,
    pub load_image: extern "win64" fn(
        _boot_policy: bool,
        _parent_image_handle: EfiHandle,
        _device_path: &EfiDevicePathProtocol,
        _source_buffer: *mut core::ffi::c_void,
        _source_size: usize,
        _image_handle: &mut EfiHandle,
    ) -> EfiStatus,
    pub start_image: extern "win64" fn(
        _image_handle: EfiHandle,
        _exit_data_size: &mut usize,
        _exit_data: &mut *mut u16,
    ) -> EfiStatus,
    pub exit_image: extern "win64" fn(
        _image_handle: EfiHandle,
        _exit_status: EfiStatus,
        _exit_data_size: usize,
        _exit_data: *mut u16,
    ) -> EfiStatus,
    pub unload_image: extern "win64" fn(
        _image_handle: EfiHandle,
    ) -> EfiStatus,
    pub exit_boot_services: extern "win64" fn(
        _image_handle: EfiHandle,
        _map_key: usize,
    ) -> EfiStatus,
    pub get_next_monotonic_count: extern "win64" fn(
        _count: &mut u64,
    ) -> EfiStatus,
    pub stall: extern "win64" fn(
        _microseconds: usize,
    ) -> EfiStatus,
    pub set_watchdog_timer: extern "win64" fn(
        _timeout: usize,
        _watchlist: u64,
        _code_len: usize,
        _watchlist_code: *mut u16,
    ) -> EfiStatus,
    pub connect_controller: extern "win64" fn(
        _controller_handle: EfiHandle,
        _driver_binding_handle: *mut EfiHandle,
        _remaining_device_path: *mut EfiDevicePathProtocol,
        _recursive: bool,
    ) -> EfiStatus,
    pub disconnect_controller: extern "win64" fn(
        _controller_handle: EfiHandle,
        _driver_binding_handle: EfiHandle,
        _child_handle: EfiHandle,
    ) -> EfiStatus,
    pub open_protocol: extern "win64" fn(
        _handle: EfiHandle,
        _protocol: &EfiGuid,
        _interface: &mut *mut core::ffi::c_void,
        _agent_handle: EfiHandle,
        _controller_handle: EfiHandle,
        _attributes: u32,
    ) -> EfiStatus,
    pub close_protocol: extern "win64" fn(
        _handle: EfiHandle,
        _protocol: &EfiGuid,
        _agent_handle: EfiHandle,
        _controller_handle: EfiHandle,
    ) -> EfiStatus,
    pub open_protocol_information: extern "win64" fn(
        _handle: EfiHandle,
        _protocol: &EfiGuid,
        _entry_buffer: &mut *mut EfiOpenProtocolInformationEntry,
        _entry_count: &mut usize,
    ) -> EfiStatus,
    pub protocols_per_handle: extern "win64" fn(
        _handle: EfiHandle,
        _protocol_buffer: &mut *mut EfiGuid,
        _protocol_buffer_count: &mut usize,
    ) -> EfiStatus,
    pub locate_handle_buffer: extern "win64" fn(
        _search_type: EfiLocateSearchType,
        _protocol: &EfiGuid,
        _search_key: *mut core::ffi::c_void,
        _no_handles: &mut usize,
        _buffer: &mut *mut EfiHandle,
    ) -> EfiStatus,
    pub locate_protocol: extern "win64" fn(
        _protocol: &EfiGuid,
        _registration: *mut core::ffi::c_void,
        _interface: &mut *mut core::ffi::c_void,
    ) -> EfiStatus,
    pub install_multiple_protocol_interfaces: extern "win64" fn(
        _handle: &mut EfiHandle,
        // ...可変引数
    ) -> EfiStatus,
    pub uninstall_multiple_protocol_interfaces: extern "win64" fn(
        _handle: EfiHandle,
        // ...可変引数
    ) -> EfiStatus,
    pub calculate_crc32: extern "win64" fn(
        _data: *mut core::ffi::c_void,
        _data_size: usize,
        _crc32: &mut u32,
    ) -> EfiStatus,
    pub copy_mem: extern "win64" fn(
        _destination: *mut core::ffi::c_void,
        _source: *mut core::ffi::c_void,
        _length: usize,
    ) -> EfiStatus,
    pub set_mem: extern "win64" fn(
        _buffer: *mut core::ffi::c_void,
        _size: usize,
        _value: u8,
    ) -> EfiStatus,
    pub create_event_ex: extern "win64" fn(
        _type: u32,
        _notify_tpl: EfiTpl,
        _notify_function: EfiEventNotify,
        _notify_context: *mut core::ffi::c_void,
        _event_group: &EfiGuid,
        _event: &mut EfiEvent,
    ) -> EfiStatus,
}

// EFI_SYSTEM_TABLE 構造体 (一部のみ定義)
#[repr(C)]
pub struct EfiSystemTable {
    pub hdr: EfiTableHeader,
    pub firmware_vendor: *mut u16,
    pub firmware_revision: u32,
    pub console_in_handle: EfiHandle,
    pub con_in: *mut EfiSimpleTextInputProtocol,
    pub console_out_handle: EfiHandle,
    pub con_out: *mut EfiSimpleTextOutputProtocol,
    pub standard_error_handle: EfiHandle,
    pub std_err: *mut EfiSimpleTextOutputProtocol,
    pub runtime_services: *mut EfiRuntimeServices,
    pub boot_services: *mut EfiBootServices,
    pub number_of_table_entries: usize,
    pub configuration_table: *mut EfiConfigurationTable,
}

// EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL 構造体 (一部のみ定義)
#[repr(C)]
pub struct EfiSimpleTextOutputProtocol {
    pub reset: extern "win64" fn(
        _this: *mut EfiSimpleTextOutputProtocol,
        _extended_verification: bool,
    ) -> EfiStatus,
    pub output_string: extern "win64" fn(
        _this: *mut EfiSimpleTextOutputProtocol,
        _string: *mut u16,
    ) -> EfiStatus,
    pub test_string: extern "win64" fn(
        _this: *mut EfiSimpleTextOutputProtocol,
        _string: *mut u16,
    ) -> EfiStatus,
    pub query_mode: extern "win64" fn(
        _this: *mut EfiSimpleTextOutputProtocol,
        _mode_number: usize,
        _columns: &mut usize,
        _rows: &mut usize,
    ) -> EfiStatus,
    pub set_mode: extern "win64" fn(
        _this: *mut EfiSimpleTextOutputProtocol,
        _mode_number: usize,
    ) -> EfiStatus,
    pub set_attribute: extern "win64" fn(
        _this: *mut EfiSimpleTextOutputProtocol,
        _attribute: usize,
    ) -> EfiStatus,
    pub clear_screen: extern "win64" fn(
        _this: *mut EfiSimpleTextOutputProtocol,
    ) -> EfiStatus,
    pub set_cursor_position: extern "win64" fn(
        _this: *mut EfiSimpleTextOutputProtocol,
        _column: usize,
        _row: usize,
    ) -> EfiStatus,
    pub enable_cursor: extern "win64" fn(
        _this: *mut EfiSimpleTextOutputProtocol,
        _visible: bool,
    ) -> EfiStatus,
    pub mode: *mut EfiSimpleTextOutputMode,
}

// EFI_SIMPLE_TEXT_INPUT_PROTOCOL 構造体 (一部のみ定義)
#[repr(C)]
pub struct EfiSimpleTextInputProtocol {
    pub reset: extern "win64" fn(
        _this: *mut EfiSimpleTextInputProtocol,
        _extended_verification: bool,
    ) -> EfiStatus,
    pub read_key_stroke: extern "win64" fn(
        _this: *mut EfiSimpleTextInputProtocol,
        _key: &mut EfiInputKey,
    ) -> EfiStatus,
    pub wait_for_key: EfiEvent,
}

// EFI_RUNTIME_SERVICES 構造体 (一部のみ定義)
#[repr(C)]
pub struct EfiRuntimeServices {
    pub hdr: EfiTableHeader,
    // ...
}

// EFI_CONFIGURATION_TABLE 構造体
#[repr(C)]
pub struct EfiConfigurationTable {
    pub vendor_guid: EfiGuid,
    pub vendor_table: *mut core::ffi::c_void,
}

// EFI_MEMORY_DESCRIPTOR 構造体
#[repr(C)]
pub struct EfiMemoryDescriptor {
    pub _type: u32,
    pub physical_start: EfiPhysicalAddress,
    pub virtual_start: EfiVirtualAddress,
    pub number_of_pages: u64,
    pub attribute: u64,
}

// EFI_SIMPLE_TEXT_OUTPUT_MODE 構造体
#[repr(C)]
pub struct EfiSimpleTextOutputMode {
    pub max_mode: i32,
    pub current_mode: i32,
    pub attribute: u32,
    pub cursor_column: i32,
    pub cursor_row: i32,
    pub cursor_visible: bool,
}

// EFI_INPUT_KEY 構造体
#[repr(C)]
pub struct EfiInputKey {
    pub scan_code: u16,
    pub unicode_char: u16,
}

// その他の列挙型や定数
#[repr(usize)]
pub enum EfiAllocateType {
    AnyPages,
    MaxAddress,
    Address,
    MaxAllocateType,
}

#[repr(usize)]
pub enum EfiMemoryType {
    ReservedMemoryType,
    LoaderCode,
    LoaderData,
    BootServicesCode,
    BootServicesData,
    RuntimeServicesCode,
    RuntimeServicesData,
    ConventionalMemory,
    UnusableMemory,
    ACPIReclaimMemory,
    ACPIMemoryNVS,
    MemoryMappedIO,
    MemoryMappedIOPortSpace,
    PalCode,
    PersistentMemory,
    MaxMemoryType,
}

#[repr(usize)]
pub enum EfiTimerDelay {
    Cancel,
    Periodic,
    Relative,
}

#[repr(usize)]
pub enum EfiInterfaceType {
    NativeInterface,
    ControllerInterface,
    BusInterface,
    MaxInterfaceType,
}

#[repr(usize)]
pub enum EfiLocateSearchType {
    AllHandles,
    ByRegisterNotify,
    ByProtocol,
    MaxSearchType,
}

pub type EfiEventNotify = extern "win64" fn(event: EfiEvent, context: *mut core::ffi::c_void);

#[repr(C)]
pub struct EfiDevicePathProtocol {
    pub _type: u8,
    pub sub_type: u8,
    pub length: u16,
}

#[repr(C)]
pub struct EfiOpenProtocolInformationEntry {
    pub agent_handle: EfiHandle,
    pub controller_handle: EfiHandle,
    pub attributes: u32,
    pub open_count: u32,
}

// グローバルなシステムテーブルへのポインタ
static mut SYSTEM_TABLE: *mut EfiSystemTable = core::ptr::null_mut();

// UEFI エントリポイント
#[no_mangle]
pub extern "win64" fn efi_main(image_handle: EfiHandle, system_table: *mut EfiSystemTable) -> EfiStatus {
    unsafe {
        SYSTEM_TABLE = system_table;
    }

    // コンソール出力プロトコルを取得
    let con_out = unsafe { (*system_table).con_out };

    // 画面をクリア
    unsafe {
        ((*con_out).clear_screen)(con_out);
    }

    // 文字列を出力
    let s = "Hello from UEFI (self-implemented)!\0";
    let mut buf: [u16; 100] = [0; 100];
    for (i, c) in s.encode_utf16().enumerate() {
        buf[i] = c;
    }
    unsafe {
        ((*con_out).output_string)(con_out, buf.as_mut_ptr());
    }

    // ブートサービスを呼び出す例: Stall (5秒待機)
    let boot_services = unsafe { (*system_table).boot_services };
    unsafe {
        ((*boot_services).stall)(boot_services, 5_000_000); // 5秒 (5,000,000マイクロ秒)
    }

    // 0を返して正常終了
    0
}

// グローバルなシステムテーブルへのアクセス関数
pub fn system_table() -> &'static mut EfiSystemTable {
    unsafe {
        assert!(!SYSTEM_TABLE.is_null());
        &mut *SYSTEM_TABLE
    }
}

// コンソール出力用のラッパー
pub struct UefiWriter;

impl Write for UefiWriter {
        fn write_str(&mut self, s: &str) -> fmt::Result {
        let con_out = unsafe { (*system_table()).con_out };
        let mut buf: [u16; 256] = [0; 256]; // Buffer for UTF-16 string
        let mut i = 0;
        for c in s.encode_utf16() {
            buf[i] = c;
            i += 1;
            if i == 255 {
                buf[i] = 0; // Null terminator
                unsafe {
                    ((*con_out).output_string)(con_out, buf.as_mut_ptr());
                }
                i = 0;
            }
        }
        if i > 0 {
            buf[i] = 0; // Null terminator for the remainder
            unsafe {
                ((*con_out).output_string)(con_out, buf.as_mut_ptr());
            }
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::uefi::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    UefiWriter.write_fmt(args).unwrap();
}
