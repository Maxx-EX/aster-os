ENTRY(_start)

SECTIONS
{
    /* 内核基址：QEMU virt 为 0x40200000；树莓派构建时由 Makefile 替换成 0x200000 */
    . = 0x40200000;

    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    . = ALIGN(8);
    .rodata : {
        *(.rodata .rodata.*)
        *(.eh_frame*)
    }

    . = ALIGN(4096);
    .data : {
        *(.data .data.*)
        *(.got)
        *(.got.plt)
    }

    . = ALIGN(4096);
    __bss_start = .;
    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    }
    . = ALIGN(4096);
    __bss_end = .;

    . = ALIGN(4096);
    . = . + 0x40000;        /* 256 KiB 内核栈 */
    __stack_top = .;

    /DISCARD/ : {
        *(.comment)
        *(.note*)
    }
}
