//! Common, compact observation records used by HBD reports.

use alloc::string::String;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationValue {
    Bool(bool),
    Integer(u64),
    Text(&'static str),
    OwnedText(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub key: &'static str,
    pub value: ObservationValue,
}

impl Observation {
    pub const fn boolean(key: &'static str, value: bool) -> Self {
        Self {
            key,
            value: ObservationValue::Bool(value),
        }
    }

    pub const fn integer(key: &'static str, value: u64) -> Self {
        Self {
            key,
            value: ObservationValue::Integer(value),
        }
    }

    pub const fn text(key: &'static str, value: &'static str) -> Self {
        Self {
            key,
            value: ObservationValue::Text(value),
        }
    }

    pub fn owned_text(key: &'static str, value: String) -> Self {
        Self {
            key,
            value: ObservationValue::OwnedText(value),
        }
    }
}

impl core::fmt::Display for ObservationValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Bool(value) => write!(f, "{}", value),
            Self::Integer(value) => write!(f, "{}", value),
            Self::Text(value) => f.write_str(value),
            Self::OwnedText(value) => f.write_str(value),
        }
    }
}

/// Render a compact key/value observation list for diagnostics.
pub fn format_observations(observations: &[Observation]) -> String {
    use core::fmt::Write;
    let mut out = String::new();
    for (index, observation) in observations.iter().enumerate() {
        if index != 0 {
            out.push(' ');
        }
        let _ = write!(out, "{}={}", observation.key, observation.value);
    }
    out
}
