/* Aster Boot — 引导主流程
 *
 *   上电
 *    └─ 初始化串口 / 计时器
 *       └─ AVP 三级校验内嵌内核镜像
 *          ├─ 通过 → 3 秒内无人按 B → 载入 0x40200000 → 跳 EL1 进内核
 *          │        按了 B  → 底层模式（Recovery Shell）
 *          └─ 失败 → 强制进入底层模式，绝不放行
 */
#include "uart.h"
#include "verify.h"
#include "image.h"
#include "shell.h"
#include "board.h"
#include "bootinfo.h"
#include "timer.h"
#include "flash.h"

static struct aster_bootinfo g_bootinfo;

static void press_b_window(void)
{
    uart_puts("\n[MENU] 3 秒内按 B 进入底层模式，其余任意键继续启动...\n");
    uint64_t t0 = timer_us();
    while (timer_us() - t0 < 3000000ull) {
        int c = uart_getc();
        if (c < 0) continue;
        if (c == 'b' || c == 'B') {
            uart_puts("[MENU] 用户请求底层模式\n");
            g_bootinfo.trusted = 0;   /* 标记为由用户主动中断 */
            return;
        }
        uart_puts("[MENU] 跳过等待，继续启动\n");
        return;
    }
}

void boot_main(uint64_t dtb)
{
    uart_init();

    uart_puts("\n");
    uart_puts("==================================================\n");
    uart_puts("   ASTER BOOT  v0.1  /  ARM64 原创引导器\n");
    uart_puts("   Board: "); uart_puts(ASTER_BOARD_NAME); uart_putc('\n');
    uart_puts("==================================================\n");

    uart_print_hex("[BOOT] DTB        @ ", dtb);
    uart_print_hex("[BOOT] Payload    @ ", (uint64_t)image_payload());
    bprintf("[BOOT] Payload len : %d bytes\n", (int)image_size());

    uart_puts("\n[AVP] 开始三级校验 ...\n");
    avp_result r = avp_check(image_payload(), image_size());
    avp_print_report(&r);

    g_bootinfo.magic        = ASTER_BOOT_MAGIC;
    g_bootinfo.dtb          = dtb;
    g_bootinfo.payload_size = image_size();
    g_bootinfo.avp_elapsed_us = (uint32_t)r.elapsed_us;
    g_bootinfo.trusted      = (uint32_t)r.trusted;
    for (int i = 0; i < 32; i++) {
        g_bootinfo.image_hash[i] = r.hash[i];
        g_bootinfo.image_mac[i]  = r.mac[i];
    }

    if (!r.trusted) {
        uart_puts("\n[AVP] 校验未通过，引导器拒绝放行内核。\n");
        shell_run(&r);
        /* shell 明确要求启动时才继续；仍要求校验通过或使用已校验的暂存镜像 */
    } else {
        press_b_window();
        if (!g_bootinfo.trusted) {
            shell_run(&r);
        }
    }

    /* 选择启动源：优先用刷写暂存区里已校验的镜像 */
    const afp_state *s = afp_get_state();
    const void *src;
    size_t len;
    if (g_bootinfo.trusted == 0 && s->staged) {
        src = (const void *)s->addr;
        len = s->size;
        uart_puts("[BOOT] 从 AFP 暂存区启动\n");
    } else {
        src = image_payload();
        len = image_size();
        uart_puts("[BOOT] 从内嵌镜像启动\n");
    }

    uart_puts("[BOOT] 载入内核 ...\n");
    if (image_load_to(ASTER_KERNEL_ADDR, src, len) != 0) {
        uart_puts("[BOOT] 载入失败，停在此处。\n");
        for (;;) __asm__ volatile("wfe");
    }

    g_bootinfo.payload_base = ASTER_KERNEL_ADDR;
    g_bootinfo.payload_size = len;

    uart_print_hex("[BOOT] Entry      @ ", ASTER_KERNEL_ADDR);
    uart_puts("[BOOT] 跳转到 EL1 ...\n\n");
    timer_spin_ms(20);

    image_jump(ASTER_KERNEL_ADDR, &g_bootinfo);

    /* 正常情况下永远回不到这里 */
    for (;;) __asm__ volatile("wfe");
}
