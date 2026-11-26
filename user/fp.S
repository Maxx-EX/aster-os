/* Aster 用户态 —— 浮点/NEON 上下文验证
 *
 * 两个任务跑同一份代码，各自把 d0 设成不同常量（1.5 / 2.5），
 * 忙等约 16 ms（期间必然被抢占若干次），再算一遍期望值比较。
 * 打印 '.' = 自己的 d0 没被别的任务改掉；'X' = FP 上下文串了。
 *
 * 陷阱帧里如果只存 x0-x30 而不存 q0-q31，这个测试一定会出 'X'。
 */

    .section .text.start, "ax"
    .global _start

_start:
    mov     x8, #172                /* getpid */
    svc     #0
    mov     x19, x0

    cmp     x19, #1
    b.eq    1f
    ldr     d0, lit25
    b       2f
1:  ldr     d0, lit15
2:
    /* ---- 忙等约 16 ms：cntfrq/64 ---- */
    mrs     x5, cntfrq_el0
    lsr     x5, x5, #6
    mrs     x6, cntpct_el0
    add     x6, x6, x5
3:  mrs     x7, cntpct_el0
    cmp     x7, x6
    b.lo    3b

    /* ---- 期望值 ---- */
    cmp     x19, #1
    b.eq    4f
    ldr     d1, lit25
    b       5f
4:  ldr     d1, lit15
5:  fcmp    d0, d1
    adr     x20, s_bad
    b.ne    6f
    adr     x20, s_ok
6:
    /* ---- 打印 "FP n: c\n" ---- */
    mov     x0, #1
    adr     x1, s_fp
    mov     x2, #3
    mov     x8, #64
    svc     #0

    add     x0, x19, #'0'
    sub     x1, sp, #16
    strb    w0, [x1]
    mov     x0, #1
    mov     x2, #1
    mov     x8, #64
    svc     #0

    mov     x0, #1
    adr     x1, s_sep
    mov     x2, #2
    mov     x8, #64
    svc     #0

    mov     x0, #1
    mov     x1, x20
    mov     x2, #1
    mov     x8, #64
    svc     #0

    mov     x0, #1
    adr     x1, s_nl
    mov     x2, #1
    mov     x8, #64
    svc     #0

    mov     x0, #0
    mov     x8, #93
    svc     #0
7:  b       7b

    .section .rodata, "a"
    .align  3
lit15:  .quad   0x3FF8000000000000      /* 1.5 */
lit25:  .quad   0x4004000000000000      /* 2.5 */
s_fp:   .ascii  "FP "
s_sep:  .ascii  ": "
s_ok:   .ascii  "."
s_bad:  .ascii  "X"
s_nl:   .ascii  "\n"
