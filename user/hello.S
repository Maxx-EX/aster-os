/* Aster 用户态示例程序（EL0 / AArch64）
 *
 * 这个程序不是内核的一部分：它是一个独立的扁平二进制，被链接到 0x44400000，
 * 由内核在 shell 里用 run 命令把它映射进用户页表、切到 EL0 执行。
 *
 * 它只能用 svc 走系统调用，不能直接碰 UART、不能改页表、不能访问内核内存 ——
 * 违反任何一条都会触发异常被内核捕获。
 *
 * 系统调用号沿用 AArch64 Linux 编号，方便以后移植现成的用户态程序：
 *    64 = write(fd, buf, len)
 *    93 = exit(code)
 *   172 = getpid()
 * 约定：x8 = 号，x0..x2 = 参数，返回值在 x0。
 */

    .section .text.start, "ax"
    .global _start

_start:
    /* ---- 1) write(1, "Aster 用户态已启动 ...") ---- */
    mov     x0, #1                      /* fd = 1 stdout */
    adr     x1, msg_hello
    mov     x2, #(msg_hello_end - msg_hello)
    mov     x8, #64                     /* SYS_write */
    svc     #0
    mov     x19, x0                     /* 记下返回值，稍后证明它是真值 */

    /* ---- 2) getpid() ---- */
    mov     x8, #172                    /* SYS_getpid */
    svc     #0
    mov     x20, x0

    /* ---- 3) write(1, "...") ---- */
    mov     x0, #1
    adr     x1, msg_svc
    mov     x2, #(msg_svc_end - msg_svc)
    mov     x8, #64
    svc     #0

    /* ---- 4) 演示一次非法调用：号 9999 应该返回 -ENOSYS 而不是把内核打死 ---- */
    mov     x8, #9999
    svc     #0

    /* ---- 5) write(1, "...") ---- */
    mov     x0, #1
    adr     x1, msg_bye
    mov     x2, #(msg_bye_end - msg_bye)
    mov     x8, #64
    svc     #0

    /* ---- 6) exit(7)：退出码 7 一路带回内核 ---- */
    mov     x0, #7
    mov     x8, #93                     /* SYS_exit */
    svc     #0

    /* exit 正常不会返回；万一返回就自锁 */
1:  b       1b

    .section .rodata, "a"
msg_hello:
    .ascii  "Aster 用户态已启动 (EL0 · AArch64)\n"
msg_hello_end:
msg_svc:
    .ascii  "这句是用户程序通过 svc #0 发给内核、再经内核驱动串口打出来的\n"
msg_svc_end:
msg_bye:
    .ascii  "用户态即将 exit(7)\n"
msg_bye_end:
