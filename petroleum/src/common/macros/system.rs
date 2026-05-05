//! System initialization and general utility macros for Fullerene OS

#[macro_export]
macro_rules! save_sysv64_registers {
    () => {
        "push rdi
         push rsi
         push rdx
         push rcx
         push r8
         push r9
         push r10
         push r11"
    };
}

#[macro_export]
macro_rules! bitmap_chunk_bit {
    ($frame:expr) => {{
        let chunk_index = $frame / 64;
        let bit_index = $frame % 64;
        (chunk_index, bit_index)
    }};
}

#[macro_export]
macro_rules! bit_ops {
    (set_field, $field:expr, $mask:expr, $shift:expr, $value:expr) => {
        $field = ($field & !($mask << $shift)) | (($value as u32 & $mask) << $shift);
    };
    (set_bool_bit, $field:expr, $bit:expr, $value:expr) => {
        if $value {
            $field |= 1 << $bit;
        } else {
            $field &= !(1 << $bit);
        }
    };
    (bitmap_set_free, $bitmap:expr, $frame:expr) => {
        if let Some(ref mut bitmap) = $bitmap {
            let (chunk_index, bit_index) = bitmap_chunk_bit!($frame);
            if chunk_index < bitmap.len() {
                bitmap[chunk_index] &= !(1 << bit_index);
            }
        }
    };
    (bitmap_set_used, $bitmap:expr, $frame:expr) => {
        if let Some(ref mut bitmap) = $bitmap {
            let (chunk_index, bit_index) = bitmap_chunk_bit!($frame);
            if chunk_index < bitmap.len() {
                bitmap[chunk_index] |= 1 << bit_index;
            }
        }
    };
    (bitmap_is_free, $bitmap:expr, $frame:expr) => {
        if let Some(ref bitmap) = $bitmap {
            let (chunk_index, bit_index) = bitmap_chunk_bit!($frame);
            if chunk_index < bitmap.len() {
                (bitmap[chunk_index] & (1 << bit_index)) == 0
            } else {
                false
            }
        } else {
            false
        }
    };
}

#[macro_export]
macro_rules! command_args {
    () => {
        &[]
    };
    ($($arg:expr),* $(,)?) => {
        &[$($arg.to_string()),*]
    };
}

#[macro_export]
macro_rules! ensure_initialized {
    ($self:expr) => {
        if !$self.initialized {
            return Err($crate::common::logging::SystemError::InternalError);
        }
    };
}

#[macro_export]
macro_rules! calc_offset_addr {
    ($base:expr, $i:expr) => {
        $base + ($i * 4096)
    };
}

#[macro_export]
macro_rules! lock_and_modify {
    ($lock:expr, $var:ident, $code:block) => {{
        let mut $var = $lock.lock();
        $code
    }};
}

#[macro_export]
macro_rules! lock_and_read {
    ($lock:expr, $var:ident, $val:expr) => {{
        let $var = $lock.lock();
        $val
    }};
}

#[macro_export]
macro_rules! init_component {
    ($component:expr, $name:expr) => {{
        match $component.init() {
            Ok(()) => {
                log::info!(concat!($name, " initialized successfully"));
                Ok(())
            }
            Err(e) => {
                log::error!("Failed to initialize {}: {:?}", $name, e);
                Err(e)
            }
        }
    }};
}

#[macro_export]
macro_rules! ensure_with_msg {
    ($condition:expr, $error:expr, $msg:expr) => {
        if !$condition {
            $crate::log_error!($error, $msg);
            return Err(*$error);
        }
    };
}

#[macro_export]
macro_rules! option_to_result {
    ($option:expr, $error:expr) => {
        match $option {
            Some(value) => Ok(value),
            None => {
                $crate::log_error!($error, "Option was None");
                Err(*$error)
            }
        }
    };
}

#[macro_export]
macro_rules! try_or_log {
    ($expr:expr, $context:expr) => {
        match $expr {
            Ok(value) => value,
            Err(e) => {
                $crate::log_error!(e, $context);
                return Err(e);
            }
        }
    };
}

#[macro_export]
macro_rules! static_str {
    ($s:expr) => {{
        const S: &str = $s;
        S
    }};
}

#[macro_export]
macro_rules! scheduler_log {
    ($msg:literal) => {
        log::info!("Scheduler: {}", $msg);
    };
    ($msg:literal, $($arg:tt)*) => {
        log::info!("Scheduler: {}", format_args!($msg, $($arg)*));
    };
}

#[macro_export]
macro_rules! init_boot_step {
    ($step_name:expr, $init_fn:expr) => {{
        $crate::println!($step_name);
        $crate::serial::_print(format_args!("{} \n", $step_name));
        $init_fn.expect(concat!("Bootloader initialization failed at: ", $step_name))
    }};
}

#[macro_export]
macro_rules! init_step {
    ($name:expr, $func:expr) => {
        (
            $name,
            $func as fn() -> Result<(), &'static str>,
        )
    };
}

#[macro_export]
macro_rules! read_unaligned {
    ($ptr:expr, $offset:expr, $ty:ty) => {
        unsafe { core::ptr::read_unaligned(($ptr as *const u8).add($offset) as *const $ty) }
    };
}

#[macro_export]
macro_rules! check_periodic {
    ($last_tick:expr, $interval:expr, $current_tick:expr, $block:block) => {{
        let mut last = $last_tick.lock();
        if $current_tick - *last >= $interval {
            *last = $current_tick;
            $block;
        }
    }};
}

#[macro_export]
macro_rules! periodic_task {
    ($current_tick:expr, $interval:expr, $block:block) => {
        if $current_tick % $interval == 0 {
            $block;
        }
    };
}

#[macro_export]
macro_rules! health_check {
    ($fn_name:ident, $threshold_expr:expr, $log_level:ident, $msg:expr, $body:block) => {
        fn $fn_name() {
            if $threshold_expr {
                log::$log_level!($msg);
                $body
            }
        }
    };
}

#[macro_export]
macro_rules! periodic_fs_log {
    ($filename:expr, $interval_ticks:expr, $current_tick:expr, $($stats_expr:tt)*) => {{
        static LAST_LOG_TICK: spin::Mutex<u64> = spin::Mutex::new(0);

        petroleum::check_periodic!(LAST_LOG_TICK, $interval_ticks, $current_tick, {
            let log_content = alloc::format!($($stats_expr)*);
            log::info!("{}", log_content);
        });
    }};
}

#[macro_export]
macro_rules! maintenance_tasks {
    ($current_tick:expr, $(($interval:expr, $fn_call:expr)),* $(,)?) => {
        $(
            petroleum::periodic_task!($current_tick, $interval, {
                $fn_call
            });
        )*
    };
}

#[macro_export]
macro_rules! impl_error_from {
    ($src:ty, $dst:ty, $map_fn:expr) => {
        impl From<$src> for $dst {
            fn from(error: $src) -> Self {
                ($map_fn)(error)
            }
        }
    };
}

#[macro_export]
macro_rules! error_variant_map {
    ($src:ty, $dst:ty, $pat:pat => $result:expr) => {
        impl From<$src> for $dst {
            fn from(error: $src) -> Self {
                match error {
                    $pat => $result,
                }
            }
        }
    };
}

#[macro_export]
macro_rules! error_chain {
    ($src:ty, $dst:ty, $( $pat:pat => $result:expr ),* $(,)?) => {
        impl From<$src> for $dst {
            fn from(error: $src) -> Self {
                match error {
                    $(
                        $pat => $result,
                    )*
                }
            }
        }
    };
}

#[macro_export]
macro_rules! check_uefi_status {
    ($status:expr, $log_msg:expr, $err:expr) => {
        if EfiStatus::from($status) != EfiStatus::Success {
            log::error!($log_msg);
            return Err($err);
        }
    };
}

#[macro_export]
macro_rules! pause {
    () => {
        unsafe {
            core::arch::asm!("pause", options(nomem, nostack, preserves_flags));
        }
    };
}

#[macro_export]
macro_rules! halt {
    () => {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    };
}

