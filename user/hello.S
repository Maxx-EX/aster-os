/* Aster 用户态示例程序（EL0 / AArch64）—— 两个任务共用这一份代码
 *
 * 被链接到 0x44400000（槽 A）和 0x44600000（槽 B）各装一次。
 * 程序自己不知道住在哪个槽，它问内核 getpid() 拿到任务号，
 * 于是同一个二进制在两处打印出 TASK 1 / TASK 2。
 *
 * 每次打印后忙等约 8 ms（用 CNTPCT 计数器），让 2 ms 的时间片
 * 有机会把 CPU 换给另一个任务 —— 这样串口上能看到两个任务交替输出。
 *
 * 系统调用号沿用 AArch64 Linux 编号：
 *    64 = write(fd, buf, len)   93 = exit(code)   172 = getpid()
 */

    .section .text.start, "ax"
    .global _start

_start:
    mov     x19, #30                /* 循环 30 次后退出 */

.Lloop:
    /* ---- 取任务号，转成字符存到栈上 ---- */
    mov     x8, #172                /* SYS_getpid */
    svc     #0
    add     x0, x0, #'0'
    sub     x1, sp, #16
    strb    w0, [x1]

    /* ---- write(1, "TASK ", 5) ---- */
    mov     x0, #1
    adr     x1, s_task
    mov     x2, #5
    mov     x8, #64
    svc     #0

    /* ---- write(1, &digit, 1) ---- */
    mov     x0, #1
    sub     x1, sp, #16
    mov     x2, #1
    mov     x8, #64
    svc     #0

    /* ---- write(1, "\n", 1) ---- */
    mov     x0, #1
    adr     x1, s_nl
    mov     x2, #1
    mov     x8, #64
    svc     #0

    /* ---- 忙等 ≈8 ms：cntfrq/128 个计数 ---- */
    mrs     x5, cntfrq_el0
    lsr     x5, x5, #7
    mrs     x6, cntpct_el0
    add     x6, x6, x5
1:  mrs     x7, cntpct_el0
    cmp     x7, x6
    b.lo    1b

    subs    x19, x19, #1
    b.ne    .Lloop

    /* ---- exit(0) ---- */
    mov     x0, #0
    mov     x8, #93
    svc     #0
2:  b       2b

    .section .rodata, "a"
s_task:
    .ascii  "TASK "
s_nl:
    .ascii  "\n"
