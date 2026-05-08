//! Shell and process management macros for Fullerene OS

#[macro_export]
macro_rules! define_commands {
    ($entry_ty:ident, $(($name:expr, $desc:expr, $func:expr)),* $(,)?) => {
        &[
            $(
                $entry_ty {
                    name: $name,
                    description: $desc,
                    function: $func,
                }
            ),*
        ]
    };
}

#[macro_export]
macro_rules! define_shell_commands {
    ($entry_ty:ident, $(($name:expr, $desc:expr, $func:expr)),* $(,)?) => {
        &[
            $(
                $entry_ty {
                    name: $name,
                    description: $desc,
                    function: $func,
                }
            ),*
        ]
    };
}

#[macro_export]
macro_rules! shell_response {
    ($($arg:tt)*) => {{
        petroleum::print!($($arg)*);
    }};
}

#[macro_export]
macro_rules! find_process_by_id {
    ($process_list:expr, $pid:expr, $action:block) => {{
        if let Some(process) = $process_list.iter_mut().find(|p| p.id == $pid) {
            $action
        }
    }};
}

#[macro_export]
macro_rules! with_process_list {
    ($list:expr, $action:block) => {{
        let mut process_list = $list.lock();
        $action
    }};
}

#[macro_export]
macro_rules! simple_command_fn {
    ($fn_name:ident, $message:literal) => {
        fn $fn_name(_args: &[&str]) -> i32 {
            petroleum::print!($message);
            0
        }
    };
    ($fn_name:ident, $message:literal, $exit_code:expr) => {
        fn $fn_name(_args: &[&str]) -> i32 {
            petroleum::print!($message);
            $exit_code
        }
    };
}