#[macro_export]
macro_rules! init_with_log {
    ($name:expr, $init_fn:expr) => {{
        debug_log!(concat!($name, " initializing"));
        match $init_fn() {
            Ok(_) => debug_log!(concat!($name, " initialized successfully")),
            Err(e) => {
                error_log!(concat!($name, " initialization failed: {:?}"), e);
                panic!("{} initialization failed", $name);
            }
        }
    }};
}

#[macro_export]
macro_rules! ensure {
    ($condition:expr, $error_ty:ty, $error_val:expr) => {
        if !$condition {
            return Err(<$error_ty>::$error_val as $error_ty);
        }
    };
    ($condition:expr, $error_ty:ty, $error_val:expr, $msg:expr) => {
        if !$condition {
            error_log!($msg);
            return Err(<$error_ty>::$error_val as $error_ty);
        }
    };
}

#[macro_export]
macro_rules! init_system_component {
    ($name:expr, $init_expr:expr) => {{
        petroleum::println!(concat!($name, " initializing..."));
        match $init_expr {
            Ok(_) => petroleum::println!(concat!($name, " initialized successfully")),
            Err(e) => {
                petroleum::println!(concat!($name, " initialization failed: {:?}"), e);
                return Err(e);
            }
        }
    }};
}

#[macro_export]
macro_rules! define_periodic_task {
    ($name:ident, $interval:expr, $task_fn:expr) => {
        fn $name(tick: u64, iter: u64) {
            $task_fn(tick, iter);
        }

        lazy_static::lazy_static! {
            static ref $name: PeriodicTask = PeriodicTask {
                interval: $interval,
                last_tick: alloc::sync::Arc::new(spin::Mutex::new(0)),
                task: $name,
            };
        }
    };
}

#[macro_export]
macro_rules! define_periodic_tasks {
    ($task_ty:ident, $(($interval:expr, $task_fn:ident, $desc:expr)),* $(,)?) => {
        lazy_static::lazy_static! {
            static ref PERIODIC_TASKS: [$task_ty; count!($($task_fn),*)] = [
                $(
                    $task_ty {
                        interval: $interval,
                        last_tick: alloc::sync::Arc::new(spin::Mutex::new(0)),
                        task: $task_fn,
                        description: $desc,
                    }
                ),*
            ];
        }
    };
}

#[macro_export]
macro_rules! count {
    () => { 0 };
    ($head:expr $(, $tail:expr)*) => { 1 + count!($($tail),*) };
}

#[macro_export]
macro_rules! define_vbox_settings {
    ($vm_name:expr, $(($args:expr, $failure_msg:expr)),* $(,)?) => {{
        $(
            let status = ::std::process::Command::new("VBoxManage")
                .arg("modifyvm")
                .arg($vm_name)
                .args($args)
                .status()?;
            if !status.success() {
                return Err(::std::io::Error::new(::std::io::ErrorKind::Other, $failure_msg));
            }
        )*
        Ok(()) as ::std::io::Result<()>
    }};
}

#[macro_export]
macro_rules! build_package {
    ($package:expr, $target:expr, [$($features:expr),* $(,)?]) => {{
        let mut args = vec!["+nightly", "build", "-q", "-Zbuild-std=core,alloc"];
        $(
            args.push($features);
        )*
        args.extend_from_slice(&["--package", $package, "--target", $target, "--profile", "dev"]);

        let status = std::process::Command::new("cargo")
            .current_dir(std::env::var("CARGO_MANIFEST_DIR").unwrap().parent().unwrap())
            .args(&args)
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other(concat!($package, " build failed")));
        }
        Ok(())
    }};
}

#[macro_export]
macro_rules! create_iso_files {
    ($(($source:expr, $dest:expr)),* $(,)?) => {
        vec![
            $(
                isobemak::IsoImageFile {
                    source: $source.clone(),
                    destination: $dest.to_string(),
                }
            ),*
        ]
    };
}

#[macro_export]
macro_rules! bootloader_expect {
    ($expr:expr, $msg:expr) => {
        match $expr {
            Ok(val) => val,
            Err(e) => {
                petroleum::println!(concat!($msg, ": {:?}"), e);
                panic!($msg);
            }
        }
    };
}