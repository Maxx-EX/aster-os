/* Aster 原生程序示例（XCE，EL0 / AArch64）
 *
 * 这就是 XCE 格式里装的"body"：一段扁平二进制，链接时固定落在 0x4440_0000。
 * 它不知道自己是文件还是内嵌镜像，只按 SVC 约定跟内核说话：
 *    64 = write(fd, buf, len)   93 = exit(code)
 *
 * 打包时 tools/mkxce.py 会套上 28 字节头（魔数/版本/入口/长度/CRC32），
 * 内核核验通过后把它装进一个新地址空间，切 EL0 跑。
 */

    .section .text.start, "ax"
    .global _start

_start:
    mov     x0, #1
    adr     x1, s_msg
    mov     x2, #MSGLEN
    mov     x8, #64
    svc     #0

    mov     x0, #7                  /* 退出码 7，方便在日志里认出来 */
    mov     x8, #93
    svc     #0
1:  b       1b

    .section .rodata, "a"
s_msg:
    .ascii  "XCE ok: native program, EL0, Aster\n"
    .equ    MSGLEN, . - s_msg
