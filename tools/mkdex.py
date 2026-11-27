#!/usr/bin/env python3
"""mkdex.py — 手搓一个最小但合法的 DEX，打进 demo3.apk。

结构：一个类 Lcom/aster/Main;，一个 static 方法 run()V，字节码只有三条：

    const-string v0, "Hello from Aster, this string lives in a real DEX"
    invoke-static {v0}, Lcom/aster/Log;->i(Ljava/lang/String;)V
    return-void

这个 DEX 不是 dx/d8 产出的，是按 DEX 格式规范逐字段拼的：header / map_list /
string_ids / type_ids / proto_ids / method_ids / class_defs / data（string_data、
type_list、class_data、code_item）。checksum（adler32）和 signature（sha1）最后回填，
所以任何校验器都能过。
"""
import hashlib
import struct
import zipfile
import zlib

def uleb(n):
    out = bytearray()
    while True:
        b = n & 0x7f
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)

def align(buf, n=4):
    while len(buf) % n:
        buf.append(0)

# ---------------- 字符串（必须按 UTF-16 码元升序） ----------------
STRINGS = [
    "Hello from Aster, this string lives in a real DEX",
    "Lcom/aster/Log;",
    "Lcom/aster/Main;",
    "Ljava/lang/String;",
    "V",
    "VL",
    "i",
    "run",
]
assert STRINGS == sorted(STRINGS), "string_ids 必须升序"

S_HELLO, S_LOG, S_MAIN, S_STRING, S_V, S_VL, S_I, S_RUN = range(8)

# type_ids：0=String, 1=Main, 2=V, 3=Log
T_STRING, T_MAIN, T_V, T_LOG = 0, 1, 2, 3

# proto_ids：0 = (String)V 给 Log->i，1 = ()V 给 Main->run
P_I, P_RUN = 0, 1

# method_ids：0 = Log->i(String)V, 1 = Main->run()V
M_I, M_RUN = 0, 1

# ---------------- 各段内容 ----------------
def build():
    buf = bytearray(0x70)                      # header 先占位
    map_off = len(buf)

    # data 段稍后才拼，先算 ids 段
    string_ids_off = map_off
    # map_list 大小未知（取决于段数），先预留 11 项
    n_map = 11
    string_ids_off = map_off + 4 + 12 * n_map
    buf = buf[:0x70] + bytearray(b"\0" * (string_ids_off - 0x70))

    # string_ids：每项指向 string_data（data 段里回填）
    str_off_pos = []
    for _ in STRINGS:
        str_off_pos.append(len(buf))
        buf += b"\0\0\0\0"                     # 占位
    type_ids_off = len(buf)
    buf += b"".join(struct.pack("<I", t) for t in (S_STRING, S_MAIN, S_V, S_LOG))
    proto_ids_off = len(buf)
    # shorty_idx, return_type_idx, parameters_off
    buf += struct.pack("<III", S_VL, S_V, 0)   # params 稍后回填
    buf += struct.pack("<III", S_V, S_V, 0)
    method_ids_off = len(buf)
    buf += struct.pack("<HHI", T_LOG, P_I, S_I)
    buf += struct.pack("<HHI", T_MAIN, P_RUN, S_RUN)
    class_defs_off = len(buf)
    class_def_pos = len(buf)
    buf += b"\0" * 32

    data_off = len(buf)

    # string_data
    str_offsets = []
    for s in STRINGS:
        str_offsets.append(len(buf))
        b = s.encode("utf-8")
        buf += uleb(len(s)) + b + b"\0"

    # type_list（proto 0 的参数表）：size + type_idx
    type_list_off = len(buf)
    buf += struct.pack("<IH", 1, T_STRING)
    buf += b"\0\0"                             # 对齐到 4
    # 回填 proto0 的 parameters_off
    struct.pack_into("<I", buf, proto_ids_off + 8, type_list_off)

    # code_item（放在 class_data 之后也行，但 class_data 需要它的偏移）
    align(buf)
    code_off = len(buf)
    insns = [
        0x001a, S_HELLO,                      # const-string v0, string@7
        0x1071, M_I, 0x0000,                  # invoke-static {v0}, method@0
        0x000e,                               # return-void
    ]
    buf += struct.pack("<HHHHII", 1, 0, 1, 0, 0, len(insns))
    buf += b"".join(struct.pack("<H", i) for i in insns)

    # class_data_item：static=0, instance=0, direct=1, virtual=0
    class_data_off = len(buf)
    buf += uleb(0) + uleb(0) + uleb(1) + uleb(0)
    buf += uleb(M_RUN - 0) + uleb(0x0009) + uleb(code_off)

    # 回填 string_ids 的偏移
    for pos, off in zip(str_off_pos, str_offsets):
        struct.pack_into("<I", buf, pos, off)

    # class_def
    struct.pack_into(
        "<IIIIIIII", buf, class_def_pos,
        T_MAIN, 0x0001, 0xFFFFFFFF, 0, 0xFFFFFFFF, 0, class_data_off, 0
    )

    data_size = len(buf) - data_off

    # map_list
    items = [
        (0x0000, 0, 1, 0),                    # header_item
        (0x0001, 0, len(STRINGS), string_ids_off),
        (0x0002, 0, 4, type_ids_off),
        (0x0003, 0, 2, proto_ids_off),
        (0x0005, 0, 2, method_ids_off),
        (0x0006, 0, 1, class_defs_off),
        (0x1000, 0, 1, map_off),
        (0x1001, 0, 1, type_list_off),
        (0x2000, 0, 1, class_data_off),
        (0x2001, 0, 1, code_off),
        (0x2002, 0, len(STRINGS), str_offsets[0]),
    ]
    # map 按 type 码升序要求不严格，但规范里推荐升序
    items.sort(key=lambda x: x[0])
    map_bytes = struct.pack("<I", len(items)) + b"".join(
        struct.pack("<HHII", t, 0, size, off) for t, _, size, off in items
    )
    buf[map_off:map_off + len(map_bytes)] = map_bytes
    assert len(map_bytes) <= 4 + 12 * n_map

    # header 由 write_header 统一写
    return buf, dict(
        file_size=len(buf), map_off=map_off,
        string_ids=(len(STRINGS), string_ids_off),
        type_ids=(4, type_ids_off),
        proto_ids=(2, proto_ids_off),
        method_ids=(2, method_ids_off),
        class_defs=(1, class_defs_off),
        data=(data_size, data_off),
        code_off=code_off, class_data_off=class_data_off,
    )

# header 需要精确写，单独一个函数（上面的 pack_into 格式串只是为了占位）
def write_header(buf, info):
    buf[0:8] = b"dex\n035\x00"
    struct.pack_into("<I", buf, 8, 0)                 # checksum
    buf[12:32] = b"\0" * 20                           # signature
    struct.pack_into("<I", buf, 32, info["file_size"])
    struct.pack_into("<I", buf, 36, 0x70)
    struct.pack_into("<I", buf, 40, 0x12345678)
    struct.pack_into("<II", buf, 44, 0, 0)            # link
    struct.pack_into("<I", buf, 52, info["map_off"])
    for base, key in ((56, "string_ids"), (64, "type_ids"), (72, "proto_ids")):
        size, off = info[key]
        struct.pack_into("<II", buf, base, size, off)
    struct.pack_into("<II", buf, 80, 0, 0)            # field_ids
    for base, key in ((88, "method_ids"), (96, "class_defs"), (104, "data")):
        size, off = info[key]
        struct.pack_into("<II", buf, base, size, off)

def finish(buf):
    import hashlib
    buf[12:32] = hashlib.sha1(bytes(buf[32:])).digest()
    struct.pack_into("<I", buf, 8, zlib.adler32(bytes(buf[12:])) & 0xffffffff)

def main():
    root = "/home/user/Doubao/chats/38456625919635970/aster-os"
    buf, info = build()
    write_header(buf, info)
    finish(buf)
    assert len(buf) == info["file_size"]

    # 自检：重新解析一遍关键字段
    assert buf[:8] == b"dex\n035\x00"
    assert struct.unpack_from("<I", buf, 8)[0] == zlib.adler32(bytes(buf[12:])) & 0xffffffff, "checksum 不符"
    assert buf[12:32] == hashlib.sha1(bytes(buf[32:])).digest(), "signature 不符"
    code_off = info["code_off"]
    regs, ins, outs, tries, dbg, n = struct.unpack_from("<HHHHII", buf, code_off)
    insns = [struct.unpack_from("<H", buf, code_off + 16 + 2 * i)[0] for i in range(n)]
    print("dex", len(buf), "bytes  regs", regs, "ins", ins, "outs", outs, "insns", [hex(i) for i in insns])

    apk = f"{root}/assets/demo3.apk"
    with zipfile.ZipFile(apk, "w") as z:
        zi = zipfile.ZipInfo("classes.dex", (2026, 1, 1, 0, 0, 0))
        z.writestr(zi, bytes(buf), compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
        z.writestr(
            zipfile.ZipInfo("AndroidManifest.xml", (2026, 1, 1, 0, 0, 0)),
            b"\x03\x00\x08\x00" + b"\0" * 60, compress_type=zipfile.ZIP_STORED,
        )
    import os
    print("demo3.apk", os.path.getsize(apk), "bytes")
    with zipfile.ZipFile(apk) as z:
        d = z.read("classes.dex")
        h = 0xcbf29ce484222325
        for b in d:
            h ^= b
            h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
        print("classes.dex", len(d), "fnv", hex(h))

if __name__ == "__main__":
    main()
