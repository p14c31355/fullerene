use super::ports::RegisterConfig;

macro_rules! reg {
    ($index:expr, $value:expr) => {
        RegisterConfig { index: $index, value: $value }
    };
}

macro_rules! config {
    ( $( ($index:expr, $value:expr) ),* $(,)? ) => {
        &[ $( reg!($index, $value) , )* ]
    };
}

// VGA Sequencer registers configuration for mode 13h (320x200, 256 colors)
// These control the timing and memory access for the VGA sequencer
pub const SEQUENCER_CONFIG: &[RegisterConfig] = config!(
    (0x00, 0x03), // Reset register - synchronous reset
    (0x01, 0x01), // Clocking mode register - 8/9 dot clocks, use 9th bit in text mode (not applicable in graphics mode)
    (0x02, 0x0F), // Map mask register - enable all planes (15)
    (0x03, 0x00), // Character map select register - select character sets (not used in graphics mode)
    (0x04, 0x0E), // Memory mode register - enable ext memory, enable odd/even in chain 4, use chain 4 addressing mode
);

// VGA CRTC (Cathode Ray Tube Controller) registers configuration for mode 13h
// These control horizontal/vertical synchronization, display size, and timing
pub const CRTC_CONFIG: &[RegisterConfig] = config!(
    (0x00, 0x5F), // Horizontal total (horizontal display end)
    (0x01, 0x4F), // Horizontal display enable end
    (0x02, 0x50), // Start horizontal blanking
    (0x03, 0x82), // End horizontal blanking
    (0x04, 0x54), // Start horizontal retrace pulse
    (0x05, 0x80), // End horizontal retrace
    (0x06, 0xBF), // Vertical total (vertical display end)
    (0x07, 0x1F), // Overflow
    (0x08, 0x00), // Preset row scan
    (0x09, 0x41), // Maximum scan line
    (0x10, 0x9C), // Start vertical retrace
    (0x11, 0x8E), // End vertical retrace
    (0x12, 0x8F), // Vertical display enable end
    (0x13, 0x28), // Offset (line offset/logical width - number of bytes per scan line)
    (0x14, 0x40), // Underline location
    (0x15, 0x96), // Start vertical blanking
    (0x16, 0xB9), // End vertical blanking
    (0x17, 0xA3), // CRTC mode control
);

// VGA Graphics Controller registers configuration for mode 13h
// These control how graphics memory is mapped and accessed
pub const GRAPHICS_CONFIG: &[RegisterConfig] = config!(
    (0x00, 0x00), // Set/reset register - reset all bits
    (0x01, 0x00), // Enable set/reset register - disable
    (0x02, 0x00), // Color compare register - compare mode
    (0x03, 0x00), // Data rotate register - no rotate
    (0x04, 0x00), // Read plane select register - select plane 0
    (0x05, 0x40), // Graphics mode register - chain odd/even planes, read mode 0, write mode 0, read plane 0
    (0x06, 0x05), // Miscellaneous register - memory map mode A0000-AFFFF (64KB), alphanumerics/text mode disabled, chain odd/even planes disabled
    (0x07, 0x0F), // Color don't care register - care about all bits
    (0x08, 0xFF), // Bit mask register - enable all bits
);

// VGA Attribute Controller registers configuration for mode 13h
// These control color mapping and screen display attributes
pub const ATTRIBUTE_CONFIG: &[RegisterConfig] = config!(
    (0x00, 0x00), // Palette register 0 (red|green|blue|intensity)
    (0x01, 0x00), // Palette register 1
    (0x02, 0x0F), // Palette register 2
    (0x03, 0x00), // Palette register 3
    (0x04, 0x00), // Palette register 4
    (0x05, 0x00), // Palette register 5
    (0x06, 0x00), // Palette register 6
    (0x07, 0x00), // Palette register 7
    (0x08, 0x00), // Palette register 8
    (0x09, 0x00), // Palette register 9
    (0x0A, 0x00), // Palette register A
    (0x0B, 0x00), // Palette register B
    (0x0C, 0x00), // Palette register C
    (0x0D, 0x00), // Palette register D
    (0x0E, 0x00), // Palette register E
    (0x0F, 0x00), // Palette register F
    (0x10, 0x41), // Attr mode control register - enable 256-color mode, enable graphics mode
    (0x11, 0x00), // Overscan color register - border color (black)
    (0x12, 0x0F), // Color plane enable register - enable all planes
    (0x13, 0x00), // Horizontal pixel panning register - no panning
    (0x14, 0x00), // Color select register
);

// VGA Sequencer registers configuration for text mode (80x25)
pub const SEQUENCER_TEXT_CONFIG: &[RegisterConfig] = config!(
    (0x00, 0x03), // Reset register
    (0x01, 0x00), // Clocking mode register
    (0x02, 0x03), // Map mask register
    (0x03, 0x00), // Character map select register
    (0x04, 0x02), // Memory mode register
);

// VGA CRTC registers configuration for text mode (80x25)
pub const CRTC_TEXT_CONFIG: &[RegisterConfig] = config!(
    (0x00, 0x5F), // Horizontal total
    (0x01, 0x4F), // Horizontal display enable end
    (0x02, 0x50), // Start horizontal blanking
    (0x03, 0x82), // End horizontal blanking
    (0x04, 0x55), // Start horizontal retrace pulse
    (0x05, 0x81), // End horizontal retrace
    (0x06, 0xBF), // Vertical total
    (0x07, 0x1F), // Overflow
    (0x08, 0x00), // Preset row scan
    (0x09, 0x4F), // Maximum scan line
    (0x10, 0x9C), // Start vertical retrace
    (0x11, 0x8E), // End vertical retrace
    (0x12, 0x8F), // Vertical display enable end
    (0x13, 0x28), // Offset
    (0x14, 0x1F), // Underline location
    (0x15, 0x96), // Start vertical blanking
    (0x16, 0xB9), // End vertical blanking
    (0x17, 0xA3), // CRTC mode control
);

// VGA Graphics Controller registers configuration for text mode
pub const GRAPHICS_TEXT_CONFIG: &[RegisterConfig] = config!(
    (0x05, 0x10), // Graphics mode register
    (0x06, 0x0E), // Miscellaneous register
);

// VGA Attribute Controller registers configuration for text mode
pub const ATTRIBUTE_TEXT_CONFIG: &[RegisterConfig] = config!(
    (0x00, 0x00), // Palette register 0
    (0x01, 0x01), // Palette register 1
    (0x02, 0x02), // Palette register 2
    (0x03, 0x03), // Palette register 3
    (0x04, 0x04), // Palette register 4
    (0x05, 0x05), // Palette register 5
    (0x06, 0x06), // Palette register 6
    (0x07, 0x07), // Palette register 7
    (0x08, 0x08), // Palette register 8
    (0x09, 0x09), // Palette register 9
    (0x0A, 0x0A), // Palette register A
    (0x0B, 0x0B), // Palette register B
    (0x0C, 0x0C), // Palette register C
    (0x0D, 0x0D), // Palette register D
    (0x0E, 0x0E), // Palette register E
    (0x0F, 0x0F), // Palette register F
    (0x10, 0x0C), // Attr mode control register - text mode, blinking enabled
    (0x11, 0x00), // Overscan color register
    (0x12, 0x0F), // Color plane enable register
    (0x13, 0x00), // Horizontal pixel panning register
    (0x14, 0x00), // Color select register
);
