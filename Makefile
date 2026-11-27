# Aster —— 星芒 / ARM64 原创系统
# 全流程用命令完成：cargo 编内核 → aster-sign 生成校验清单 → 交叉 gcc 编引导器
#                              → QEMU 启动验证

CROSS     ?= aarch64-linux-gnu
BOARD     ?= qemu
B         := build
KREL      := kernel/target/aarch64-unknown-none/release
KBIN      := $(B)/aster-kernel.ast
BOOT_ELF  := $(B)/aster-boot.elf
QEMU      := qemu-system-aarch64
IMG       := build/aster.img
IMG_MB    := 8

ifeq ($(BOARD),rpi3)
  BDEF      := -DASTER_BOARD_RPI3
  LOAD_BASE := 0x80000
  KERN_BASE := 0x800000
  CARGO_FEAT := --features rpi3
else ifeq ($(BOARD),rpi4)
  BDEF      := -DASTER_BOARD_RPI4
  LOAD_BASE := 0x80000
  KERN_BASE := 0x800000
  CARGO_FEAT := --features rpi4
else
  BDEF      :=
  LOAD_BASE := 0x40080000
  KERN_BASE := 0x40200000
  CARGO_FEAT :=
endif

CFLAGS := -O2 -ffreestanding -fno-builtin -fno-stack-protector -nostdlib \
          -mgeneral-regs-only -Wall -Wextra -Wno-unused-parameter \
          -Iboot $(BDEF)

BOOT_C   := boot/main.c boot/uart.c boot/sha256.c boot/verify.c boot/timer.c \
            boot/image.c boot/flash.c boot/shell.c boot/mem.c
BOOT_OBJ := $(patsubst boot/%.c,$(B)/obj/%.o,$(BOOT_C)) $(B)/obj/start.o

.PHONY: all image kernel sign boot run run-sock run-disk img test clean rpi4 info user

all: image

info:
	@echo "BOARD=$(BOARD)  LOAD=$(LOAD_BASE)  KERNEL=$(KERN_BASE)"

# ---------- 0. 用户态程序（独立扁平二进制，链接到 0x44400000） ----------
USER_BIN  := $(B)/user/hello.bin
ISO_BIN   := $(B)/user/iso.bin
FP_BIN    := $(B)/user/fp.bin
WX_BIN    := $(B)/user/wx.bin
YL_BIN    := $(B)/user/yield.bin
FILE_BIN  := $(B)/user/file.bin
FAULT_BIN := $(B)/user/fault.bin
BUSY_BIN  := $(B)/user/busy.bin

user: $(USER_BIN) $(FAULT_BIN) $(BUSY_BIN) $(ISO_BIN) $(FP_BIN) $(WX_BIN) $(YL_BIN) $(FILE_BIN)

$(B)/user/%.o: user/%.S
	@mkdir -p $(B)/user
	$(CROSS)-gcc -c -mgeneral-regs-only -ffreestanding -nostdlib -Wall -Wextra \
	    $< -o $@

$(B)/user/%.elf: $(B)/user/%.o user/hello.ld
	$(CROSS)-ld -T user/hello.ld -o $@ $<

$(ISO_BIN): $(B)/user/iso.elf
	$(CROSS)-objcopy -O binary $< $@
	@echo "[BUILD] 用户程序 iso: $$(stat -c%s $@) bytes"

$(FP_BIN): $(B)/user/fp.elf
	$(CROSS)-objcopy -O binary $< $@
	@echo "[BUILD] 用户程序 fp: $$(stat -c%s $@) bytes"

$(WX_BIN): $(B)/user/wx.elf
	$(CROSS)-objcopy -O binary $< $@
	@echo "[BUILD] 用户程序 wx: $$(stat -c%s $@) bytes"

$(YL_BIN): $(B)/user/yield.elf
	$(CROSS)-objcopy -O binary $< $@
	@echo "[BUILD] 用户程序 yield: $$(stat -c%s $@) bytes"

$(USER_BIN): $(B)/user/hello.elf
	$(CROSS)-objcopy -O binary $< $@
	@echo "[BUILD] 用户程序 hello: $$(stat -c%s $@) bytes"

$(FAULT_BIN): $(B)/user/fault.elf
	$(CROSS)-objcopy -O binary $< $@
	@echo "[BUILD] 用户程序 fault: $$(stat -c%s $@) bytes"

$(BUSY_BIN): $(B)/user/busy.elf
	$(CROSS)-objcopy -O binary $< $@
	@echo "[BUILD] 用户程序 busy: $$(stat -c%s $@) bytes"

