//! Utility functions, macros, and operation helpers

/// Helper struct for sequenced initialization
pub struct InitSequence<'a> {
    steps: &'a [(&'static str, fn() -> Result<(), &'static str>)],
}

impl<'a> InitSequence<'a> {
    pub fn new(steps: &'a [(&'static str, fn() -> Result<(), &'static str>)]) -> Self {
        Self { steps }
    }

    pub fn run(&self) {
        for (name, init_fn) in self.steps {
            init_log!("About to init {}", name);
            if let Err(e) = init_fn() {
                init_log!("Init {} failed: {}", name, e);
                panic!("{}", e);
            }
            init_log!("{} init done", name);
        }
    }
}
