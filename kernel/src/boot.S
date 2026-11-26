/* Aster Kernel — AArch64 入口（引导器把镜像载入 0x40200000 后跳到这里）
 * x0 = struct aster_bootinfo 指针
 */
    .section .text.boot, "ax"
    .global _start

_start:
    /* 内核自己的栈 */
    ldr     x1, =__stack_top
    mov     sp, x1

    /* 放行 FP/SIMD（CPACR_EL1.FPEN=0b11），否则编译器用到的 NEON 指令会触发陷阱 */
    mov     x1, #0x300000
    msr     cpacr_el1, x1
    isb

    /* 清内核 .bss（引导器只清了它自己的） */
    ldr     x2, =__bss_start
    ldr     x3, =__bss_end
.Lbss:
    cmp     x2, x3
    b.hs    .Lbss_done
    str     xzr, [x2], #8
    b       .Lbss
.Lbss_done:

    bl      kernel_main

.Lhalt:
    wfe
    b       .Lhalt