$(FILE_BIN): $(B)/user/file.elf
	$(CROSS)-objcopy -O binary $< $@
	@echo "[BUILD] 用户程序 file: $$(stat -c%s $@) bytes"

# ---------- 1. 内核（Rust + C） ----------
kernel: $(KBIN)

$(KBIN): FORCE $(USER_BIN) $(FAULT_BIN) $(BUSY_BIN) $(ISO_BIN) $(FP_BIN) $(WX_BIN) $(YL_BIN)
	@mkdir -p $(B)
	@test -f boot/keys/boot.key || python3 tools/aster-sign.py keygen
	@python3 tools/aster-sign.py --build-id "aster-$(BOARD)" gen-ex3
	@sed 's/0x40200000/$(KERN_BASE)/' kernel/src/linker.ld > $(B)/kernel.ld
	cd kernel && RUSTFLAGS="-C link-arg=-T$(CURDIR)/$(B)/kernel.ld -C relocation-model=static" \
	    ASTER_USER_BIN="$(CURDIR)/$(USER_BIN)" \
	    ASTER_USER_FAULT_BIN="$(CURDIR)/$(FAULT_BIN)" \
	    ASTER_USER_BUSY_BIN="$(CURDIR)/$(BUSY_BIN)" \
	    ASTER_USER_ISO_BIN="$(CURDIR)/$(ISO_BIN)" \
	    ASTER_USER_FP_BIN="$(CURDIR)/$(FP_BIN)" \
	    ASTER_USER_WX_BIN="$(CURDIR)/$(WX_BIN)" \
	    ASTER_USER_YIELD_BIN="$(CURDIR)/$(YL_BIN)" \
	    cargo build --release $(CARGO_FEAT)
	$(CROSS)-objcopy -O binary $(KREL)/aster-kernel $@
	@echo "[BUILD] kernel .ast: $$(stat -c%s $@) bytes"

# ---------- 2. 签名 + 生成 AVP 清单 ----------
sign: $(KBIN)
	@test -f boot/keys/boot.key || python3 tools/aster-sign.py keygen
	python3 tools/aster-sign.py --build-id "aster-$(BOARD)" sign $(KBIN)

# ---------- 3. 引导器 ----------
boot: sign $(BOOT_OBJ)
	@sed 's/0x40080000/$(LOAD_BASE)/' boot/linker.ld > $(B)/boot.ld
	$(CROSS)-objcopy -I binary -O elf64-littleaarch64 -B aarch64 \
	    --rename-section .data=.payload,alloc,load,readonly,data,contents \
	    $(KBIN) $(B)/obj/payload.o
	$(CROSS)-ld -T $(B)/boot.ld -o $(BOOT_ELF) $(BOOT_OBJ) $(B)/obj/payload.o
	@$(CROSS)-objcopy -O binary $(BOOT_ELF) $(B)/aster-boot.bin
	@echo "[BUILD] boot elf: $$(stat -c%s $(BOOT_ELF)) bytes"

$(B)/obj/%.o: boot/%.c boot/manifest.h boot/ex3.h
	@mkdir -p $(B)/obj
	$(CROSS)-gcc $(CFLAGS) -c $< -o $@

$(B)/obj/start.o: boot/start.S
	@mkdir -p $(B)/obj
	$(CROSS)-gcc $(CFLAGS) -c $< -o $@

image: boot

# ---------- 4. 运行 ----------
run: image
	$(QEMU) -M virt -cpu cortex-a72 -m 512M -nographic -kernel $(BOOT_ELF)

# 图形 + 远程：画面走 VNC（默认 5901），命令行走 TCP（默认 4444）
img:
	@mkdir -p build
	@dd if=/dev/zero of=$(IMG) bs=1M count=$(IMG_MB) status=none
	@echo "[IMG] $(IMG) $(IMG_MB) MiB"

run-disk: image img
	@echo "  磁盘: $(IMG) ($(IMG_MB) MiB)   控制台: nc 0.0.0.0 4455"
	$(QEMU) -M virt -cpu cortex-a72 -m 512M -device ramfb \
	    -device virtio-keyboard-device -device virtio-mouse-device \
	    -global virtio-mmio.force-legacy=false \
	    -drive if=none,file=$(IMG),format=raw,id=d0,file.locking=off,cache=writethrough \
	    -device virtio-blk-device,drive=d0 \
	    -display vnc=0.0.0.0:2 -serial tcp:0.0.0.0:4455,server,nowait \
	    -monitor unix:/tmp/aster-mon.sock,server,nowait -kernel $(BOOT_ELF)

