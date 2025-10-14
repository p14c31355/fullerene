//! Utility functions, macros, and operation helpers

// Buffer operations trait
pub trait BufferOps<T> {
    fn write_at(&mut self, index: usize, value: T);
    fn read_at(&self, index: usize) -> Option<&T>;
    fn clear_with(&mut self, value: T) where T: Clone;
}

impl<T: Clone> BufferOps<T> for [T] {
    fn write_at(&mut self, index: usize, value: T) {
        if index < self.len() {
            self[index] = value;
        }
    }

    fn read_at(&self, index: usize) -> Option<&T> {
        self.get(index)
    }

    fn clear_with(&mut self, value: T) {
        self.fill(value);
    }
}

pub mod port_operations {
    use x86_64::instructions::port::{Port, PortWrite};

    pub trait PortWriter<T> {
        fn write_sequence(&mut self, values: &[T]);
    }

    impl<T: Copy + PortWrite> PortWriter<T> for Port<T> {
        fn write_sequence(&mut self, values: &[T]) {
            for &value in values {
                unsafe { self.write(value) };
            }
        }
    }
}

#[macro_export]
macro_rules! resource_wrapper {
    ($name:ident, $inner:ty, $($field:ident: $ftype:ty),*) => {
        pub struct $name {
            inner: $inner,
            $($field: $ftype,)*
            initialized: bool,
        }

        impl $name {
            pub fn new(inner: $inner) -> Self {
                Self {
                    inner,
                    $($field: Default::default(),)*
                    initialized: false,
                }
            }

            pub fn init(&mut self) -> crate::types::SystemResult<()> {
                self.initialized = true;
                Ok(())
            }

            pub fn is_initialized(&self) -> bool {
                self.initialized
            }
        }

        impl core::ops::Deref for $name {
            type Target = $inner;
            fn deref(&self) -> &Self::Target {
                &self.inner
            }
        }

        impl core::ops::DerefMut for $name {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.inner
            }
        }
    };
}

#[macro_export]
macro_rules! delegate_operation {
    ($self:expr, $method:ident, $($arg:expr),*) => {
        match $self {
            _ => $self.$method($($arg),*),
        }
    };
    ($self:expr, $method:ident) => {
        match $self {
            _ => $self.$method(),
        }
    };
}

#[macro_export]
macro_rules! buffered_write {
    ($buffer:expr, $value:expr) => {
        $buffer.write($value);
    };
    ($buffer:expr, $($values:expr),*) => {
        $($buffer.write($values);)*
    };
}

#[macro_export]
macro_rules! generic_memory_operation {
    ($self:expr, $operation:expr) => {
        if !$self.is_initialized() {
            return Err(crate::errors::SystemError::InternalError);
        }
        $operation
    };
}
