/* Aster — 板级差异集中在这里，换板子只改这个文件 + 编译宏 */
#ifndef ASTER_BOARD_H
#define ASTER_BOARD_H

#if defined(ASTER_BOARD_RPI4)
   /* 树莓派 4/5（Pi 5 的 PL011 也在同一个地址段），运行在 EL2 时要先降到 EL1 */
#  define ASTER_UART_BASE   0xFE201000UL
#  define ASTER_RAM_BASE    0x00000000UL
#  define ASTER_LOAD_ADDR   0x00080000UL
#  define ASTER_KERNEL_ADDR 0x00200000UL
#  define ASTER_BOARD_NAME  "Raspberry Pi 4/5"
#else
   /* QEMU virt（默认） */
#  define ASTER_UART_BASE   0x09000000UL
#  define ASTER_RAM_BASE    0x40000000UL
#  define ASTER_LOAD_ADDR   0x40080000UL
#  define ASTER_KERNEL_ADDR 0x40200000UL
#  define ASTER_BOARD_NAME  "QEMU virt (ARM64)"
#endif

#define ASTER_KERNEL_MAX   (8UL * 1024 * 1024)      /* 内核镜像上限 8 MiB */
#define ASTER_FLASH_BUF    0x44100000UL             /* 刷写暂存区（RAM） */
#define ASTER_FLASH_MAX    (8UL * 1024 * 1024)

#endif
