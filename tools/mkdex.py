#!/usr/bin/env python3
"""mkdex.py — 手搓一个合法 DEX（第八版）：画布绘制 + 触摸回传。

类与代码：

    class Counter {
        int v;
        Counter(int x) { v = x; }
        int next() { v = v + 1; return v; }
    }

    class Base { int v; Base(int x){v=x;} int get(){return v;} }
    class Child extends Base { Child(int x){ super(x); } }

    static void run() {
        int[] a = new int[5];
        for (int i = 0; i < 5; i++) { a[i] = i * 3; sum += a[i]; }
        Log.n(sum);                     // 30
        hits = hits + 1;                // sget / sput
        StringBuilder sb = new StringBuilder();
        sb.append("sum="); sb.append(sum);
        sb.append(" hits="); sb.append(hits);
        Log.i(sb.toString());           // sum=30 hits=1
        Child c = new Child(7);
        Log.n(c.get());                 // 7，方法解析上溯到 Base
        try { throw new RuntimeException(); } catch (Throwable t) { Log.i("caught"); }
        Log.i("apk8 ok");
    }

用到的 opcode：new-instance、new-array、array-length、aget、aput、iget、iput、sget、sput、
invoke-direct(<init>)、invoke-virtual、invoke-static、invoke-super、move-result、move-exception、
throw、const/4、const-string、mul-int、add-int、add-int/lit8、if-ge、goto、return/return-void。
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
    " hits=",
    "<init>",
    "Aster APK",
    "Base",
    "Boom",
    "Child",
    "Hello from Aster, this string lives in a real DEX",
    "I",
    "L",
    "LI",
    "LL",
    "Lcom/aster/Base;",
    "Lcom/aster/Boom;",
    "Lcom/aster/Canvas;",
    "Lcom/aster/Child;",
    "Lcom/aster/Counter;",
    "Lcom/aster/Log;",
    "Lcom/aster/Main;",
    "Ljava/lang/RuntimeException;",
    "Ljava/lang/String;",
    "Ljava/lang/StringBuilder;",
    "RuntimeException",
    "V",
    "VI",
    "VIIII",
    "VL",
    "VLII",
    "[I",
    "apk8 ok",
    "append",
    "array ok",
    "canvas ok",
    "caught",
    "caught local",
    "caught xmethod",
    "clear",
    "cvh",
    "drawRect",
    "drawText",
    "frame",
    "get",
    "height",
    "hello",
    "hits",
    "i",
    "loop done",
    "moveX",
    "moveY",
    "n",
    "next",
    "no touch",
    "objects ok",
    "px",
    "py",
    "run",
    "setColor",
    "sum=",
    "toString",
    "touch hit",
    "touchX",
    "touchY",
    "v",
    "width",
]
assert STRINGS == sorted(STRINGS), "string_ids 必须升序"
IDX = {s: i for i, s in enumerate(STRINGS)}
S_INIT, S_HELLO = IDX["<init>"], IDX["Hello from Aster, this string lives in a real DEX"]
S_I_T, S_COUNTER, S_LOG, S_MAIN = IDX["I"], IDX["Lcom/aster/Counter;"], IDX["Lcom/aster/Log;"], IDX["Lcom/aster/Main;"]
S_STRING, S_V, S_VI, S_VL = IDX["Ljava/lang/String;"], IDX["V"], IDX["VI"], IDX["VL"]
S_ARR, S_ARRAYOK = IDX["[I"], IDX["array ok"]
S_SB, S_APPEND, S_STRM, S_SUM, S_HITS_S, S_TOSTR = (
    IDX["Ljava/lang/StringBuilder;"], IDX["append"], IDX["toString"],
    IDX["sum="], IDX[" hits="], IDX["toString"])
S_HITS_F = IDX["hits"]
S_L, S_LI, S_LL = IDX["L"], IDX["LI"], IDX["LL"]
S_VIIII, S_VLII = IDX["VIIII"], IDX["VLII"]
S_BASE, S_CHILD, S_RTE = IDX["Lcom/aster/Base;"], IDX["Lcom/aster/Child;"], IDX["Ljava/lang/RuntimeException;"]
S_GET, S_CAUGHT, S_APK8 = IDX["get"], IDX["caught local"], IDX["apk8 ok"]
S_BOOM, S_CAUGHTX = IDX["Lcom/aster/Boom;"], IDX["caught xmethod"]
S_CV = IDX["Lcom/aster/Canvas;"]
S_APKTITLE, S_CANVASOK = IDX["Aster APK"], IDX["canvas ok"]
S_CLEAR, S_SETCOLOR, S_DRECT, S_DTEXT = IDX["clear"], IDX["setColor"], IDX["drawRect"], IDX["drawText"]
S_TOUCHHIT, S_NOTOUCH = IDX["touch hit"], IDX["no touch"]
S_TOUCHX, S_TOUCHY = IDX["touchX"], IDX["touchY"]
S_CVH_F, S_PX_F, S_PY_F = IDX["cvh"], IDX["px"], IDX["py"]
S_FRAME = IDX["frame"]
S_MVX, S_MVY, S_W, S_H = IDX["moveX"], IDX["moveY"], IDX["width"], IDX["height"]
S_HELLO_M, S_I_M, S_LOOPDONE = IDX["hello"], IDX["i"], IDX["loop done"]
S_N_M, S_NEXT, S_OBJOK, S_RUN_M, S_V_F = IDX["n"], IDX["next"], IDX["objects ok"], IDX["run"], IDX["v"]

T_STRING, T_MAIN, T_V, T_LOG, T_INT, T_COUNTER, T_ARR, T_SB = 0, 1, 2, 3, 4, 5, 6, 7
T_BASE, T_CHILD, T_RTE, T_BOOM, T_CV = 8, 9, 10, 11, 12
# proto：0=(String)V 1=()V 2=(I)V 3=()I 4=(String)SB 5=(I)SB 6=()SB
P_STR, P_VOID, P_INT_V, P_INT_I = 0, 1, 2, 3
P_APP_STR, P_APP_INT, P_SB_VOID = 4, 5, 6
P_DRECT, P_DTEXT = 7, 8
# 字段表顺序（同 class 内 name 升序）：cvh/hits/px/py 都在 Main 上
F_V, F_CVH, F_HITS, F_PX, F_PY, F_BASE_V = 0, 1, 2, 3, 4, 5
# method：0=Log.i 1=Log.n 2=Counter.<init> 3=Counter.next 4=Main.run 5=Main.hello
#        6=SB.<init> 7=SB.append(String) 8=SB.append(I) 9=SB.toString
M_I, M_N, M_INIT, M_NEXT, M_RUN, M_HELLO = 0, 1, 2, 3, 4, 5
M_SB_INIT, M_SB_APP_S, M_SB_APP_I, M_SB_STR = 6, 7, 8, 9
M_BASE_INIT, M_BASE_GET, M_CHILD_INIT, M_BOOM_INIT = 10, 11, 12, 13
M_CV_INIT, M_CV_CLEAR, M_CV_COLOR, M_CV_RECT, M_CV_TEXT = 14, 15, 16, 17, 18
M_CV_TX, M_CV_TY = 19, 20
M_FRAME = 21
M_CV_MVX, M_CV_MVY, M_CV_W, M_CV_H = 22, 23, 24, 25


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
    for t in (S_STRING, S_MAIN, S_V, S_LOG, S_I_T, S_COUNTER, S_ARR, S_SB,
              S_BASE, S_BOOM, S_CHILD, S_RTE, S_CV):
        buf += struct.pack("<I", t)

    proto_ids_off = len(buf)
    buf += struct.pack("<III", S_VL, T_V, 0)
    buf += struct.pack("<III", S_V, T_V, 0)
    buf += struct.pack("<III", S_VI, T_V, 0)
    buf += struct.pack("<III", S_I_T, T_INT, 0)
    buf += struct.pack("<III", S_LL, T_SB, 0)     # 4: (String)SB，params_size 稍后填
    buf += struct.pack("<III", S_LI, T_SB, 0)     # 5: (I)SB
    buf += struct.pack("<III", S_L, T_SB, 0)      # 6: ()SB
    buf += struct.pack("<III", S_VIIII, T_V, 0)   # 7: (IIII)V
    buf += struct.pack("<III", S_VLII, T_V, 0)    # 8: (String,II)V

    field_ids_off = len(buf)
    buf += struct.pack("<HHI", T_COUNTER, T_INT, S_V_F)   # 0 Counter.v
    buf += struct.pack("<HHI", T_MAIN, T_INT, S_CVH_F)     # 1 Main.cvh（Canvas 句柄）
    buf += struct.pack("<HHI", T_MAIN, T_INT, S_HITS_F)    # 2 Main.hits
    buf += struct.pack("<HHI", T_MAIN, T_INT, S_PX_F)      # 3 Main.px
    buf += struct.pack("<HHI", T_MAIN, T_INT, S_PY_F)      # 4 Main.py
    buf += struct.pack("<HHI", T_BASE, T_INT, S_V_F)       # 5 Base.v

    method_ids_off = len(buf)
    buf += struct.pack("<HHI", T_LOG, P_STR, S_I_M)
    buf += struct.pack("<HHI", T_LOG, P_INT_V, S_N_M)
    buf += struct.pack("<HHI", T_COUNTER, P_INT_V, S_INIT)
    buf += struct.pack("<HHI", T_COUNTER, P_INT_I, S_NEXT)
    buf += struct.pack("<HHI", T_MAIN, P_VOID, S_RUN_M)
    buf += struct.pack("<HHI", T_MAIN, P_VOID, S_HELLO_M)
    buf += struct.pack("<HHI", T_SB, P_SB_VOID, S_INIT)        # 6 <init>()V
    buf += struct.pack("<HHI", T_SB, P_APP_STR, S_APPEND)      # 7 append(String)SB
    buf += struct.pack("<HHI", T_SB, P_APP_INT, S_APPEND)      # 8 append(I)SB
    buf += struct.pack("<HHI", T_SB, P_SB_VOID, S_TOSTR)       # 9 toString()SB
    buf += struct.pack("<HHI", T_BASE, P_INT_V, S_INIT)        # 10 Base.<init>(I)V
    buf += struct.pack("<HHI", T_BASE, P_INT_I, S_GET)         # 11 Base.get()I
    buf += struct.pack("<HHI", T_CHILD, P_INT_V, S_INIT)       # 12 Child.<init>(I)V
    buf += struct.pack("<HHI", T_BOOM, P_VOID, S_INIT)         # 13 Boom.<init>()V（直接抛）
    buf += struct.pack("<HHI", T_CV, P_VOID, S_INIT)           # 14 Canvas.<init>()V
    buf += struct.pack("<HHI", T_CV, P_INT_V, S_CLEAR)         # 15 clear(I)V
    buf += struct.pack("<HHI", T_CV, P_INT_V, S_SETCOLOR)      # 16 setColor(I)V
    buf += struct.pack("<HHI", T_CV, P_DRECT, S_DRECT)         # 17 drawRect(IIII)V
    buf += struct.pack("<HHI", T_CV, P_DTEXT, S_DTEXT)         # 18 drawText(String,II)V
    buf += struct.pack("<HHI", T_CV, P_INT_I, S_TOUCHX)        # 19 touchX()I
    buf += struct.pack("<HHI", T_CV, P_INT_I, S_TOUCHY)        # 20 touchY()I
    buf += struct.pack("<HHI", T_MAIN, P_VOID, S_FRAME)        # 21 Main.frame()V
    buf += struct.pack("<HHI", T_CV, P_INT_I, S_MVX)          # 22 moveX()I
    buf += struct.pack("<HHI", T_CV, P_INT_I, S_MVY)          # 23 moveY()I
    buf += struct.pack("<HHI", T_CV, P_INT_I, S_W)            # 24 width()I
    buf += struct.pack("<HHI", T_CV, P_INT_I, S_H)            # 25 height()I

    class_defs_off = len(buf)
    main_def_pos = len(buf)
    buf += b"\0" * 32
    counter_def_pos = len(buf)
    buf += b"\0" * 32
    base_def_pos = len(buf)
    buf += b"\0" * 32
    child_def_pos = len(buf)
    buf += b"\0" * 32
    rte_def_pos = len(buf)
    buf += b"\0" * 32
    boom_def_pos = len(buf)
    buf += b"\0" * 32
    cv_def_pos = len(buf)
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
    tl_sb_str = len(buf)
    buf += struct.pack("<IH", 1, T_STRING)
    buf += b"\0\0"
    tl_sb_int = len(buf)
    buf += struct.pack("<IH", 1, T_INT)
    buf += b"\0\0"
    struct.pack_into("<I", buf, proto_ids_off + 48 + 8, tl_sb_str)
    struct.pack_into("<I", buf, proto_ids_off + 60 + 8, tl_sb_int)
    tl_4i = len(buf)
    buf += struct.pack("<IH", 4, T_INT) + b"\0\0" * 4
    tl_s2i = len(buf)
    buf += struct.pack("<IH", 3, T_STRING) + b"\0\0"
    buf += struct.pack("<IH", 2, T_INT) + b"\0\0"
    buf += struct.pack("<IH", 3, T_INT) + b"\0\0"
    struct.pack_into("<I", buf, proto_ids_off + 84 + 8, tl_4i)
    struct.pack_into("<I", buf, proto_ids_off + 96 + 8, tl_s2i)

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
    # 28 sget v7, hits                  （APK-7 从这里接着排）
    # 30 add-int/lit8 v7, v7, #1
    # 32 sput v7, hits
    # 34 new-instance v6, StringBuilder
    # 36 invoke-direct {v6}, <init>()V
    # 39 const-string v0, "sum="
    # 41 invoke-virtual {v6, v0}, append(String)
    # 44 invoke-virtual {v6, v3}, append(I)
    # 47 const-string v0, " hits="
    # 49 invoke-virtual {v6, v0}, append(String)
    # 52 invoke-virtual {v6, v7}, append(I)
    # 55 invoke-virtual {v6}, toString()
    # 58 move-result v0
    # 59 invoke-static {v0}, Log.i
    # 62 return-void
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
    ]

    # —— APK-7：静态字段 + StringBuilder ——
    ins += [
        0x0760, F_HITS,          # sget v7, hits（21c：AA 8bit + field 16bit）
        0x07d8, 0x0701,          # add-int/lit8 v7, v7, #1
        0x0767, F_HITS,          # sput v7, hits
        0x0622, T_SB,            # new-instance v6, StringBuilder
        0x1070, M_SB_INIT, 0x0006,   # invoke-direct {v6}, <init>()V
        0x001a, S_SUM,           # const-string v0, "sum="
        0x206e, M_SB_APP_S, 0x0006,  # invoke-virtual {v6,v0}, append(String)
        0x206e, M_SB_APP_I, 0x0036,  # invoke-virtual {v6,v3}, append(I)
        0x001a, S_HITS_S,        # const-string v0, " hits="
        0x206e, M_SB_APP_S, 0x0006,  # append(String)
        0x206e, M_SB_APP_I, 0x0076,  # invoke-virtual {v6,v7}, append(I)
        0x106e, M_SB_STR, 0x0006,    # invoke-virtual {v6}, toString()
        0x000a,                  # move-result v0
        0x1071, M_I, 0x0000,     # Log.i(v0)
        # —— APK-8：继承 + 跨方法 throw + 本方法 throw ——
        0x7512,                  # const/4 v5, #7（const/4 只装 -8..7）
        0x0622, T_CHILD,         # new-instance v6, Child
        0x2070, M_CHILD_INIT, 0x0056,  # invoke-direct {v6,v5}, Child.<init>(I)V
        0x106e, M_BASE_GET, 0x0006,    # invoke-virtual {v6}, Base.get()I（解析上溯）
        0x000a,                  # move-result v0
        0x1071, M_N, 0x0000,     # Log.n(v0) -> 7
        # try#1 覆盖 [75,80)：Boom.<init> 在子方法里抛
        0x0722, T_BOOM,          # new-instance v7, Boom
        0x1070, M_BOOM_INIT, 0x0007,   # invoke-direct {v7}, Boom.<init>()V（在这里抛）
        # handler#1（pc 80）
        0x000d,                  # move-exception v0
        0x001a, S_CAUGHTX,       # const-string v0, "caught xmethod"
        0x1071, M_I, 0x0000,     # Log.i(v0)
        # try#2 覆盖 [86,89)：本方法直接 throw
        0x0722, T_RTE,           # new-instance v7, RuntimeException
        0x0727,                  # throw v7
        # handler#2（pc 89）
        0x000d,                  # move-exception v0
        0x001a, S_CAUGHT,        # const-string v0, "caught local"
        0x1071, M_I, 0x0000,     # Log.i(v0)
        0x001a, S_APK8,          # const-string v0, "apk8 ok"
        0x1071, M_I, 0x0000,     # Log.i(v0)
        # —— APK-9：画布（pc 100 起）——
        0x0622, T_CV,            # new-instance v6, Canvas
        0x1070, M_CV_INIT, 0x0006,   # invoke-direct {v6}, <init>()V
        0x0514, 0x1418, 0x0010,  # const v5, #0x00101418（31i：AA<<8|0x14）
        0x206e, M_CV_CLEAR, 0x0056,  # clear(v5)：argc=2（含 this）
        0x0514, 0x99ff, 0x005b,  # const v5, #0x005b99ff
        0x206e, M_CV_COLOR, 0x0056,  # setColor(v5)
        0x0313, 0x0014,          # const/16 v3, #20
        0x0413, 0x0014,          # const/16 v4, #20
        0x0513, 0x003c,          # const/16 v5, #60
        0x0713, 0x0008,          # const/16 v7, #8
        0x576e, M_CV_RECT, 0x5436,   # drawRect {v6,v3,v4,v5,v7}
        0x0514, 0xdde2, 0x00da,  # const v5, #0x00dadde2
        0x206e, M_CV_COLOR, 0x0056,  # setColor
        0x001a, S_APKTITLE,      # const-string v0, "Aster APK"
        0x0313, 0x0014,          # const/16 v3, #20
        0x0413, 0x0030,          # const/16 v4, #48
        0x406e, M_CV_TEXT, 0x4306,   # drawText {v6,v0,v3,v4}
        0x0514, 0xc46a, 0x0046,  # const v5, #0x0046c46a
        0x206e, M_CV_COLOR, 0x0056,  # setColor
        0x0313, 0x0014,          # const/16 v3, #20
        0x0413, 0x0050,          # const/16 v4, #80
        0x0513, 0x0028,          # const/16 v5, #40
        0x0713, 0x0028,          # const/16 v7, #40
        0x576e, M_CV_RECT, 0x5436,   # drawRect
        0x0313, 0x0046,          # const/16 v3, #70
        0x576e, M_CV_RECT, 0x5436,   # drawRect
        0x0313, 0x0078,          # const/16 v3, #120
        0x576e, M_CV_RECT, 0x5436,   # drawRect
        0x001a, S_CANVASOK,      # const-string v0, "canvas ok"
        0x1071, M_I, 0x0000,     # Log.i(v0)
        # —— APK-10：触摸（pc 176 起）——
        0x0212,                  # const/4 v2, #0
        0x106e, M_CV_TX, 0x0006, # invoke-virtual {v6}, touchX()I
        0x000a,                  # move-result v0
        0x2033, 0x1E00,          # if-lt v0, v2 -> +30（跳到 no-touch 211）
        0x4412,                  # const/4 v4, #4
        0x0591, 0x0004,          # sub-int v5, v0, v4（高=BB0 低=CC4）
        0x106e, M_CV_TY, 0x0006, # invoke-virtual {v6}, touchY()I
        0x030a,                  # move-result v3
        0x0391, 0x0304,          # sub-int v3, v3, v4（高=BB3 低=CC4）
        0x0214, 0xa441, 0x00d9,  # const v2, #0x00d9a441（WARN 色）
        0x206e, M_CV_COLOR, 0x0026,  # setColor(v2)
        0x0713, 0x0008,          # const/16 v7, #8
        0x0113, 0x0008,          # const/16 v1, #8
        0x516e, M_CV_RECT, 0x7356,   # drawRect {v6,v5,v3,v7,v1}
        0x001a, S_TOUCHHIT,      # const-string v0, "touch hit"
        0x1071, M_I, 0x0000,     # Log.i(v0)
        0x0628,                  # goto +6（跳到 216 return）
        # no-touch（pc 211）
        0x001a, S_NOTOUCH,       # const-string v0, "no touch"
        0x1071, M_I, 0x0000,     # Log.i(v0)
        0x000e,                  # return-void
    ]
    assert len(ins) == 216, len(ins)
    # tries_size=1：throw 在 pc 77，try 覆盖 [75, 78)
    buf += struct.pack("<HHHHII", 8, 0, 5, 2, 0, len(ins))
    buf += b"".join(struct.pack("<H", i) for i in ins)
    if len(ins) % 2 == 1:
        buf += b"\0\0"                       # insns 奇数条时补到 4 字节对齐
    # try#1：Boom 构造器（76..81），try#2：本地 throw（87..90）
    buf += struct.pack("<IHH", 75, 5, 0)
    buf += struct.pack("<IHH", 86, 3, 2)
    buf += b"\x00" + uleb(80)                  # handler#1：catch-all -> 80
    buf += b"\x00" + uleb(89)                  # handler#2：catch-all -> 89

    # Main.hello()V  regs=1 ins=0 outs=1
    code_hello = len(buf)
    ins = [0x001a, S_HELLO, 0x1071, M_I, 0x0000, 0x000e]
    buf += struct.pack("<HHHHII", 1, 0, 1, 0, 0, len(ins))
    buf += b"".join(struct.pack("<H", i) for i in ins)

    # Base.<init>(I)V  regs=2 ins=2
    code_base_init = len(buf)
    ins = [0x0159, F_BASE_V, 0x000e]
    buf += struct.pack("<HHHHII", 2, 2, 0, 0, 0, len(ins))
    buf += b"".join(struct.pack("<H", i) for i in ins)

    # Base.get()I  regs=2 ins=1（this 在 v1）
    code_base_get = len(buf)
    ins = [0x1052, F_BASE_V, 0x000f]
    buf += struct.pack("<HHHHII", 2, 1, 0, 0, 0, len(ins))
    buf += b"".join(struct.pack("<H", i) for i in ins)

    # Child.<init>(I)V  regs=2 ins=2：invoke-super {v0,v1}, Base.<init>(I)V
    code_child_init = len(buf)
    ins = [0x206f, M_BASE_INIT, 0x0010, 0x000e]
    buf += struct.pack("<HHHHII", 2, 2, 0, 0, 0, len(ins))
    buf += b"".join(struct.pack("<H", i) for i in ins)

    # Boom.<init>()V  regs=2 ins=1：造一个 RuntimeException 立刻抛
    code_boom_init = len(buf)
    ins = [0x0022, T_RTE, 0x0027]
    buf += struct.pack("<HHHHII", 2, 1, 0, 0, 0, len(ins))
    buf += b"".join(struct.pack("<H", i) for i in ins)

    # Main.frame()V —— APK-11：一帧一画，静态字段跨帧保留
    #   if (cvh == 0) cvh = new Canvas();
    #   c.clear(0x101418); setColor; drawRect(16,16,40,4);
    #   drawText("Aster APK",16,36); setColor(0x46c46a); drawRect(px,py,32,32);
    #   "拖动/点击都把方块跟到手指位置"
    code_frame = len(buf)
    ins = [
        0x0060, F_CVH,               # 0  sget v0, cvh
        0x0038, 0x0700,              # 2  if-eqz v0, +7 -> 9
        0x0022, T_CV,                # 4  new-instance v0, Canvas
        0x1070, M_CV_INIT, 0x0000,   # 6  invoke-direct {v0}, <init>()V
        0x0067, F_CVH,               # 9  sput cvh, v0
        0x0660, F_CVH,               # 11 sget v6, cvh
        0x0114, 0x1418, 0x0010,      # 13 const v1, #0x101418
        0x206e, M_CV_CLEAR, 0x0016,  # 16 clear(v1)
        0x0114, 0x99ff, 0x005b,      # 19 const v1, #0x5b99ff
        0x206e, M_CV_COLOR, 0x0016,  # 22 setColor(v1)
        0x0113, 0x0010,              # 25 const/16 v1, #16
        0x0313, 0x0010,              # 27 const/16 v3, #16
        0x0213, 0x0028,              # 29 const/16 v2, #40
        0x0713, 0x0004,              # 31 const/16 v7, #4
        0x576e, M_CV_RECT, 0x2316,   # 33 drawRect(v1,v3,v2,v7)
        0x0114, 0xdde2, 0x00da,      # 36 const v1, #0xdadde2（标题用灰白）
        0x206e, M_CV_COLOR, 0x0016,  # 39 setColor(v1)
        0x021a, S_APKTITLE,          # 42 const-string v2, "Aster APK"
        0x0113, 0x0010,              # 38 const/16 v1, #16
        0x0313, 0x0024,              # 40 const/16 v3, #36
        0x406e, M_CV_TEXT, 0x3126,   # 42 drawText(v2,v1,v3)
        0x0114, 0xc46a, 0x0046,      # 45 const v1, #0x46c46a
        0x206e, M_CV_COLOR, 0x0016,  # 48 setColor(v1)
        0x0760, F_PX,                # 51 sget v7, px
        0x0360, F_PY,                # 53 sget v3, py
        0x0113, 0x0020,              # 55 const/16 v1, #32
        0x0213, 0x0020,              # 57 const/16 v2, #32
        0x526e, M_CV_RECT, 0x1376,   # 59 drawRect(v7,v3,v1,v2)
        0x106e, M_CV_MVX, 0x0006,    # 62 moveX()
        0x000a,                      # 65 move-result v0
        0x0212,                      # 66 const/4 v2, #0
        0x2033, 0x0E00,              # 67 if-lt v0, v2 -> +14（跳到 81）
        0x03d8, 0x00F0,              # 69 add-int/lit8 v3, v0, #-16
        0x0367, F_PX,                # 71 sput px, v3
        0x106e, M_CV_MVY, 0x0006,    # 73 moveY()
        0x000a,                      # 76 move-result v0
        0x03d8, 0x00F0,              # 77 add-int/lit8 v3, v0, #-16
        0x0367, F_PY,                # 79 sput py, v3
        0x106e, M_CV_TX, 0x0006,     # 81 touchX()
        0x000a,                      # 84 move-result v0
        0x0212,                      # 85 const/4 v2, #0
        0x2033, 0x0E00,              # 86 if-lt v0, v2 -> +14（跳到 100）
        0x03d8, 0x00F0,              # 88 add-int/lit8 v3, v0, #-16
        0x0367, F_PX,                # 90 sput px, v3
        0x106e, M_CV_TY, 0x0006,     # 92 touchY()
        0x000a,                      # 95 move-result v0
        0x03d8, 0x00F0,              # 96 add-int/lit8 v3, v0, #-16
        0x0367, F_PY,                # 98 sput py, v3
        0x000e,                      # 100 return-void
    ]
    assert len(ins) == 107, len(ins)
    buf += struct.pack("<HHHHII", 8, 0, 0, 0, 0, len(ins))
    buf += b"".join(struct.pack("<H", i) for i in ins)

    # Main 的 static_values：cvh=0, hits=1, px=140, py=80
    sv_main = len(buf)
    buf += uleb(4) + b"\x04\x00" + b"\x04\x01" + b"\x24\x8c\x00" + b"\x04\x50"

    # Counter 的 class_data：1 个实例字段，1 个 direct，1 个 virtual
    cd_counter = len(buf)
    buf += uleb(0) + uleb(1) + uleb(1) + uleb(1)
    buf += uleb(F_V - 0) + uleb(0x0002)          # 字段 v：private int
    buf += uleb(M_INIT - 0) + uleb(0x10001) + uleb(code_init)
    # virtual methods 的 method_idx_diff 也从 0 开始累加，不是接着 direct 的
    buf += uleb(M_NEXT - 0) + uleb(0x0001) + uleb(code_next)

    # Main 的 class_data：4 个静态字段 + 3 个 direct（APK-11 加了 cvh/px/py 和 frame）
    cd_main = len(buf)
    buf += uleb(4) + uleb(0) + uleb(3) + uleb(0)
    buf += uleb(F_CVH - 0) + uleb(0x0008)        # cvh：static int
    buf += uleb(F_HITS - F_CVH) + uleb(0x0008)   # hits
    buf += uleb(F_PX - F_HITS) + uleb(0x0008)    # px
    buf += uleb(F_PY - F_PX) + uleb(0x0008)      # py
    buf += uleb(M_RUN - 0) + uleb(0x0009) + uleb(code_run)
    buf += uleb(M_HELLO - M_RUN) + uleb(0x0009) + uleb(code_hello)
    buf += uleb(M_FRAME - M_HELLO) + uleb(0x0009) + uleb(code_frame)

    # Base 的 class_data：1 实例字段 + 1 direct + 1 virtual
    cd_base = len(buf)
    buf += uleb(0) + uleb(1) + uleb(1) + uleb(1)
    buf += uleb(F_BASE_V - 0) + uleb(0x0002)
    buf += uleb(M_BASE_INIT - 0) + uleb(0x10001) + uleb(code_base_init)
    buf += uleb(M_BASE_GET - 0) + uleb(0x0001) + uleb(code_base_get)

    # Child 的 class_data：0 字段 + 1 direct
    cd_child = len(buf)
    buf += uleb(0) + uleb(0) + uleb(1) + uleb(0)
    buf += uleb(M_CHILD_INIT - 0) + uleb(0x10001) + uleb(code_child_init)

    # Boom 的 class_data：0 字段 + 1 direct
    cd_boom = len(buf)
    buf += uleb(0) + uleb(0) + uleb(1) + uleb(0)
    buf += uleb(M_BOOM_INIT - 0) + uleb(0x10001) + uleb(code_boom_init)

    for pos, off in zip(str_off_pos, str_offsets):
        struct.pack_into("<I", buf, pos, off)

    struct.pack_into("<IIIIIIII", buf, main_def_pos,
                     T_MAIN, 0x0001, 0xFFFFFFFF, 0, 0xFFFFFFFF, 0, cd_main, sv_main)
    struct.pack_into("<IIIIIIII", buf, counter_def_pos,
                     T_COUNTER, 0x0001, 0xFFFFFFFF, 0, 0xFFFFFFFF, 0, cd_counter, 0)
    struct.pack_into("<IIIIIIII", buf, base_def_pos,
                     T_BASE, 0x0001, 0xFFFFFFFF, 0, 0xFFFFFFFF, 0, cd_base, 0)
    struct.pack_into("<IIIIIIII", buf, child_def_pos,
                     T_CHILD, 0x0001, T_BASE, 0, 0xFFFFFFFF, 0, cd_child, 0)
    struct.pack_into("<IIIIIIII", buf, rte_def_pos,
                     T_RTE, 0x0001, 0xFFFFFFFF, 0, 0xFFFFFFFF, 0, 0, 0)
    struct.pack_into("<IIIIIIII", buf, boom_def_pos,
                     T_BOOM, 0x0001, 0xFFFFFFFF, 0, 0xFFFFFFFF, 0, cd_boom, 0)
    struct.pack_into("<IIIIIIII", buf, cv_def_pos,
                     T_CV, 0x0001, 0xFFFFFFFF, 0, 0xFFFFFFFF, 0, 0, 0)

    data_size = len(buf) - data_off

    items = [
        (0x0000, 1, 0),
        (0x0001, len(STRINGS), string_ids_off),
        (0x0002, 13, type_ids_off),
        (0x0003, 9, proto_ids_off),
        (0x0004, 6, field_ids_off),
        (0x0005, 26, method_ids_off),
        (0x0006, 7, class_defs_off),
        (0x1000, 1, map_off),
        (0x1001, 2, tl_str),
        (0x2000, 5, cd_counter),
        (0x2001, 9, code_init),
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
        type_ids=(13, type_ids_off),
        proto_ids=(9, proto_ids_off),
        field_ids=(6, field_ids_off),
        method_ids=(26, method_ids_off),
        class_defs=(7, class_defs_off),
        data=(data_size, data_off),
        code_init=code_init, code_next=code_next,
        code_run=code_run, code_hello=code_hello,
        code_base_init=code_base_init, code_base_get=code_base_get,
        code_child_init=code_child_init, code_boom_init=code_boom_init,
        code_frame=code_frame,
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
    for name in ("code_init", "code_next", "code_run", "code_hello",
                 "code_base_init", "code_base_get", "code_child_init", "code_boom_init",
                 "code_frame"):
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
