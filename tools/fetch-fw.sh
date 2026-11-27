#!/bin/sh
# 拉树莓派 boot 固件（bootcode.bin / start.elf / fixup.dat）。
# 这三个文件是 VideoCore 的闭源固件，不属于 Aster，也不进 git；
# 只在下游构建 SD 卡镜像时按需下载到 build/fw/。
set -e
DEST=${1:-build/fw}
mkdir -p "$DEST"
BASE=https://github.com/raspberrypi/firmware/raw/master/boot
for f in bootcode.bin start.elf fixup.dat; do
    if [ -s "$DEST/$f" ]; then
        echo "[FW] $f 已存在，跳过"
        continue
    fi
    echo "[FW] 下载 $f"
    curl -sSL --retry 2 -o "$DEST/$f" "$BASE/$f"
done
ls -l "$DEST"
