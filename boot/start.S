/* Aster Boot — AArch64 入口
 * QEMU -kernel 把控制权交给我们时处于 EL1、MMU 关闭、cache 关闭，
 * x0 指向设备树（DTB）的物理地址。
 */
    .section .text.boot, "ax"
    .global _start

_start:
    /* 只让 0 号核继续跑，其余核停在 wfe */
    mrs     x1, mpidr_el1
    and     x1, x1, #0xff
    cbnz    x1, .Lpark

    /* 栈：linker.ld 里 __stack_top 是 16 字节对齐的高地址 */
    ldr     x2, =__stack_top
    mov     sp, x2

    /* 清 .bss（C 的全局零初始化变量） */
    ldr     x3, =__bss_start
    ldr     x4, =__bss_end
.Lbss_loop:
    cmp     x3, x4
    b.hs    .Lbss_done
    str     xzr, [x3], #8
    b       .Lbss_loop
.Lbss_done:

    /* 放行 FP/SIMD，避免编译期生成的 NEON 指令在 EL1 触发陷阱 */
    mov     x1, #0x300000
    msr     cpacr_el1, x1
    isb

    /* boot_main(dtb_ptr, 0, 0, 0) —— x0 原样就是 DTB 地址 */
    bl      boot_main

.Lpark:
    wfe
    b       .Lpark
