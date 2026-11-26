/* Aster Boot — 内嵌内核镜像的定位与加载 */
#ifndef ASTER_IMAGE_H
#define ASTER_IMAGE_H

#include <stdint.h>
#include <stddef.h>

const void *image_payload(void);
size_t      image_size(void);
int         image_load_to(uint64_t dst, const void *src, size_t len);
void        image_jump(uint64_t entry, void *arg);

#endif
