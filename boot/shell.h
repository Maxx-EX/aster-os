/* Aster Boot — 底层模式（Recovery Shell）
 * 校验失败或用户主动按 B 时进入，这里可以看校验详情、刷写、转储内存、重启。
 */
#ifndef ASTER_SHELL_H
#define ASTER_SHELL_H

#include "verify.h"

/* 返回值：1 = 用户要求启动内核；0 = 继续留在底层模式 */
int shell_run(const avp_result *report);

#endif
