use super::ports::RegisterConfig;

// VGA Sequencer registers configuration for mode 13h (320x200, 256 colors)
// These control the timing and memory access for the VGA sequencer
pub const SEQUENCER_CONFIG: &[RegisterConfig] = &[
    RegisterConfig {
        index: 0x00,
        value: 0x03,
    }, // Reset register - synchronous reset
    RegisterConfig {
        index: 0x01,
        value: 0x01,
    }, // Clocking mode register - 8/9 dot clocks, use 9th bit in text mode (not applicable in graphics mode)
    RegisterConfig {
        index: 0x02,
        value: 0x0F,
    }, // Map mask register - enable all planes (15)
    RegisterConfig {
        index: 0x03,
        value: 0x00,
    }, // Character map select register - select character sets (not used in graphics mode)
    RegisterConfig {
        index: 0x04,
        value: 0x0E,
    }, // Memory mode register - enable ext memory, enable odd/even in chain 4, use chain 4 addressing mode
];

// VGA CRTC (Cathode Ray Tube Controller) registers configuration for mode 13h
// These control horizontal/vertical synchronization, display size, and timing
pub const CRTC_CONFIG: &[RegisterConfig] = &[
    RegisterConfig {
        index: 0x00,
        value: 0x5F,
    }, // Horizontal total (horizontal display end)
    RegisterConfig {
        index: 0x01,
        value: 0x4F,
    }, // Horizontal display enable end
    RegisterConfig {
        index: 0x02,
        value: 0x50,
    }, // Start horizontal blanking
    RegisterConfig {
        index: 0x03,
        value: 0x82,
    }, // End horizontal blanking
    RegisterConfig {
        index: 0x04,
        value: 0x54,
    }, // Start horizontal retrace pulse
    RegisterConfig {
        index: 0x05,
        value: 0x80,
    }, // End horizontal retrace
    RegisterConfig {
        index: 0x06,
        value: 0xBF,
    }, // Vertical total (vertical display end)
    RegisterConfig {
        index: 0x07,
        value: 0x1F,
    }, // Overflow
    RegisterConfig {
        index: 0x08,
        value: 0x00,
    }, // Preset row scan
    RegisterConfig {
        index: 0x09,
        value: 0x41,
    }, // Maximum scan line
    RegisterConfig {
        index: 0x10,
        value: 0x9C,
    }, // Start vertical retrace
    RegisterConfig {
        index: 0x11,
        value: 0x8E,
    }, // End vertical retrace
    RegisterConfig {
        index: 0x12,
        value: 0x8F,
    }, // Vertical display enable end
    RegisterConfig {
        index: 0x13,
        value: 0x28,
    }, // Offset (line offset/logical width - number of bytes per scan line)
    RegisterConfig {
        index: 0x14,
        value: 0x40,
    }, // Underline location
    RegisterConfig {
        index: 0x15,
        value: 0x96,
    }, // Start vertical blanking
    RegisterConfig {
        index: 0x16,
        value: 0xB9,
    }, // End vertical blanking
    RegisterConfig {
        index: 0x17,
        value: 0xA3,
    }, // CRTC mode control
];

// VGA Graphics Controller registers configuration for mode 13h
// These control how graphics memory is mapped and accessed
pub const GRAPHICS_CONFIG: &[RegisterConfig] = &[
    RegisterConfig {
        index: 0x00,
        value: 0x00,
    }, // Set/reset register - reset all bits
    RegisterConfig {
        index: 0x01,
        value: 0x00,
    }, // Enable set/reset register - disable
    RegisterConfig {
        index: 0x02,
        value: 0x00,
    }, // Color compare register - compare mode
    RegisterConfig {
        index: 0x03,
        value: 0x00,
    }, // Data rotate register - no rotate
    RegisterConfig {
        index: 0x04,
        value: 0x00,
    }, // Read plane select register - select plane 0
    RegisterConfig {
        index: 0x05,
        value: 0x40,
    }, // Graphics mode register - chain odd/even planes, read mode 0, write mode 0, read plane 0
    RegisterConfig {
        index: 0x06,
        value: 0x05,
    }, // Miscellaneous register - memory map mode A0000-AFFFF (64KB), alphanumerics/text mode disabled, chain odd/even planes disabled
    RegisterConfig {
        index: 0x07,
        value: 0x0F,
    }, // Color don't care register - care about all bits
    RegisterConfig {
        index: 0x08,
        value: 0xFF,
    }, // Bit mask register - enable all bits
];

// VGA Attribute Controller registers configuration for mode 13h
// These control color mapping and screen display attributes
pub const ATTRIBUTE_CONFIG: &[RegisterConfig] = &[
    RegisterConfig {
        index: 0x00,
        value: 0x00,
    }, // Palette register 0 (red|green|blue|intensity)
    RegisterConfig {
        index: 0x01,
        value: 0x00,
    }, // Palette register 1
    RegisterConfig {
        index: 0x02,
        value: 0x0F,
    }, // Palette register 2
    RegisterConfig {
        index: 0x03,
        value: 0x00,
    }, // Palette register 3
    RegisterConfig {
        index: 0x04,
        value: 0x00,
    }, // Palette register 4
    RegisterConfig {
        index: 0x05,
        value: 0x00,
    }, // Palette register 5
    RegisterConfig {
        index: 0x06,
        value: 0x00,
    }, // Palette register 6
    RegisterConfig {
        index: 0x07,
        value: 0x00,
    }, // Palette register 7
    RegisterConfig {
        index: 0x08,
        value: 0x00,
    }, // Palette register 8
    RegisterConfig {
        index: 0x09,
        value: 0x00,
    }, // Palette register 9
    RegisterConfig {
        index: 0x0A,
        value: 0x00,
    }, // Palette register A
    RegisterConfig {
        index: 0x0B,
        value: 0x00,
    }, // Palette register B
    RegisterConfig {
        index: 0x0C,
        value: 0x00,
    }, // Palette register C
    RegisterConfig {
        index: 0x0D,
        value: 0x00,
    }, // Palette register D
    RegisterConfig {
        index: 0x0E,
        value: 0x00,
    }, // Palette register E
    RegisterConfig {
        index: 0x0F,
        value: 0x00,
    }, // Palette register F
    RegisterConfig {
        index: 0x10,
        value: 0x41,
    }, // Attr mode control register - enable 256-color mode, enable graphics mode
    RegisterConfig {
        index: 0x11,
        value: 0x00,
    }, // Overscan color register - border color (black)
    RegisterConfig {
        index: 0x12,
        value: 0x0F,
    }, // Color plane enable register - enable all planes
    RegisterConfig {
        index: 0x13,
        value: 0x00,
    }, // Horizontal pixel panning register - no panning
    RegisterConfig {
        index: 0x14,
        value: 0x00,
    }, // Color select register
];
