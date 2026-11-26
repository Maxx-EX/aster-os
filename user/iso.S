/* Aster 用户态 —— 地址空间隔离的直接证据
 *
 * 两个任务跑同一份代码、同一个虚拟地址 0x4440_0800（代码页内的数据区），
 * 各自往里写不同的魔数，等大约 16 ms（足够被抢占几次），再读回来。
 * 每个任务看到的是不是自己写的值，决定了打印 '.'（隔离正确）还是 'X'（串了）。
 *
 * 如果两个任务共用一张页表，后写的那个会把先写的覆盖掉，必然出现 'X'。
 */

    .section .text.start, "ax"
    .global _start

_start:
    /* ---- getpid ---- */
    mov     x8, #172
    svc     #0
    mov     x19, x0                 /* x19 = 任务号 */

    /* ---- 决定自己的魔数：任务 1 = 0xAA，其它 = 0xBB ---- */
    ldr     x1, =0x44400800
    cmp     x19, #1
    b.eq    1f
    mov     x2, #0xBB
    b       2f
1:  mov     x2, #0xAA
2:  str     x2, [x1]

    /* ---- 忙等约 16 ms，期间会被时间片切走好几次 ---- */
    mrs     x5, cntfrq_el0
    lsr     x5, x5, #6
    mrs     x6, cntpct_el0
    add     x6, x6, x5
3:  mrs     x7, cntpct_el0
    cmp     x7, x6
    b.lo    3b

    /* ---- 读回同一地址，跟自己写的比 ---- */
    ldr     x3, [x1]
    cmp     x19, #1
    b.eq    4f
    cmp     x3, #0xBB
    b       5f
4:  cmp     x3, #0xAA
5:  adr     x1, s_ok
    b.eq    6f
    adr     x1, s_bad
6:
    /* ---- 打印 "ISO n: c\n" ---- */
    mov     x0, #1
    adr     x1, s_iso
    mov     x2, #4
    mov     x8, #64
    svc     #0

    add     x0, x19, #'0'
    sub     x1, sp, #16
    strb    w0, [x1]
    mov     x0, #1
    mov     x2, #1
    mov     x8, #64
    svc     #0

    /* x1 仍指向结果字符首字节 */
    mov     x0, #1
    adr     x1, s_sep
    mov     x2, #2
    mov     x8, #64
    svc     #0

    /* ---- 重新算结果字符再打印 ---- */
    ldr     x3, [x1]                /* 这里 x1 = s_sep，不再复用；重新读值 */
    ldr     x4, =0x44400800
    ldr     x3, [x4]
    cmp     x19, #1
    b.eq    7f
    cmp     x3, #0xBB
    b       8f
7:  cmp     x3, #0xAA
8:  adr     x1, s_ok
    b.eq    9f
    adr     x1, s_bad
9:  mov     x0, #1
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
10: b      10b

    .section .rodata, "a"
s_iso:  .ascii  "ISO "
s_sep:  .ascii  ": "
s_ok:   .ascii  "."
s_bad:  .ascii  "X"
s_nl:   .ascii  "\n"
