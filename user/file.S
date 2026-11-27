/* Aster 用户态示例 —— fd 系统调用（EL0 / AArch64）
 *
 * 走的是新加的 VFS：openat(56) 拿 fd → read(63) 按偏移读 → write(64) 到屏幕
 * → close(57) 释放。内核侧会打印每一次 fd 调用，串口上能看到 fd 编号和字节数。
 *
 * 测试前先在 shell 里准备文件：ex → mkfs → wr motd <内容> → file
 */

    .section .text.start, "ax"
    .global _start

_start:
    mov     x0, #0                  /* dirfd：不关心，走根目录 */
    adr     x1, s_path
    mov     x2, #0                  /* flags：只读 */
    mov     x3, #0
    mov     x8, #56                 /* openat */
    svc     #0
    mov     x19, x0
    cmp     x0, #0
    b.ge    .Lread

    mov     x0, #1
    adr     x1, s_fail
    mov     x2, #10
    mov     x8, #64
    svc     #0
    b       .Ldone

.Lread:
    mov     x0, x19
    sub     x1, sp, #128
    mov     x2, #96
    mov     x8, #63                 /* read */
    svc     #0
    mov     x2, x0
    cmp     x2, #0
    b.le    .Lnl

    mov     x0, #1
    sub     x1, sp, #128
    mov     x8, #64                 /* write 到 stdout */
    svc     #0

.Lnl:
    mov     x0, #1
    adr     x1, s_nl
    mov     x2, #1
    mov     x8, #64
    svc     #0

.Ldone:
    mov     x0, x19
    mov     x8, #57                 /* close */
    svc     #0
    mov     x0, #0
    mov     x8, #93                 /* exit */
    svc     #0
1:  b       1b

    .section .rodata, "a"
s_path:
    .ascii  "/motd\0"
s_nl:
    .ascii  "\n"
s_fail:
    .ascii  "OPEN FAIL\n"
