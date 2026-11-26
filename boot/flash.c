/* Aster Boot — 底层刷写（AFP）实现 */
#include "flash.h"
#include "board.h"
#include "uart.h"
#include "sha256.h"
#include "manifest.h"

static afp_state g_state;

const afp_state *afp_get_state(void) { return &g_state; }

static void read_exact(uint8_t *dst, size_t n)
{
    for (size_t i = 0; i < n; i++) dst[i] = (uint8_t)uart_getc_blocking();
}

const afp_state *afp_run_session(void)
{
    g_state.staged = 0;

    uart_puts("\n[AFP] 等待主机握手 (发送 'W')...\n");
    int c;
    do { c = uart_getc_blocking(); } while (c != 'W' && c != 'w');
    uart_puts("OK\n");

    uint8_t sz[4];
    read_exact(sz, 4);
    uint32_t len = ((uint32_t)sz[0] << 24) | ((uint32_t)sz[1] << 16) |
                   ((uint32_t)sz[2] << 8)  | (uint32_t)sz[3];
    uart_puts("SIZE\n");

    if (len == 0 || len > ASTER_FLASH_MAX) {
        uart_puts("FLASH REJECT reason=size\n");
        return &g_state;
    }

    uint8_t expect_mac[32];
    read_exact(expect_mac, 32);
    uart_puts("DATA\n");

    volatile uint8_t *dst = (volatile uint8_t *)ASTER_FLASH_BUF;
    for (uint32_t i = 0; i < len; i++) dst[i] = (uint8_t)uart_getc_blocking();

    uart_puts("\n[AFP] 接收完成，开始 AVP 校验...\n");
    uint8_t mac[32];
    hmac_sha256(ASTER_BOOT_KEY, ASTER_BOOT_KEY_LEN, (const void *)ASTER_FLASH_BUF, len, mac);

    if (!aster_memeq_ct(mac, expect_mac, 32)) {
        uart_puts("FLASH REJECT stage=2 (MAC 不匹配)\n");
        return &g_state;
    }

    g_state.staged = 1;
    g_state.addr = ASTER_FLASH_BUF;
    g_state.size = len;
    for (int i = 0; i < 32; i++) g_state.mac[i] = mac[i];

    uart_puts("FLASH OK trusted=1 size=");
    bprintf("%d", (int)len);
    uart_puts("\n");
    return &g_state;
}
