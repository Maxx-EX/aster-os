/* Aster Boot — 底层模式（Recovery Shell） */
#include "shell.h"
#include "uart.h"
#include "board.h"
#include "image.h"
#include "flash.h"
#include "verify.h"
#include "timer.h"
#include "manifest.h"

static char line[128];

static int read_line(void)
{
    int n = 0;
    for (;;) {
        int c = uart_getc_blocking();
        if (c == '\r' || c == '\n') {
            uart_putc('\n');
            line[n] = 0;
            return n;
        }
        if ((c == 0x7F || c == '\b') && n > 0) {
            n--;
            uart_puts("\b \b");
            continue;
        }
        if (c >= 0x20 && c < 0x7F && n < (int)sizeof(line) - 1) {
            line[n++] = (char)c;
            uart_putc((char)c);
        }
    }
}

static int str_eq(const char *a, const char *b)
{
    while (*a && *b) { if (*a != *b) return 0; a++; b++; }
    return *a == *b;
}

static long parse_num(const char *s)
{
    long v = 0;
    int base = 10;
    if (s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) { base = 16; s += 2; }
    if (!*s) return -1;
    for (; *s; s++) {
        int d;
        if (*s >= '0' && *s <= '9') d = *s - '0';
        else if (*s >= 'a' && *s <= 'f') d = *s - 'a' + 10;
        else if (*s >= 'A' && *s <= 'F') d = *s - 'A' + 10;
        else return -1;
        v = v * base + d;
    }
    return v;
}

static void cmd_help(void)
{
    uart_puts(
        "\n可用命令:\n"
        "  help                显示本帮助\n"
        "  info                板级与镜像信息\n"
        "  verify              重新执行 AVP 三级校验\n"
        "  flash               进入 AFP 串口刷写\n"
        "  staged              查看刷写暂存区状态\n"
        "  boot                校验通过则启动内核\n"
        "  dump <addr> <len>   十六进制转储内存\n"
        "  reset               复位（PSCI）\n"
        "  exit                退出底层模式（等价 boot）\n");
}

static void cmd_info(void)
{
    uart_puts("\n  Board        : ");
    uart_puts(ASTER_BOARD_NAME);
    uart_putc('\n');
    uart_print_hex("  加载地址     : ", ASTER_LOAD_ADDR);
    uart_print_hex("  内核加载地址 : ", ASTER_KERNEL_ADDR);
    uart_print_hex("  内嵌镜像     : ", (uint64_t)image_payload());
    bprintf("  镜像大小     : %d bytes\n", (int)image_size());
    bprintf("  镜像版本     : %s\n", ASTER_PAYLOAD_BUILDID);
    bprintf("  计时器频率   : %d Hz\n", (int)timer_freq());
}

static void cmd_dump(const char *args)
{
    char a[32], b[32];
    int i = 0, j = 0;
    while (*args == ' ') args++;
    while (*args && *args != ' ' && i < 31) a[i++] = *args++;
    a[i] = 0;
    while (*args == ' ') args++;
    while (*args && *args != ' ' && j < 31) b[j++] = *args++;
    b[j] = 0;

    long addr = parse_num(a), len = parse_num(b);
    if (addr < 0 || len <= 0 || len > 4096) { uart_puts("用法: dump <addr> <len>  (len <= 4096)\n"); return; }

    const uint8_t *p = (const uint8_t *)addr;
    for (long off = 0; off < len; off += 16) {
        uart_puts("  ");
        bprintf("%d: ", (int)addr + (int)off);
        uart_puts("0x");
        bprintf("%x: ", (uint32_t)(addr + off));
        for (int k = 0; k < 16 && off + k < len; k++) {
            bprintf("%x ", (uint32_t)p[off + k]);
        }
        uart_putc('\n');
    }
}

static void cmd_staged(void)
{
    const afp_state *s = afp_get_state();
    if (!s->staged) { uart_puts("  暂存区为空（没有经过校验的刷写镜像）\n"); return; }
    uart_print_hex("  暂存地址 : ", s->addr);
    bprintf("  暂存大小 : %d bytes\n", (int)s->size);
}

static void cmd_verify(void)
{
    avp_result r = avp_check(image_payload(), image_size());
    avp_print_report(&r);
}

static void psci_reset(void)
{
    /* PSCI SYSTEM_RESET：HVC 调 0x84000009 */
    __asm__ volatile("mov x0, #0x84000000\n"
                     "add x0, x0, #9\n"
                     "hvc #0\n" ::: "x0", "memory");
}

int shell_run(const avp_result *report)
{
    uart_puts("\n==============================================\n");
    uart_puts("  ASTER BOOT / 底层模式  -  Recovery Shell\n");
    uart_puts("==============================================\n");
    if (report) {
        uart_puts("  校验结论: ");
        uart_puts(report->trusted ? "TRUSTED\n" : "REJECTED —— 内核不会自动启动\n");
        if (!report->trusted) {
            uart_puts("  失败阶段: L");
            bprintf("%d", report->failed_stage);
            uart_puts("  （可在 flash 命令下重新写入已签名镜像）\n");
        }
    }
    uart_puts("  输入 help 查看命令。\n");

    for (;;) {
        uart_puts("boot> ");
        int n = read_line();
        if (n == 0) continue;

        if (str_eq(line, "help"))    { cmd_help(); }
        else if (str_eq(line, "info"))   { cmd_info(); }
        else if (str_eq(line, "verify")) { cmd_verify(); }
        else if (str_eq(line, "staged")) { cmd_staged(); }
        else if (str_eq(line, "reset"))  { psci_reset(); }
        else if (str_eq(line, "boot") || str_eq(line, "exit")) {
            if (!report || !report->trusted) {
                const afp_state *s = afp_get_state();
                if (s->staged) return 1;                 /* 用暂存镜像启动 */
                uart_puts("  拒绝：当前镜像未通过校验，先执行 verify 或 flash。\n");
                continue;
            }
            return 1;
        }
        else if (str_eq(line, "flash")) {
            afp_run_session();
        }
        else if (line[0] == 'd' && line[1] == 'u') {
            cmd_dump(line + 4);
        }
        else {
            uart_puts("  未知命令: ");
            uart_puts(line);
            uart_puts("  （输入 help）\n");
        }
    }
}
