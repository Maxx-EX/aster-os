/* Aster Boot — 底层刷写（AFP: Aster Flash Protocol）
 *
 * 主机端 → 引导器：
 *   'W'                 请求进入刷写
 *   引导器 → "OK\n"
 *   长度 4 字节（大端）
 *   引导器 → "SIZE\n"
 *   期望 MAC 32 字节（由 tools/aster-sign 生成）
 *   引导器 → "DATA\n"
 *   镜像数据
 *   引导器 → "FLASH OK trusted=1\n" 或 "FLASH REJECT stage=N\n"
 *
 * 刷写进来的镜像必须能通过同一套 AVP 校验才会被标记为可信。
 */
#ifndef ASTER_FLASH_H
#define ASTER_FLASH_H

#include <stdint.h>
#include <stddef.h>

typedef struct {
    int      staged;        /* 1 = 有可信的暂存镜像 */
    uint64_t addr;
    size_t   size;
    uint8_t  mac[32];
} afp_state;

const afp_state *afp_run_session(void);
const afp_state *afp_get_state(void);

#endif
