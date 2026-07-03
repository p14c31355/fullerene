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
}