run-gfx: image
	@echo "  画面: VNC  vnc://<本机IP>:5901"
	@echo "  终端: TCP  nc <本机IP> 4444"
	$(QEMU) -M virt -cpu cortex-a72 -m 512M -device ramfb \
	    -device virtio-keyboard-device -device virtio-mouse-device \
	    -display vnc=0.0.0.0:1 -serial tcp:0.0.0.0:4444,server,nowait \
	    -monitor unix:/tmp/aster-mon.sock,server,nowait -kernel $(BOOT_ELF)

# 网络：内核自带 TCP/IP 栈与 HTTP 服务，宿主用 hostfwd 打开 guest:80
run-net: image
	@echo "  浏览器打开: http://127.0.0.1:18088/"
	@echo "  终端: nc 127.0.0.1 4448"
	$(QEMU) -M virt -cpu cortex-a72 -m 512M -global virtio-mmio.force-legacy=false \
	    -device virtio-net-device,netdev=n0 -netdev user,id=n0,hostfwd=tcp::18088-:80 \
	    -device ramfb -display vnc=0.0.0.0:1 \
	    -serial tcp:0.0.0.0:4448,server,nowait \
	    -monitor unix:/tmp/aster-mon.sock,server,nowait -kernel $(BOOT_ELF)

# 网络端到端冒烟：起 QEMU，等内核起 HTTP，宿主 curl 应拿到 200 和页面
test-net: image
	@python3 tools/nettest.py 28088 4448 | tee $(B)/test-net.log
	@grep -q "200" $(B)/test-net.log && echo "[TEST] PASS 内核 HTTP 200" || (echo "[TEST] FAIL 未响应" && exit 1)

# 抓一张画面存成 PNG（不需要人工开 VNC 客户端）
shot: image
	@mkdir -p $(B)
	python3 tools/screenshot.py $(B)/screen.png 7

# 图形链路冒烟测试：带 ramfb 跑一次，断言 framebuffer 起来且分辨率对
test-gfx: image
	@printf '\ngfx\nhalt\n' | timeout 45 \
	    $(QEMU) -M virt -cpu cortex-a72 -m 512M -device ramfb -display none -nographic \
	    -kernel $(BOOT_ELF) > $(B)/test-gfx.log 2>&1 || true
	@grep -q "图形输出已就绪：ramfb 1024x768" $(B)/test-gfx.log && echo "[TEST] PASS ramfb framebuffer" || (echo "[TEST] FAIL 图形未就绪" && exit 1)
	@grep -q "界面已重绘" $(B)/test-gfx.log && echo "[TEST] PASS 界面绘制" || (echo "[TEST] FAIL 未绘制" && exit 1)
	@python3 tools/screenshot.py $(B)/screen.png 7 >/dev/null && \
	    python3 -c "from PIL import Image;import sys; im=Image.open('$(B)/screen.png'); \
print('[TEST] PASS 截屏 %dx%d' % im.size); \
sys.exit(0 if im.size==(1024,768) else 1)"

# 串口挂到 unix socket，给 tools/afp-flash.py 用
run-sock: image
	@echo "串口 socket: /tmp/aster.sock"
	$(QEMU) -M virt -cpu cortex-a72 -m 512M -display none \
	    -serial unix:/tmp/aster.sock,server,nowait -monitor none -kernel $(BOOT_ELF)

# 非交互冒烟测试：跳过 3 秒菜单，进内核跑命令，逐项断言
test: image
	@printf '\nhelp\nex3\nrun\nrunp\nrunf\nsched\nhash\niso\napk2 dex\nhalt\n' | timeout 45 \
	    $(QEMU) -M virt -cpu cortex-a72 -m 512M -nographic -kernel $(BOOT_ELF) \
	    | tee $(B)/test.log
	@grep -q "Aster 内核已接管" $(B)/test.log && echo "[TEST] PASS 内核接管" || (echo "[TEST] FAIL 内核未接管" && exit 1)
	@grep -q "原创校验值.*PASS" $(B)/test.log && echo "[TEST] PASS EX3" || (echo "[TEST] FAIL EX3" && exit 1)
	@grep -q "TASK 1" $(B)/test.log && echo "[TEST] PASS EL0 输出" || (echo "[TEST] FAIL EL0 无输出" && exit 1)
	@grep -q "全部任务结束，最后退出码 = 0" $(B)/test.log && echo "[TEST] PASS 系统调用/退出码" || (echo "[TEST] FAIL 退出码" && exit 1)
	@grep -q "数据中止(低 EL)" $(B)/test.log && echo "[TEST] PASS 用户越权被 MMU 拦下" || (echo "[TEST] FAIL 隔离失效" && exit 1)
	@grep -q "TASK 2" $(B)/test.log && echo "[TEST] PASS 双任务轮转" || (echo "[TEST] FAIL 无多任务输出" && exit 1)
	@grep -q "deflate 解压正确" $(B)/test.log && echo "[TEST] PASS inflate（deflate 条目还原）" || (echo "[TEST] FAIL deflate" && exit 1)
	@awk '/BUSY start/{s=NR} /TASK 2/{t=NR} /BUSY end/{e=NR} END{if (s>0 && t>s && e>t) print "[TEST] PASS 抢占：纯计算期间另一任务仍在跑"; else {print "[TEST] FAIL 抢占未生效"; exit 1}}' $(B)/test.log
	# 隔离：两个任务写同一 VA，各自读回自己写的值（打印 '.'，不是 'X'）
	@grep -q "ISO 1: ." $(B)/test.log && grep -q "ISO 2: ." $(B)/test.log && \
	    echo "[TEST] PASS 地址空间隔离（同 VA 不串）" || (echo "[TEST] FAIL 地址空间串了" && exit 1)
	# 浮点上下文：单独跑一次（连发命令会丢字符，必须带节拍）
	@(sleep 9; printf 'fp\n'; sleep 4; printf 'halt\n') | timeout 55 \
	    $(QEMU) -M virt -cpu cortex-a72 -m 512M -nographic -kernel $(BOOT_ELF) \
	    > $(B)/test-fp.log 2>&1 || true
	@grep -q "FP 1: ." $(B)/test-fp.log && grep -q "FP 2: ." $(B)/test-fp.log && \
	    echo "[TEST] PASS 浮点上下文不串（每任务 q0-q31）" || (echo "[TEST] FAIL 浮点上下文串了" && exit 1)
	# W^X：EL0 写自己的代码页必须被拦（FAR 落在 0x4440_0xxx）
	@(sleep 9; printf 'wx\n'; sleep 3; printf 'halt\n') | timeout 50 \
	    $(QEMU) -M virt -cpu cortex-a72 -m 512M -nographic -kernel $(BOOT_ELF) \
	    > $(B)/test-wx.log 2>&1 || true
	@grep -q "WX start" $(B)/test-wx.log && grep -q "数据中止(低 EL)" $(B)/test-wx.log && \
	    grep -q "FAR     = 0x0000000044400" $(B)/test-wx.log && \
	    echo "[TEST] PASS W^X（EL0 写代码页被拦）" || (echo "[TEST] FAIL W^X 未生效" && exit 1)
	# 页帧分配器：两个任务各 5 页，加内核页表共 10 页
	@grep -q "页帧池 10 / 32768 页已用" $(B)/test.log && \
	    echo "[TEST] PASS 页帧分配器" || (echo "[TEST] FAIL 页帧计数不对" && exit 1)

	# sched_yield：两个任务各 8 行，必须严格交替
	@(sleep 9; printf 'yield\n'; sleep 3; printf 'halt\n') | timeout 50 \
	    qemu-system-aarch64 -M virt -cpu cortex-a72 -m 512M -nographic \
	    -kernel $(B)/aster-boot.elf 2>&1 | tr -d '\r' > $(B)/test-yield.log || true
	@python3 -c "import re,sys; t=open('$(B)/test-yield.log').read(); \
seq=[m for m in re.findall(r'Z\\d|Y[12]', t)]; \
ok = seq[:16]==['Y1','Y2']*8; \
print('[TEST] PASS sched_yield 主动让出（严格交替）' if ok else '[TEST] FAIL yield 未交替: '+str(seq[:16])); \
sys.exit(0 if ok else 1)"

# ---------- 5. 树莓派 ----------
rpi4:
	$(MAKE) BOARD=rpi4 image
	@cp $(B)/aster-boot.bin $(B)/kernel8.img
	@echo "生成 $(B)/kernel8.img —— 拷到 SD 卡 boot 分区即可"

rpi3: img
	$(MAKE) BOARD=rpi3 image
	@cp $(B)/aster-boot.bin $(B)/kernel8.img
	@echo "生成 $(B)/kernel8.img（Raspberry Pi 3B）"

# 在没有树莓派 4 机型的环境里，用 QEMU 的 raspi3b 跑同一套驱动
run-rpi3: rpi3
	qemu-system-aarch64 -M raspi3b -m 1G -kernel $(B)/aster-boot.elf \
	  -drive if=sd,format=raw,file=$(IMG) -serial stdio -display none

clean:
	rm -rf $(B)
	cd kernel && cargo clean 2>/dev/null || true

FORCE:
