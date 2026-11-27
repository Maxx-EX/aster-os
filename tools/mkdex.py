#!/usr/bin/env python3
"""mkdex.py — 手搓一个最小但合法的 DEX（第二版），打进 demo3.apk。

两个方法，都在 Lcom/aster/Main; 里：

    static void hello() {
        Log.i("Hello from Aster, this string lives in a real DEX");
    }

    static void run() {
        int sum = 0;
        for (int i = 1; i <= 5; i++) sum += i;   // 1+2+3+4+5 = 15
        Log.n(sum);                              // 打印整数
        Log.i("loop done");
    }

字节码用到：const/4、const-string、if-gt、add-int、add-int/lit8、goto、
invoke-static、return-void —— 真的走循环、条件分支和算术，不只是打印一句话。

DEX 结构按规范逐字段拼：header / map_list / string_ids / type_ids / proto_ids /
method_ids / class_defs / data（string_data、type_list、class_data、code_item），
checksum（adler32）和 signature（sha1）最后回填。
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

STRINGS = [
    "Hello from Aster, this string lives in a real DEX",
    "I",
    "Lcom/aster/Log;",
    "Lcom/aster/Main;",
    "Ljava/lang/String;",
    "V",
    "VI",
    "VL",
    "hello",
    "i",
    "loop done",
    "n",
    "run",
]
assert STRINGS == sorted(STRINGS), "string_ids 必须升序"
(
    S_HELLO, S_I_T, S_LOG, S_MAIN, S_STRING, S_V, S_VI, S_VL,
    S_HELLO_M, S_I_M, S_LOOPDONE, S_N_M, S_RUN_M,
) = range(13)

# type_ids：0=String, 1=Main, 2=V, 3=Log, 4=I
T_STRING, T_MAIN, T_V, T_LOG, T_INT = 0, 1, 2, 3, 4
# proto_ids：0=(String)V, 1=()V, 2=(I)V
P_STR, P_VOID, P_INT = 0, 1, 2
# method_ids：0=Log->i, 1=Log->n, 2=Main->run, 3=Main->hello
M_I, M_N, M_RUN, M_HELLO = 0, 1, 2, 3


def build():
    buf = bytearray(0x70)
    n_map = 11
    map_off = 0x70
    string_ids_off = map_off + 4 + 12 * n_map
    buf += bytearray(b"\0" * (string_ids_off - 0x70))

    str_off_pos = []
    for _ in STRINGS:
        str_off_pos.append(len(buf))
        buf += b"\0\0\0\0"
    type_ids_off = len(buf)
    buf += b"".join(struct.pack("<I", t) for t in (S_STRING, S_MAIN, S_V, S_LOG, S_I_T))
    proto_ids_off = len(buf)
    buf += struct.pack("<III", S_VL, S_V, 0)
    buf += struct.pack("<III", S_V, S_V, 0)
    buf += struct.pack("<III", S_VI, S_V, 0)
    method_ids_off = len(buf)
    buf += struct.pack("<HHI", T_LOG, P_STR, S_I_M)
    buf += struct.pack("<HHI", T_LOG, P_INT, S_N_M)
    buf += struct.pack("<HHI", T_MAIN, P_VOID, S_RUN_M)
    buf += struct.pack("<HHI", T_MAIN, P_VOID, S_HELLO_M)
    class_defs_off = len(buf)
    class_def_pos = len(buf)
    buf += b"\0" * 32

    data_off = len(buf)

    str_offsets = []
    for s in STRINGS:
        str_offsets.append(len(buf))
        buf += uleb(len(s)) + s.encode("utf-8") + b"\0"

    tl_str = len(buf)
    buf += struct.pack("<IH", 1, T_STRING)
    buf += b"\0\0"
    tl_int = len(buf)
    buf += struct.pack("<IH", 1, T_INT)
    buf += b"\0\0"
    struct.pack_into("<I", buf, proto_ids_off + 8, tl_str)
    struct.pack_into("<I", buf, proto_ids_off + 24 + 8, tl_int)

    align(buf)

    # ---- run()V ----
    # 0  const/4 v0, 0         sum
    # 1  const/4 v1, 1         i
    # 2  const/4 v2, 5         上限
    # 3  if-gt v1, v2, +7      → 10（跳出循环）
    # 4  add-int v0, v0, v1
    # 5  add-int/lit8 v1, v1, #1
    # 9  goto -6               → 3
    # 5  add-int v0, v0, v1    (5,6)
    # 7  add-int/lit8 v1, v1   (7,8)
    # 10 invoke-static {v0}, Log->n   (10,11,12)
    # 13 const-string v3, "loop done" (13,14)
    # 15 invoke-static {v3}, Log->i   (15,16,17)
    # 18 return-void
    ins_run = [
        0x0012,
        0x1112,
        0x5212,
        0x2135, 0x0702,
        0x0090, 0x0100,
        0x01d8, 0x0101,
        0xfa28,
        0x1071, M_N, 0x0000,
        0x031a, S_LOOPDONE,
        0x1071, M_I, 0x0003,
        0x000e,
    ]
    assert len(ins_run) == 19, len(ins_run)
    code_run = len(buf)
    buf += struct.pack("<HHHHII", 4, 0, 2, 0, 0, len(ins_run))
    buf += b"".join(struct.pack("<H", i) for i in ins_run)

    # ---- hello()V ----
    ins_hello = [0x001a, S_HELLO, 0x1071, M_I, 0x0000, 0x000e]
    code_hello = len(buf)
    buf += struct.pack("<HHHHII", 1, 0, 1, 0, 0, len(ins_hello))
    buf += b"".join(struct.pack("<H", i) for i in ins_hello)

    # ---- class_data：2 direct methods ----
    class_data_off = len(buf)
    buf += uleb(0) + uleb(0) + uleb(2) + uleb(0)
    buf += uleb(M_RUN - 0) + uleb(0x0009) + uleb(code_run)
    buf += uleb(M_HELLO - M_RUN) + uleb(0x0009) + uleb(code_hello)

    for pos, off in zip(str_off_pos, str_offsets):
        struct.pack_into("<I", buf, pos, off)

    struct.pack_into(
        "<IIIIIIII", buf, class_def_pos,
        T_MAIN, 0x0001, 0xFFFFFFFF, 0, 0xFFFFFFFF, 0, class_data_off, 0
    )

    data_size = len(buf) - data_off

    items = [
        (0x0000, 1, 0),
        (0x0001, len(STRINGS), string_ids_off),
        (0x0002, 5, type_ids_off),
        (0x0003, 3, proto_ids_off),
        (0x0005, 4, method_ids_off),
        (0x0006, 1, class_defs_off),
        (0x1000, 1, map_off),
        (0x1001, 2, tl_str),
        (0x2000, 1, class_data_off),
        (0x2001, 2, code_run),
        (0x2002, len(STRINGS), str_offsets[0]),
    ]
    items.sort(key=lambda x: x[0])
    map_bytes = struct.pack("<I", len(items)) + b"".join(
        struct.pack("<HHII", t, 0, size, off) for t, size, off in items
    )
    buf[map_off:map_off + len(map_bytes)] = map_bytes

    return buf, dict(
        file_size=len(buf), map_off=map_off,
        string_ids=(len(STRINGS), string_ids_off),
        type_ids=(5, type_ids_off),
        proto_ids=(3, proto_ids_off),
        method_ids=(4, method_ids_off),
        class_defs=(1, class_defs_off),
        data=(data_size, data_off),
        code_run=code_run, code_hello=code_hello,
    )


def write_header(buf, info):
    buf[0:8] = b"dex\n035\x00"
    struct.pack_into("<I", buf, 8, 0)
    buf[12:32] = b"\0" * 20
    struct.pack_into("<I", buf, 32, info["file_size"])
    struct.pack_into("<I", buf, 36, 0x70)
    struct.pack_into("<I", buf, 40, 0x12345678)
    struct.pack_into("<II", buf, 44, 0, 0)
    struct.pack_into("<I", buf, 52, info["map_off"])
    for base, key in ((56, "string_ids"), (64, "type_ids"), (72, "proto_ids")):
        size, off = info[key]
        struct.pack_into("<II", buf, base, size, off)
    struct.pack_into("<II", buf, 80, 0, 0)
    for base, key in ((88, "method_ids"), (96, "class_defs"), (104, "data")):
        size, off = info[key]
        struct.pack_into("<II", buf, base, size, off)


def finish(buf):
    buf[12:32] = hashlib.sha1(bytes(buf[32:])).digest()
    struct.pack_into("<I", buf, 8, zlib.adler32(bytes(buf[12:])) & 0xffffffff)


def main():
    root = "/home/user/Doubao/chats/38456625919635970/aster-os"
    buf, info = build()
    write_header(buf, info)
    finish(buf)
    assert struct.unpack_from("<I", buf, 8)[0] == zlib.adler32(bytes(buf[12:])) & 0xffffffff
    assert buf[12:32] == hashlib.sha1(bytes(buf[32:])).digest()
    for name in ("code_run", "code_hello"):
        off = info[name]
        regs, ins, outs, tries, dbg, n = struct.unpack_from("<HHHHII", buf, off)
        insns = [struct.unpack_from("<H", buf, off + 16 + 2 * i)[0] for i in range(n)]
        print(f"{name}: regs={regs} ins={ins} outs={outs} n={n}", [hex(i) for i in insns])

    apk = f"{root}/assets/demo3.apk"
    with zipfile.ZipFile(apk, "w") as z:
        z.writestr(
            zipfile.ZipInfo("classes.dex", (2026, 1, 1, 0, 0, 0)),
            bytes(buf), compress_type=zipfile.ZIP_DEFLATED, compresslevel=9,
        )
        z.writestr(
            zipfile.ZipInfo("AndroidManifest.xml", (2026, 1, 1, 0, 0, 0)),
            b"\x03\x00\x08\x00" + b"\0" * 60, compress_type=zipfile.ZIP_STORED,
        )
    import os
    print("demo3.apk", os.path.getsize(apk), "bytes，classes.dex", len(buf), "字节")

if __name__ == "__main__":
    main()
