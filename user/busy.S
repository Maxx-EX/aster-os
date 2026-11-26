/* Aster 抢占验证程序 —— 纯计算，不调用任何系统调用
 *
 * 只在开头和结尾各 write 一次，中间是一大段减法死循环。
 * 如果调度器没有抢占能力，这段时间里另一个任务一个字都打不出来；
 * 如果抢占有效，另一个任务的输出会夹在 BUSY start 和 BUSY end 之间。
 *
 * 这是"一个失控程序不会锁死整机"的直接证据。
 */
    .section .text.start, "ax"
    .global _start

_start:
    /* write(1, "BUSY start\n", 11) */
    mov     x0, #1
    adr     x1, s_start
    mov     x2, #(s_start_end - s_start)
    mov     x8, #64
    svc     #0

    /* 纯计算：约 1.3 亿次减法，期间没有任何 svc */
    movz    x0, #0x0800, lsl #16
1:  subs    x0, x0, #1
    b.ne    1b

    /* write(1, "BUSY end\n", 9) */
    mov     x0, #1
    adr     x1, s_end
    mov     x2, #(s_end_end - s_end)
    mov     x8, #64
    svc     #0

    mov     x0, #0
    mov     x8, #93
    svc     #0
2:  b       2b

    .section .rodata, "a"
s_start:
    .ascii  "BUSY start\n"
s_start_end:
s_end:
    .ascii  "BUSY end\n"
s_end_end:
