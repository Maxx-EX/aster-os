#!/usr/bin/env python3
"""在主机上把 AsterDisk + AsterFS 镜像直接写出来，并塞进几个文件。

内核里的 `mkfs` 只能在 QEMU 里格式化一张空盘，用户程序没办法从主机搬进去。
这个脚本补上那一步：构建时就把 `build/user/*.bin` 打进镜像的根目录，开机
`s load hello` 直接读盘装载 —— 内核里不再有 include_bytes! 的用户程序。

布局（与 kernel/src/disk.rs、kernel/src/afs.rs 一字不差）：
    扇区 0        磁盘头 ADSK：magic@0 ver@4 sec@8 total@12 name@16 crc@48
    扇区 1        分区表 4×64B：name@0 start@16 sectors@24 kind@32 crc@+60
    分区 0 从 LBA 2 起，AsterFS 以 4 KiB 为块：
    块 0          超级块：AFS1@0 ver@4 bs@8 blocks@12 inodes@16 root@20
                  inode_table@24 itb@28 inode_bitmap@32 block_bitmap@36
                  data_start@40 data_blocks@44 free@48 crc@56 name@64
    块 1          inode 位图（bit n = inode n 已用）
    块 2          块位图（bit b = 块 b 已用）
    块 3..6       inode 表（64 B/个，256 个）
    块 7          根目录数据（32 B/项：name@0 ino@24 type@28）
    块 8..        文件数据
"""

import struct
import sys
import zlib

SEC = 512
SPB = 8              # 每块 8 个扇区 = 4 KiB
BS = 4096
PART_LBA = 2
NDIRECT = 10
INODE_SIZE = 64
ENT = 32
NAME_MAX = 24
MAX_INODES = 256
INODE_TABLE = 3
INODE_TABLE_BLOCKS = 4
INODE_BITMAP = 1
BLOCK_BITMAP = 2
DATA_START = 7
T_FILE, T_DIR = 1, 2


def crc32(data):
    return zlib.crc32(data) & 0xFFFFFFFF


def u16(b, o, v):
    struct.pack_into("<H", b, o, v)


def u32(b, o, v):
    struct.pack_into("<I", b, o, v)


def u64(b, o, v):
    struct.pack_into("<Q", b, o, v)


class Disk:
    def __init__(self, path, size_mb):
        self.path = path
        self.size = size_mb * 1024 * 1024
        self.total = self.size // SEC
        self.f = open(path, "wb")
        self.f.truncate(self.size)

    def write_sectors(self, lba, data):
        assert len(data) % SEC == 0
        self.f.seek(lba * SEC)
        self.f.write(data)

    def write_block(self, blk, data):
        assert len(data) == BS
        self.write_sectors(PART_LBA + blk * SPB, data)

    def write_head(self, name="asterdisk", part_name="asterfs"):
        head = bytearray(SEC)
        head[0:4] = b"ADSK"
        u32(head, 4, 1)
        u32(head, 8, SEC)
        u32(head, 12, self.total)
        nb = name.encode()[:31]
        head[16:16 + len(nb)] = nb
        u32(head, 48, crc32(bytes(head[0:48])))
        self.write_sectors(0, bytes(head))

        pt = bytearray(SEC)
        pb = part_name.encode()[:15]
        pt[0:len(pb)] = pb
        u64(pt, 16, PART_LBA)
        u64(pt, 24, self.total - PART_LBA)
        u32(pt, 32, 1)
        u32(pt, 60, crc32(bytes(pt[0:60])))
        self.write_sectors(1, bytes(pt))

    def close(self):
        self.f.close()


def build(path, size_mb, files):
    if size_mb < 4:
        sys.exit("镜像太小，至少 4 MiB")
    d = Disk(path, size_mb)
    d.write_head()

    blocks = (d.total - PART_LBA) // SPB
    if blocks <= DATA_START + 8:
        sys.exit("分区太小")
    data_blocks = blocks - DATA_START

    # 超级块
    sb = bytearray(BS)
    sb[0:4] = b"AFS1"
    u32(sb, 4, 1)
    u32(sb, 8, BS)
    u32(sb, 12, blocks)
    u32(sb, 16, MAX_INODES)
    u32(sb, 20, 0)
    u32(sb, 24, INODE_TABLE)
    u32(sb, 28, INODE_TABLE_BLOCKS)
    u32(sb, 32, INODE_BITMAP)
    u32(sb, 36, BLOCK_BITMAP)
    u32(sb, 40, DATA_START)
    u32(sb, 44, data_blocks)
    vol = b"asterfs root volume"
    sb[64:64 + len(vol)] = vol

    # inode 位图：inode 0 是根目录，之后每个文件一个
    ibm = bytearray(BS)
    for i in range(len(files) + 1):
        ibm[i // 8] |= 1 << (i % 8)

    # 块位图：元数据块 0..DATA_START 都用掉了
    bbm = bytearray(BS)
    for b in range(0, DATA_START + 1):
        bbm[b // 8] |= 1 << (b % 8)

    # inode 表：4 块，根目录在 0 号
    itab = bytearray(BS * INODE_TABLE_BLOCKS)

    def inode(ino, typ, mode, size, blocks_list, parent=0):
        o = ino * INODE_SIZE
        u16(itab, o + 0, typ)
        u16(itab, o + 2, mode)
        u32(itab, o + 4, size)
        u32(itab, o + 8, parent)
        u32(itab, o + 12, 0)
        for i, b in enumerate(blocks_list[:NDIRECT]):
            u32(itab, o + 16 + i * 4, b)
        u16(itab, o + 56, 1000)
        u16(itab, o + 58, 1000)

    inode(0, T_DIR, 0o755, ENT * (2 + len(files)), [DATA_START])

    # 根目录
    root = bytearray(BS)
    root[0:1] = b"."
    u32(root, 24, 0)
    u16(root, 28, T_DIR)
    root[32:34] = b".."
    u32(root, 32 + 24, 0)
    u16(root, 32 + 28, T_DIR)
    ent = 2

    next_blk = DATA_START + 1
    layout = []
    for ino, (name, payload) in enumerate(files, start=1):
        if len(name.encode()) > NAME_MAX:
            sys.exit("名字太长：%s" % name)
        if len(payload) > BS * NDIRECT:
            sys.exit("%s 超过 %d 块上限" % (name, NDIRECT))
        nblk = max(1, (len(payload) + BS - 1) // BS)
        blks = []
        for _ in range(nblk):
            blks.append(next_blk)
            chunk = payload[(_) * BS:(_ + 1) * BS]
            pad = bytearray(BS)
            pad[0:len(chunk)] = chunk
            d.write_block(next_blk, bytes(pad))
            bbm[next_blk // 8] |= 1 << (next_blk % 8)
            next_blk += 1
        inode(ino, T_FILE, 0o644, len(payload), blks)
        o = ent * ENT
        root[o:o + len(name)] = name.encode()
        u32(root, o + 24, ino)
        u16(root, o + 28, T_FILE)
        ent += 1
        layout.append("%-10s ino=%-3d %5d B  blk=%s" % (name, ino, len(payload), blks))

    u32(sb, 48, data_blocks - 1 - (next_blk - DATA_START - 1))
    u32(sb, 56, crc32(bytes(sb[0:56])))

    d.write_block(0, bytes(sb))
    d.write_block(INODE_BITMAP, bytes(ibm))
    d.write_block(BLOCK_BITMAP, bytes(bbm))
    for i in range(INODE_TABLE_BLOCKS):
        d.write_block(INODE_TABLE + i, bytes(itab[i * BS:(i + 1) * BS]))
    d.write_block(DATA_START, bytes(root))
    d.close()

    print("[MKAFS] %s  %d MiB  块 %d  inode 256  文件 %d"
          % (path, size_mb, blocks, len(files)))
    for line in layout:
        print("        " + line)
    return layout


def main(argv):
    if len(argv) < 3:
        print("用法: mkafs.py <img> <size_mb> [名字=路径 ...]")
        return 2
    path, size = argv[1], int(argv[2])
    files = []
    for spec in argv[3:]:
        if "=" not in spec:
            print("参数要写成 名字=路径：%s" % spec)
            return 2
        name, src = spec.split("=", 1)
        try:
            with open(src, "rb") as fh:
                payload = fh.read()
        except FileNotFoundError:
            print("[MKAFS] 跳过（还不存在）：%s" % src)
            continue
        files.append((name, payload))
    build(path, size, files)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
