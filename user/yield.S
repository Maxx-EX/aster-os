/* Aster 用户态 —— sched_yield 验证
 *
 * 两个任务各打印 8 行 "Y<n>"，每打印完就主动让出 CPU（syscall 124）。
 * 如果让出生效，输出应该是 Y1 Y2 Y1 Y2 ... 严格交替，
 * 而不是某一边连续刷完（那说明只能靠 2 ms 时间片硬切）。
 */

    .section .text.start, "ax"
    .global _start

_start:
    mov     x8, #172
    svc     #0
    mov     x19, x0
    mov     x20, #8

1:
    /* write("Y", 1) */
    mov     x0, #1
    adr     x1, s_y
    mov     x2, #1
    mov     x8, #64
    svc     #0

    /* write(&digit, 1) */
    add     x0, x19, #'0'
    sub     x1, sp, #16
    strb    w0, [x1]
    mov     x0, #1
    mov     x2, #1
    mov     x8, #64
    svc     #0

    /* write("\n", 1) */
    mov     x0, #1
    adr     x1, s_nl
    mov     x2, #1
    mov     x8, #64
    svc     #0

    /* yield */
    mov     x8, #124
    svc     #0

    subs    x20, x20, #1
    b.ne    1b

    mov     x0, #0
    mov     x8, #93
    svc     #0
2:  b       2b

    .section .rodata, "a"
s_y:    .ascii  "Y"
s_nl:   .ascii  "\n"
