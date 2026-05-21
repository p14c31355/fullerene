//! Prompt configuration for Nozzle shell
//!
//! Handles the prompt string displayed before each input line.

use alloc::string::String;

/// Shell prompt
#[derive(Clone)]
pub struct Prompt {
    text: String,
}

impl Prompt {
    /// Create a new prompt with the given text.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// Return the prompt string.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Set the prompt text.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }
}