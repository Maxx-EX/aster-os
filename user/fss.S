/* Aster 用户态示例 —— 随机读（EL0 / AArch64）
 *
 * 验证刚补上的 62 lseek 与 80 fstat：
 *   openat("/note.txt") → lseek(SEEK_END) 拿文件长度 → fstat 读元信息
 *   → lseek(SEEK_SET) → read 前 64 字节 → 打到屏幕
 * 再打开根目录，用 61 readdir 列一遍目录，看内核把条目数打出来。
 *
 * 注意 note.txt 里是 README 全文（19 KiB 左右），比旧的 4 KiB 页缓冲大得多：
 * 现在 VFS 只读打开不搬内容，read 按偏移从块缓存取，所以多大都能读。
 */

    .section .text.start, "ax"
    .global _start

_start:
    /* ---- openat("/note.txt", O_RDONLY) ---- */
    mov     x0, #0
    adr     x1, s_note
    mov     x2, #0
    mov     x3, #0
    mov     x8, #56
    svc     #0
    mov     x19, x0
    cmp     x19, #0
    b.lt    .Lfail

    /* ---- lseek(fd, 0, SEEK_END)：长度回给内核打印 ---- */
    mov     x0, x19
    mov     x1, #0
    mov     x2, #2
    mov     x8, #62
    svc     #0

    /* ---- fstat(fd, buf)：内核把 size/ino/mode/type 打出来 ---- */
    mov     x0, x19
    sub     x1, sp, #64
    mov     x8, #80
    svc     #0

    /* ---- lseek(fd, 0, SEEK_SET) ---- */
    mov     x0, x19
    mov     x1, #0
    mov     x2, #0
    mov     x8, #62
    svc     #0

    /* ---- read(fd, buf, 64) ---- */
    mov     x0, x19
    sub     x1, sp, #128
    mov     x2, #64
    mov     x8, #63
    svc     #0
    mov     x2, x0
    cmp     x2, #0
    b.le    .Lnl

    /* ---- write(1, buf, n) ---- */
    mov     x0, #1
    sub     x1, sp, #128
    mov     x8, #64
    svc     #0

.Lnl:
    mov     x0, #1
    adr     x1, s_nl
    mov     x2, #1
    mov     x8, #64
    svc     #0

    mov     x0, x19
    mov     x8, #57                 /* close */
    svc     #0

    /* ---- openat("/", O_RDONLY) → 目录句柄 ---- */
    mov     x0, #0
    adr     x1, s_root
    mov     x2, #0
    mov     x3, #0
    mov     x8, #56
    svc     #0
    mov     x20, x0
    cmp     x20, #0
    b.lt    .Ldone

    /* ---- readdir(dirfd, buf, 256) ---- */
    mov     x0, x20
    sub     x1, sp, #256
    mov     x2, #256
    mov     x8, #61
    svc     #0

    mov     x0, x20
    mov     x8, #57
    svc     #0

.Ldone:
    mov     x0, #0
    mov     x8, #93
    svc     #0
    b       .Ldone

.Lfail:
    mov     x0, #1
    adr     x1, s_fail
    mov     x2, #10
    mov     x8, #64
    svc     #0
    b       .Ldone

    .section .rodata, "a"
s_note:
    .ascii  "/note.txt\0"
s_root:
    .ascii  "/\0"
s_nl:
    .ascii  "\n"
s_fail:
    .ascii  "OPEN FAIL\n"
