ENTRY(_start)

SECTIONS
{
    /* 用户代码段固定落在 0x44400000：内核给这一页开了 EL0 可执行权限 */
    . = 0x44400000;

    .text : {
        KEEP(*(.text.start))
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
        *(COMMON)
    }

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}
