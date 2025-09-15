OUTPUT_FORMAT("efi-app-x86_64")
ENTRY(efi_main)

SECTIONS {
    .text : {
        *(.text .text.*)
    }
    .rodata : {
        *(.rodata .rodata.*)
    }
    .data : {
        *(.data .data.*)
    }
    .bss : {
        *(.bss .bss.*)
    }
    /DISCARD/ : {
        *(.eh_frame)
        *(.gcc_except_table)
    }
}
