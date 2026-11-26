/* Aster Verify Protocol (AVP) — 原创三级镜像校验
 *
 *  L1 完整性：SHA-256(payload)              → 与清单里的 expected_hash 比对
 *  L2 真实性：HMAC-SHA256(K, payload)       → 与清单里的 expected_mac 比对
 *  L3 链绑定：HMAC-SHA256(K, BOOT_ID||L2)   → 保证镜像与引导器版本绑定，
 *                                            单独替换内核而不重新签名会失败
 *
 *  K 只存在于引导器的只读区（由工具链从 keys/boot.key 注入），
 *  内核侧拿不到 K，因此内核无法伪造自己的校验结果。
 */
#ifndef ASTER_VERIFY_H
#define ASTER_VERIFY_H

#include <stdint.h>
#include <stddef.h>

#define AVP_STAGE_NONE  0
#define AVP_STAGE_L1    1
#define AVP_STAGE_L2    2
#define AVP_STAGE_L3    3
#define AVP_STAGE_ALL   4

typedef struct {
    int      trusted;        /* 1 = 三级全过 */
    int      failed_stage;   /* 0 = 不适用 */
    uint64_t elapsed_us;     /* 校验耗时 */
    uint8_t  hash[32];
    uint8_t  mac[32];
    uint8_t  chain[32];
    uint8_t  ex3[32];        /* 本次构建的原创校验值 */
} avp_result;

avp_result avp_check(const void *payload, size_t len);
void avp_print_report(const avp_result *r);
void avp_hex16(const uint8_t *d, char out[33]);

#endif
