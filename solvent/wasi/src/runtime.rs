use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use wasmi::{Engine, Linker, Module, Store};

use crate::wasi::{
    WasiCtx, args_get, args_sizes_get, clock_time_get, environ_get, environ_sizes_get, fd_close,
    fd_fdstat_get, fd_filestat_get, fd_prestat_dir_name, fd_prestat_get, fd_read, fd_readdir,
    fd_seek, fd_write, fullerene_capture_screen, fullerene_capture_screen_chunk,
    fullerene_close_window, fullerene_create_window, fullerene_screen_dimensions,
    fullerene_show_error, fullerene_show_image, fullerene_show_text, fullerene_update_window,
    fullerene_wait_for_ns, fullerene_write_file_chunk, path_filestat_get, path_open, proc_exit,
    random_get, sched_yield,
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
    wait_for_ns: fn(u64),
    file_size: fn(&str) -> Result<u64, genome::FsError>,
    read_file_range: fn(&str, u64, usize) -> Result<Vec<u8>, genome::FsError>,
    read_directory: fn(&str) -> Result<Vec<(String, u8)>, genome::FsError>,
    write_file: fn(&str, &[u8]) -> Result<(), genome::FsError>,
    write_file_chunk: fn(&str, u64, &[u8], bool) -> Result<(), genome::FsError>,
    get_monotonic_ns: fn() -> u64,
    screen_dimensions: fn() -> (u32, u32),
    capture_screen: fn() -> Option<(u32, u32, Vec<u8>)>,
    capture_screen_chunk: fn(u32, &mut [u8]) -> Option<(u32, u32)>,
    show_image: fn(u32, u32, &[u8]) -> i32,
    show_text: fn(&str, &str) -> i32,
    show_error: fn(&str, &str) -> i32,
    create_window: fn(&str, u32, u32) -> i32,
    update_window: fn(i32, u32, u32, &[u8]) -> i32,
    close_window: fn(i32) -> i32,
) -> i32 {
    const INITIAL_FUEL: u64 = 100_000_000;
    // The file viewer is synchronous by design. Give it a smaller compute
    // budget so malformed media metadata cannot monopolize the shell while
    // still leaving enough room for normal image/video decoding.
    let is_viewer = args
        .first()
        .is_some_and(|path| path.ends_with("viewer.wasm"));
    let is_mp4 = args
        .iter()
        .any(|path| path.to_ascii_lowercase().ends_with(".mp4"));
    let is_mp3 = args
        .iter()
        .any(|path| path.to_ascii_lowercase().ends_with(".mp3"));
    let fuel = if is_mp4 {
        // H.264 decoding every sample is intentionally synchronous. The
        // parser still has independent time/I/O/sample-count guards, so the
        // fuel budget must cover a complete long video instead of aborting
        // partway through playback.
        500_000_000
    } else if is_mp3 {
        // MP3 metadata scanning is linear but may inspect thousands of
        // frames. Keep it finite while leaving enough room for a long track.
        100_000_000
    } else if is_viewer {
        25_000_000
    } else {
        INITIAL_FUEL
    };
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
        wait_for_ns,
        file_size,
        read_file_range,
        read_directory,
        write_file,
        write_file_chunk,
        get_monotonic_ns,
        screen_dimensions,
        capture_screen,
        capture_screen_chunk,
        show_image,
        show_text,
        show_error,
        create_window,
        update_window,
        close_window,
    );

    let mut store = Store::new(&engine, ctx);
    if let Err(error) = store.set_fuel(fuel) {
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
    wasi_func!("sched_yield", sched_yield);
    wasi_func!("clock_time_get", clock_time_get);
    wasi_func!("random_get", random_get);

    // Fullerene custom host functions (import module "fullerene")
    let fullerene = "fullerene";
    linker.func_wrap(fullerene, "screen_dimensions", fullerene_screen_dimensions)?;
    linker.func_wrap(fullerene, "capture_screen", fullerene_capture_screen)?;
    linker.func_wrap(
        fullerene,
        "capture_screen_chunk",
        fullerene_capture_screen_chunk,
    )?;
    linker.func_wrap(fullerene, "write_file_chunk", fullerene_write_file_chunk)?;
    linker.func_wrap(fullerene, "wait_for_ns", fullerene_wait_for_ns)?;
    linker.func_wrap(fullerene, "show_image", fullerene_show_image)?;
    linker.func_wrap(fullerene, "show_text", fullerene_show_text)?;
    linker.func_wrap(fullerene, "show_error", fullerene_show_error)?;
    linker.func_wrap(fullerene, "create_window", fullerene_create_window)?;
    linker.func_wrap(fullerene, "update_window", fullerene_update_window)?;
    linker.func_wrap(fullerene, "close_window", fullerene_close_window)?;

    Ok(linker)
}
