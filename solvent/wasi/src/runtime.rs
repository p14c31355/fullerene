use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use wasmi::{Engine, Linker, Module, Store};

use crate::wasi::{
    WasiCtx, args_get, args_sizes_get, clock_time_get, environ_get, environ_sizes_get, fd_close,
    fd_fdstat_get, fd_filestat_get, fd_prestat_dir_name, fd_prestat_get, fd_read, fd_readdir,
    fd_seek, fd_write, fullerene_capture_screen, fullerene_close_window, fullerene_create_window,
    fullerene_show_error, fullerene_show_image, fullerene_show_text, fullerene_update_window,
    path_filestat_get, path_open, proc_exit, random_get,
};

/// Run a WASI module with the given binary, arguments, and I/O callbacks.
/// Returns the exit code (0 = success).
///
/// Synchronous WASM still has a finite fuel budget so compute-only modules
/// cannot trap the kernel forever.
pub fn run(
    wasm_binary: &[u8],
    args: &[&str],
    write_stdout: fn(&[u8]),
    write_stderr: fn(&[u8]),
    read_stdin: fn() -> Option<u8>,
    yield_now: fn(),
    file_size: fn(&str) -> Result<u64, genome::FsError>,
    read_file_range: fn(&str, u64, usize) -> Result<Vec<u8>, genome::FsError>,
    read_directory: fn(&str) -> Result<Vec<(String, u8)>, genome::FsError>,
    get_monotonic_ns: fn() -> u64,
    capture_screen: fn() -> Option<(u32, u32, Vec<u8>)>,
    show_image: fn(u32, u32, &[u8]) -> i32,
    show_text: fn(&str, &str) -> i32,
    show_error: fn(&str, &str) -> i32,
    create_window: fn(&str, u32, u32) -> i32,
    update_window: fn(i32, u32, u32, &[u8]) -> i32,
    close_window: fn(i32) -> i32,
) -> i32 {
    const INITIAL_FUEL: u64 = 100_000_000;
    let mut config = wasmi::Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = match Module::new(&engine, wasm_binary) {
        Ok(m) => m,
        Err(e) => {
            let msg = format!("wasm: parse error: {}\n", e);
            write_stderr(msg.as_bytes());
            return -1;
        }
    };

    let ctx = WasiCtx::new(
        args,
        write_stdout,
        write_stderr,
        read_stdin,
        yield_now,
        file_size,
        read_file_range,
        read_directory,
        get_monotonic_ns,
        capture_screen,
        show_image,
        show_text,
        show_error,
        create_window,
        update_window,
        close_window,
    );

    let mut store = Store::new(&engine, ctx);
    if let Err(error) = store.set_fuel(INITIAL_FUEL) {
        let msg = format!("wasm: fuel setup failed: {}\n", error);
        write_stderr(msg.as_bytes());
        return -1;
    }

    let linker = match create_linker(&engine) {
        Ok(l) => l,
        Err(e) => {
            let msg = format!("wasm: linker setup failed: {}\n", e);
            write_stderr(msg.as_bytes());
            return -1;
        }
    };

    let instance = match linker.instantiate(&mut store, &module) {
        Ok(pre) => match pre.start(&mut store) {
            Ok(inst) => inst,
            Err(e) => {
                if let Some(code) = store.data().exit_code {
                    return code as i32;
                }
                let msg = format!("wasm: pre.start() failed: {}\n", e);
                write_stderr(msg.as_bytes());
                return 1;
            }
        },
        Err(e) => {
            let msg = format!("wasm: instantiation failed: {}\n", e);
            write_stderr(msg.as_bytes());
            return -1;
        }
    };

    // Try _start first (WASI command entry point)
    if let Ok(func) = instance.get_typed_func::<(), ()>(&store, "_start") {
        match func.call(&mut store, ()) {
            Ok(()) => {}
            Err(trap) => {
                if let Some(code) = store.data().exit_code {
                    return code as i32;
                }
                let msg = format!("wasm: _start trapped: {}\n", trap);
                write_stderr(msg.as_bytes());
                return 1;
            }
        }
    } else if let Ok(func) = instance.get_typed_func::<(), ()>(&store, "_initialize") {
        match func.call(&mut store, ()) {
            Ok(()) => {}
            Err(trap) => {
                if let Some(code) = store.data().exit_code {
                    return code as i32;
                }
                let msg = format!("wasm: _initialize trapped: {}\n", trap);
                write_stderr(msg.as_bytes());
                return 1;
            }
        }
    } else {
        let msg = "wasm: no _start or _initialize entry point found\n";
        write_stderr(msg.as_bytes());
        return -1;
    }

    store.data().exit_code.unwrap_or(0) as i32
}

fn create_linker(engine: &Engine) -> Result<Linker<WasiCtx>, wasmi::Error> {
    let mut linker = Linker::<WasiCtx>::new(engine);
    let module = "wasi_snapshot_preview1";

    macro_rules! wasi_func {
        ($name:expr, $func:expr) => {
            linker.func_wrap(module, $name, $func)?;
        };
    }

    wasi_func!("args_sizes_get", args_sizes_get);
    wasi_func!("args_get", args_get);
    wasi_func!("environ_sizes_get", environ_sizes_get);
    wasi_func!("environ_get", environ_get);
    wasi_func!("fd_write", fd_write);
    wasi_func!("fd_read", fd_read);
    wasi_func!("fd_close", fd_close);
    wasi_func!("fd_seek", fd_seek);
    wasi_func!("fd_fdstat_get", fd_fdstat_get);
    wasi_func!("fd_prestat_get", fd_prestat_get);
    wasi_func!("fd_prestat_dir_name", fd_prestat_dir_name);
    wasi_func!("fd_filestat_get", fd_filestat_get);
    wasi_func!("fd_readdir", fd_readdir);
    wasi_func!("path_open", path_open);
    wasi_func!("path_filestat_get", path_filestat_get);
    wasi_func!("proc_exit", proc_exit);
    wasi_func!("clock_time_get", clock_time_get);
    wasi_func!("random_get", random_get);

    // Fullerene custom host functions (import module "fullerene")
    let fullerene = "fullerene";
    linker.func_wrap(fullerene, "capture_screen", fullerene_capture_screen)?;
    linker.func_wrap(fullerene, "show_image", fullerene_show_image)?;
    linker.func_wrap(fullerene, "show_text", fullerene_show_text)?;
    linker.func_wrap(fullerene, "show_error", fullerene_show_error)?;
    linker.func_wrap(fullerene, "create_window", fullerene_create_window)?;
    linker.func_wrap(fullerene, "update_window", fullerene_update_window)?;
    linker.func_wrap(fullerene, "close_window", fullerene_close_window)?;

    Ok(linker)
}
