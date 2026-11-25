ENTRY(_start)

SECTIONS {
    /* Load above identity page tables (0x1000..0x4000) and initial stack (..0x200000). */
    . = 0x200000;

    .text : ALIGN(4K) {
        *(.text._start)
        *(.text .text.*)
    }

    .rodata : ALIGN(4K) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4K) {
        *(.data .data.*)
    }

    .bss : ALIGN(4K) {
        *(.bss .bss.*)
        *(COMMON)
    }

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note*)
    }
}
