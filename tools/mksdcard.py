#!/usr/bin/env python3
"""把 build/kernel8.img 打成可以直接 dd 到 SD 卡的镜像。

结构是最朴素的树莓派启动布局：

    sector 0         MBR（一个分区，type 0x0C = FAT32 LBA）
    sector 2048 起   FAT16「ASTERBOOT」分区（树莓派固件两种都认，小镜像用 16 更省 cluster），里面放
                       config.txt    引导参数（arm_64bit / kernel8.img / UART）
                       kernel8.img   Aster 引导器（内含内核与校验链）

不用 mkfs 之外的工具在偏移处建文件系统：先建一个独立的 FAT32 分区镜像，
用 mcopy 往里放文件，再整块 dd 回主镜像的 2048 扇区处。
"""
import os
import struct
import subprocess
import sys

MBR_PART_OFF = 2048
SECTOR = 512

CONFIG = """# Aster 星芒 —— 树莓派启动参数
arm_64bit=1
kernel=kernel8.img
enable_uart=1
dtoverlay=disable-bt
disable_overscan=1
gpu_mem=64
"""


def sh(*args):
    subprocess.run(args, check=True)


def mbr(part_sectors: int) -> bytes:
    """一个分区的 MBR：可引导标志 0x80 + type 0x0C + LBA 起止。"""
    code = bytearray(446)   # 446 + 64 分区表 + 2 签名 = 512
    # 引导代码没有：Aster 的引导器在 FAT 分区里由固件加载，不走 MBR
    table = bytearray(64)
    entry = struct.pack(
        "<B3sB3sII",
        0x00,            # 不置可引导位（树莓派固件不看这个）
        b"\x00\x00\x00",
        0x0C,            # FAT32 LBA
        b"\xfe\xff\xff",
        MBR_PART_OFF,
        part_sectors,
    )
    table[0:16] = entry
    sig = b"\x55\xaa"
    return bytes(code) + bytes(table) + sig


def main() -> int:
    if len(sys.argv) < 3:
        print("用法: python3 tools/mksdcard.py <kernel8.img> <out.img> [size_mb] [fw_dir]")
        return 2
    kern, out = sys.argv[1], sys.argv[2]
    size_mb = int(sys.argv[3]) if len(sys.argv) > 3 else 64
    fw = sys.argv[4] if len(sys.argv) > 4 else None
    if not os.path.exists(kern):
        print(f"找不到 {kern}（先 make rpi3）")
        return 1

    tmp = out + ".part"
    total = size_mb * 1024 * 1024
    part_bytes = total - MBR_PART_OFF * SECTOR

    open(tmp, "wb").truncate(part_bytes)
    sh("mkfs.vfat", "-F", "16", "-n", "ASTERBOOT", tmp)
    with open(tmp + ".cfg", "w") as f:
        f.write(CONFIG)
    sh("mcopy", "-i", tmp, kern, "::kernel8.img")
    sh("mcopy", "-i", tmp, tmp + ".cfg", "::config.txt")
    extra = 0
    if fw and os.path.isdir(fw):
        for name in ("bootcode.bin", "start.elf", "fixup.dat"):
            p = os.path.join(fw, name)
            if os.path.exists(p):
                sh("mcopy", "-i", tmp, p, "::" + name)
                extra += 1

    with open(out, "wb") as f:
        f.truncate(total)
    with open(out, "r+b") as f:
        f.seek(0)
        f.write(mbr(part_bytes // SECTOR))
        f.seek(MBR_PART_OFF * SECTOR)
        with open(tmp, "rb") as p:
            f.write(p.read())
    os.remove(tmp)
    os.remove(tmp + ".cfg")

    st = os.stat(out)
    print(f"[SDCARD] {out}  {st.st_size // (1024*1024)} MiB  分区 @ sector {MBR_PART_OFF}  kernel8.img {os.path.getsize(kern)} 字节")
    print(f"[SDCARD] 固件：{extra} 个（VideoCore boot，闭源，不入 git）")
    print(f"[SDCARD] 写入： sudo dd if={out} of=/dev/sdX bs=4M status=progress")
    return 0


if __name__ == "__main__":
    sys.exit(main())
