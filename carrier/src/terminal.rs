pub trait Terminal {
    fn write_str(&mut self, s: &str);

    fn read_byte(&mut self) -> Option<u8>;

    fn input_available(&self) -> bool {
        false
    }

    fn set_stdin(&mut self, _data: alloc::string::String) {}

    fn take_stdout(&mut self) -> Option<alloc::string::String> {
        None
    }

    fn take_stdin(&mut self) -> Option<alloc::string::String> {
        None
    }

    fn arm_pipe_stdout(&mut self) {}

    fn clear_pipe_stdin(&mut self) {}

    /// Record a command in this terminal session's history.
    fn record_history(&mut self, _line: &str) {}

    /// Return this terminal session's command history, newest first.
    fn history_snapshot(&self) -> alloc::vec::Vec<alloc::string::String> {
        alloc::vec::Vec::new()
    }
}
