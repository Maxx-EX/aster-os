/* Aster 越权访问测试 —— 故意从 EL0 读内核地址
 *
 * 内核镜像在 0x40200000，页表里这一页 AP=0b00（只有 EL1 能访问）。
 * 所以这条 ldr 会触发 Data Abort (EC=0x24, 来自低 EL)，内核打印现场、
 * 杀掉任务、回到 shell —— 全程内核不需要做任何"检查"，是 MMU 硬件拦下来的。
 *
 * 如果这个程序没被拦，说明页表权限写错了，那才是真事故。
 */
    .section .text.start, "ax"
    .global _start

_start:
    movz    x0, #0x4020, lsl #16     /* x0 = 0x40200000 内核镜像基址 */
    ldr     x1, [x0]                 /* ← 这里应该 fault */
    /* 万一没 fault（说明隔离失效），打印一句提示再 exit */
    movz    x0, #1
    adr     x1, msg_leak
    mov     x2, #(msg_leak_end - msg_leak)
    mov     x8, #64
    svc     #0
    mov     x0, #0
    mov     x8, #93
    svc     #0
1:  b       1b

    .section .rodata, "a"
msg_leak:
    .ascii  "!! 隔离失效：EL0 读到了内核内存 !!\n"
msg_leak_end:
