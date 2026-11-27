#!/usr/bin/env python3
"""mkdex.py — 手搓一个合法 DEX（第三版）：对象 + 字段 + 虚方法。

类与代码：

    class Counter {
        int v;
        Counter(int x) { v = x; }
        int next() { v = v + 1; return v; }
    }

    static void run() {
        Counter c = new Counter(10);
        Log.n(c.next());   // 11
        Log.n(c.next());   // 12
        Log.i("objects ok");
    }

用到的 opcode：new-instance、invoke-direct(<init>)、invoke-virtual、iget、iput、
move-result、const/4、const-string、add-int/lit8、invoke-static、return/return-void。
这要求解释器有对象堆、能递归调用方法、能把返回值塞回调用者的 move-result。

DEX 结构仍按规范逐字段拼（header / map_list / 全部 ids / class_defs / data），
checksum 与 signature 最后回填。
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
    "<init>",
    "Hello from Aster, this string lives in a real DEX",
    "I",
    "Lcom/aster/Counter;",
    "Lcom/aster/Log;",
    "Lcom/aster/Main;",
    "Ljava/lang/String;",
    "V",
    "VI",
    "VL",
    "[I",
    "array ok",
    "hello",
    "i",
    "loop done",
    "n",
    "next",
    "objects ok",
    "run",
    "v",
]
assert STRINGS == sorted(STRINGS), "string_ids 必须升序"
IDX = {s: i for i, s in enumerate(STRINGS)}
S_INIT, S_HELLO = IDX["<init>"], IDX["Hello from Aster, this string lives in a real DEX"]
S_I_T, S_COUNTER, S_LOG, S_MAIN = IDX["I"], IDX["Lcom/aster/Counter;"], IDX["Lcom/aster/Log;"], IDX["Lcom/aster/Main;"]
S_STRING, S_V, S_VI, S_VL = IDX["Ljava/lang/String;"], IDX["V"], IDX["VI"], IDX["VL"]
S_ARR, S_ARRAYOK = IDX["[I"], IDX["array ok"]
S_HELLO_M, S_I_M, S_LOOPDONE = IDX["hello"], IDX["i"], IDX["loop done"]
S_N_M, S_NEXT, S_OBJOK, S_RUN_M, S_V_F = IDX["n"], IDX["next"], IDX["objects ok"], IDX["run"], IDX["v"]

T_STRING, T_MAIN, T_V, T_LOG, T_INT, T_COUNTER, T_ARR = 0, 1, 2, 3, 4, 5, 6
# proto：0=(String)V 1=()V 2=(I)V 3=()I
P_STR, P_VOID, P_INT_V, P_INT_I = 0, 1, 2, 3
F_V = 0
# method：0=Log.i 1=Log.n 2=Counter.<init> 3=Counter.next 4=Main.run 5=Main.hello
M_I, M_N, M_INIT, M_NEXT, M_RUN, M_HELLO = 0, 1, 2, 3, 4, 5


def build():
    buf = bytearray(0x70)
    n_map = 12
    map_off = 0x70
    string_ids_off = map_off + 4 + 12 * n_map
    buf += bytearray(b"\0" * (string_ids_off - 0x70))

    str_off_pos = []
    for _ in STRINGS:
        str_off_pos.append(len(buf))
        buf += b"\0\0\0\0"

    type_ids_off = len(buf)
    for t in (S_STRING, S_MAIN, S_V, S_LOG, S_I_T, S_COUNTER, S_ARR):
        buf += struct.pack("<I", t)

    proto_ids_off = len(buf)
    buf += struct.pack("<III", S_VL, S_V, 0)
    buf += struct.pack("<III", S_V, S_V, 0)
    buf += struct.pack("<III", S_VI, S_V, 0)
    buf += struct.pack("<III", S_I_T, S_I_T, 0)

    field_ids_off = len(buf)
    buf += struct.pack("<HHI", T_COUNTER, T_INT, S_V_F)

    method_ids_off = len(buf)
    buf += struct.pack("<HHI", T_LOG, P_STR, S_I_M)
    buf += struct.pack("<HHI", T_LOG, P_INT_V, S_N_M)
    buf += struct.pack("<HHI", T_COUNTER, P_INT_V, S_INIT)
    buf += struct.pack("<HHI", T_COUNTER, P_INT_I, S_NEXT)
    buf += struct.pack("<HHI", T_MAIN, P_VOID, S_RUN_M)
    buf += struct.pack("<HHI", T_MAIN, P_VOID, S_HELLO_M)

    class_defs_off = len(buf)
    main_def_pos = len(buf)
    buf += b"\0" * 32
    counter_def_pos = len(buf)
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

    # Counter.<init>(I)V   regs=2 ins=1
    #   iput v1, v0, field@0
    #   return-void
    code_init = len(buf)
    ins = [0x0159, F_V, 0x000e]
    # 实例方法的 ins_size 含 this：这里 this + int = 2
    buf += struct.pack("<HHHHII", 2, 2, 0, 0, 0, len(ins))
    buf += b"".join(struct.pack("<H", i) for i in ins)

    # Counter.next()I    regs=2 ins=1
    #   iget v0, v1, field@0
    #   add-int/lit8 v0, v0, #1
    #   iput v0, v1, field@0
    #   return v0
    code_next = len(buf)
    # 22c：B 在 unit0 高 4 位，unit1 整 16 位都是字段索引
    ins = [0x1052, F_V, 0x00d8, 0x0001, 0x1059, F_V, 0x000f]
    buf += struct.pack("<HHHHII", 2, 1, 0, 0, 0, len(ins))
    buf += b"".join(struct.pack("<H", i) for i in ins)

    # Main.run()V   regs=6 ins=0 outs=2   —— APK-6：数组
    # 0  const/4 v0, #5                  环长
    # 1  new-array v1, v0, [I           建一个 5 元素 int 数组
    # 3  const/4 v2, #0                  i = 0
    # 4  const/4 v3, #0                  sum = 0
    # 5  const/4 v5, #3                  常数 3
    # 6  array-length v4, v1
    # 7  if-ge v2, v4 -> 20              循环出口
    # 9  mul-int v4, v2, v5              a[i] = i*3
    # 11 aput v4, v1, v2
    # 13 aget v4, v1, v2
    # 15 add-int v3, v3, v4             sum += a[i]
    # 17 add-int/lit8 v2, v2, #1
    # 19 goto 6
    # 20 invoke-static {v3}, Log.n
    # 23 const-string v0, "array ok"
    # 25 invoke-static {v0}, Log.i
    # 28 return-void
    code_run = len(buf)
    ins = [
        0x5012,                  # const/4 v0, #5
        0x0123, T_ARR,           # new-array v1, v0, [I
        0x0212,                  # const/4 v2, #0
        0x0312,                  # const/4 v3, #0
        0x3512,                  # const/4 v5, #3（lit 在高 4 位、A 在 bits8-11）
        0x1421,                  # array-length v4, v1
        0x4234, 0x0D00,          # if-ge v2, v4 -> +13（end 在 pc 20）
        0x0492, 0x0205,          # mul-int v4, v2, v5（unit1 = B<<8 | C）
        0x044b, 0x0201,          # aput v4, v1, v2
        0x0444, 0x0201,          # aget v4, v1, v2
        0x0390, 0x0403,          # add-int v3, v3, v4（0x90 是 add，0x93 是 div）
        0x02d8, 0x0201,          # add-int/lit8 v2, v2, #1（22b：B 在高字节、lit 在低字节）
        0xF328,                  # goto -13 -> 6
        0x1071, M_N, 0x0003,     # Log.n(v3)
        0x001a, S_ARRAYOK,       # const-string v0, "array ok"
        0x1071, M_I, 0x0000,     # Log.i(v0)
        0x000e,                  # return-void
    ]
    assert len(ins) == 29, len(ins)
    buf += struct.pack("<HHHHII", 6, 0, 2, 0, 0, len(ins))
    buf += b"".join(struct.pack("<H", i) for i in ins)

    # Main.hello()V  regs=1 ins=0 outs=1
    code_hello = len(buf)
    ins = [0x001a, S_HELLO, 0x1071, M_I, 0x0000, 0x000e]
    buf += struct.pack("<HHHHII", 1, 0, 1, 0, 0, len(ins))
    buf += b"".join(struct.pack("<H", i) for i in ins)

    # Counter 的 class_data：1 个实例字段，1 个 direct，1 个 virtual
    cd_counter = len(buf)
    buf += uleb(0) + uleb(1) + uleb(1) + uleb(1)
    buf += uleb(F_V - 0) + uleb(0x0002)          # 字段 v：private int
    buf += uleb(M_INIT - 0) + uleb(0x10001) + uleb(code_init)
    # virtual methods 的 method_idx_diff 也从 0 开始累加，不是接着 direct 的
    buf += uleb(M_NEXT - 0) + uleb(0x0001) + uleb(code_next)

    # Main 的 class_data：2 个 direct
    cd_main = len(buf)
    buf += uleb(0) + uleb(0) + uleb(2) + uleb(0)
    buf += uleb(M_RUN - 0) + uleb(0x0009) + uleb(code_run)
    buf += uleb(M_HELLO - M_RUN) + uleb(0x0009) + uleb(code_hello)

    for pos, off in zip(str_off_pos, str_offsets):
        struct.pack_into("<I", buf, pos, off)

    struct.pack_into("<IIIIIIII", buf, main_def_pos,
                     T_MAIN, 0x0001, 0xFFFFFFFF, 0, 0xFFFFFFFF, 0, cd_main, 0)
    struct.pack_into("<IIIIIIII", buf, counter_def_pos,
                     T_COUNTER, 0x0001, 0xFFFFFFFF, 0, 0xFFFFFFFF, 0, cd_counter, 0)

    data_size = len(buf) - data_off

    items = [
        (0x0000, 1, 0),
        (0x0001, len(STRINGS), string_ids_off),
        (0x0002, 7, type_ids_off),
        (0x0003, 4, proto_ids_off),
        (0x0004, 1, field_ids_off),
        (0x0005, 6, method_ids_off),
        (0x0006, 2, class_defs_off),
        (0x1000, 1, map_off),
        (0x1001, 2, tl_str),
        (0x2000, 2, cd_counter),
        (0x2001, 4, code_init),
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
        type_ids=(7, type_ids_off),
        proto_ids=(4, proto_ids_off),
        field_ids=(1, field_ids_off),
        method_ids=(6, method_ids_off),
        class_defs=(2, class_defs_off),
        data=(data_size, data_off),
        code_init=code_init, code_next=code_next,
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
    for base, key in ((80, "field_ids"), (88, "method_ids"), (96, "class_defs"), (104, "data")):
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
    for name in ("code_init", "code_next", "code_run", "code_hello"):
        off = info[name]
        regs, ins_n, outs, tries, dbg, n = struct.unpack_from("<HHHHII", buf, off)
        insns = [struct.unpack_from("<H", buf, off + 16 + 2 * i)[0] for i in range(n)]
        print(f"{name}: regs={regs} ins={ins_n} outs={outs} n={n}")
        print("   ", [hex(i) for i in insns])

    apk = f"{root}/assets/demo3.apk"
    with zipfile.ZipFile(apk, "w") as z:
        z.writestr(zipfile.ZipInfo("classes.dex", (2026, 1, 1, 0, 0, 0)),
                   bytes(buf), compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
        z.writestr(zipfile.ZipInfo("AndroidManifest.xml", (2026, 1, 1, 0, 0, 0)),
                   b"\x03\x00\x08\x00" + b"\0" * 60, compress_type=zipfile.ZIP_STORED)
    import os
    print("demo3.apk", os.path.getsize(apk), "bytes，classes.dex", len(buf), "字节")


if __name__ == "__main__":
    main()
