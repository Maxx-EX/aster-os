/* Aster Boot — 链接脚本
 * QEMU virt 的 RAM 起始于 0x40000000，QEMU 的 -kernel 会把 ELF 载入到这里。
 * 我们把自己放在 0x40080000，避开 QEMU 放在 0x40000000 的少量保留区。
 */
ENTRY(_start)

/* 加载基址：QEMU virt 为 0x40080000；树莓派构建时由 Makefile 用 sed 替换 */
SECTIONS
{
    . = 0x40080000;

    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    .rodata : {
        *(.rodata .rodata.*)
    }

    /* 内核镜像以二进制块的形式内嵌在引导器里。
       校验系统在运行时对这个区间做 HMAC-SHA256。 */
    . = ALIGN(4096);
    .payload : {
        __payload_start = .;
        KEEP(*(.payload))
        __payload_end = .;
    }

    . = ALIGN(16);
    .data : {
        *(.data .data.*)
    }

    . = ALIGN(16);
    __bss_start = .;
    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    }
    . = ALIGN(16);
    __bss_end = .;

    . = ALIGN(4096);
    . = . + 0x10000;          /* 64 KiB 引导栈 */
    __stack_top = .;

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}
