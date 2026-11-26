/* Aster 用户态 —— W^X 验证（写自己的代码页应当被硬件拒绝）
 *
 * 代码页在页表里是 AP=0b10（EL1 可写、EL0 只读），UXN=0 允许执行。
 * 所以 EL0 执行没问题，但一旦 store 进去就会触发 Data Abort(低 EL)。
 */

    .section .text.start, "ax"
    .global _start

_start:
    mov     x0, #1
    adr     x1, s_msg
    mov     x2, #12
    mov     x8, #64
    svc     #0

    ldr     x1, =0x44400000         /* 自己的代码页 */
    mov     x2, #0x99
    str     x2, [x1]                /* 应该在这里被 MMU 拦下 */

    /* 能走到这里说明代码页是可写的 —— W^X 没生效 */
    mov     x0, #1
    adr     x1, s_bad
    mov     x2, #14
    mov     x8, #64
    svc     #0

    mov     x0, #0
    mov     x8, #93
    svc     #0
1:  b       1b

    .section .rodata, "a"
s_msg:  .ascii  "WX start...\n"
s_bad:  .ascii  "WX BAD writab\n"
