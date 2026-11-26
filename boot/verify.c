/* Aster Verify Protocol (AVP) — 原创三级镜像校验 */
#include "verify.h"
#include "sha256.h"
#include "uart.h"
#include "manifest.h"
#include "timer.h"

static void hex32(const uint8_t *d, char out[65])
{
    static const char *H = "0123456789abcdef";
    for (int i = 0; i < 32; i++) {
        out[i * 2]     = H[d[i] >> 4];
        out[i * 2 + 1] = H[d[i] & 0xF];
    }
    out[64] = 0;
}

void avp_hex16(const uint8_t *d, char out[33])
{
    static const char *H = "0123456789abcdef";
    for (int i = 0; i < 16; i++) {
        out[i * 2]     = H[d[i] >> 4];
        out[i * 2 + 1] = H[d[i] & 0xF];
    }
    out[32] = 0;
}

avp_result avp_check(const void *payload, size_t len)
{
    avp_result r = {0};
    uint64_t t0 = timer_us();

    /* EX3 是本次构建的原创校验值，先落位，报告里要显示 */
    for (int i = 0; i < 32; i++) r.ex3[i] = ASTER_EX3[i];

    /* L1：完整性 */
    sha256(payload, len, r.hash);
    if (!aster_memeq_ct(r.hash, ASTER_EXPECTED_HASH, 32)) {
        r.failed_stage = AVP_STAGE_L1;
        r.elapsed_us = timer_us() - t0;
        return r;
    }

    /* L2：真实性 */
    hmac_sha256(ASTER_BOOT_KEY, ASTER_BOOT_KEY_LEN, payload, len, r.mac);
    if (!aster_memeq_ct(r.mac, ASTER_EXPECTED_MAC, 32)) {
        r.failed_stage = AVP_STAGE_L2;
        r.elapsed_us = timer_us() - t0;
        return r;
    }

    /* L3：链绑定 —— 把引导器身份、EX3 和镜像指纹绑在一起 */
    {
        hmac_sha256_ctx c;
        hmac_sha256_init(&c, ASTER_BOOT_KEY, ASTER_BOOT_KEY_LEN);
        hmac_sha256_update(&c, ASTER_BOOT_ID, sizeof(ASTER_BOOT_ID) - 1);
        hmac_sha256_update(&c, "|CHAIN|", 7);
        hmac_sha256_update(&c, ASTER_EX3, 32);
        hmac_sha256_update(&c, r.mac, 32);
        hmac_sha256_final(&c, r.chain);
    }
    if (!aster_memeq_ct(r.chain, ASTER_EXPECTED_CHAIN, 32)) {
        r.failed_stage = AVP_STAGE_L3;
        r.elapsed_us = timer_us() - t0;
        return r;
    }

    r.trusted = 1;
    r.failed_stage = 0;
    r.elapsed_us = timer_us() - t0;
    return r;
}

void avp_print_report(const avp_result *r)
{
    char h1[65], h2[33], h3[33];
    hex32(r->hash, h1);
    avp_hex16(r->mac, h2);
    avp_hex16(r->chain, h3);

    uart_puts("  L1 完整性 SHA-256      : ");
    uart_puts(r->trusted || r->failed_stage != AVP_STAGE_L1 ? "PASS" : "FAIL");
    uart_puts("  sha256=");
    uart_puts(h1);
    uart_putc('\n');

    uart_puts("  L2 真实性 HMAC-SHA256  : ");
    uart_puts(r->trusted || r->failed_stage > AVP_STAGE_L2 ? "PASS" : "FAIL");
    uart_puts("  mac=");
    uart_puts(h2);
    uart_puts("...\n");

    uart_puts("  L3 链绑定(+EX3)        : ");
    uart_puts(r->trusted ? "PASS" : "FAIL");
    uart_puts("  chain=");
    uart_puts(h3);
    uart_puts("...\n");

    uart_puts("  EX3 原创校验值         : ");
    uart_puts(ASTER_EX3_TAG);
    uart_putc('\n');

    uart_puts("  校验耗时               : ");
    bprintf("%d", (int)(r->elapsed_us / 1000));
    uart_puts(" ms\n");

    uart_puts("  结论                   : ");
    uart_puts(r->trusted ? "TRUSTED / 允许启动\n" : "REJECTED / 进入底层模式\n");
}
