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

ifeq ($(BOARD),rpi4)
  BDEF      := -DASTER_BOARD_RPI4
  LOAD_BASE := 0x80000
  KERN_BASE := 0x200000
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

.PHONY: all image kernel sign boot run run-sock test clean rpi4 info user

all: image

info:
	@echo "BOARD=$(BOARD)  LOAD=$(LOAD_BASE)  KERNEL=$(KERN_BASE)"

# ---------- 0. 用户态程序（独立扁平二进制，链接到 0x44400000） ----------
USER_BIN  := $(B)/user/hello.bin
FAULT_BIN := $(B)/user/fault.bin
BUSY_BIN  := $(B)/user/busy.bin

user: $(USER_BIN) $(FAULT_BIN) $(BUSY_BIN)

$(B)/user/%.o: user/%.S
	@mkdir -p $(B)/user
	$(CROSS)-gcc -c -mgeneral-regs-only -ffreestanding -nostdlib -Wall -Wextra \
	    $< -o $@

$(B)/user/%.elf: $(B)/user/%.o user/hello.ld
	$(CROSS)-ld -T user/hello.ld -o $@ $<

$(USER_BIN): $(B)/user/hello.elf
	$(CROSS)-objcopy -O binary $< $@
	@echo "[BUILD] 用户程序 hello: $$(stat -c%s $@) bytes"

$(FAULT_BIN): $(B)/user/fault.elf
	$(CROSS)-objcopy -O binary $< $@
	@echo "[BUILD] 用户程序 fault: $$(stat -c%s $@) bytes"

$(BUSY_BIN): $(B)/user/busy.elf
	$(CROSS)-objcopy -O binary $< $@
	@echo "[BUILD] 用户程序 busy: $$(stat -c%s $@) bytes"

# ---------- 1. 内核（Rust + C） ----------
kernel: $(KBIN)

$(KBIN): FORCE $(USER_BIN) $(FAULT_BIN) $(BUSY_BIN)
	@mkdir -p $(B)
	@test -f boot/keys/boot.key || python3 tools/aster-sign.py keygen
	@python3 tools/aster-sign.py --build-id "aster-$(BOARD)" gen-ex3
	@sed 's/0x40200000/$(KERN_BASE)/' kernel/src/linker.ld > $(B)/kernel.ld
	cd kernel && RUSTFLAGS="-C link-arg=-T$(CURDIR)/$(B)/kernel.ld -C relocation-model=static" \
	    ASTER_USER_BIN="$(CURDIR)/$(USER_BIN)" \
	    ASTER_USER_FAULT_BIN="$(CURDIR)/$(FAULT_BIN)" \
	    ASTER_USER_BUSY_BIN="$(CURDIR)/$(BUSY_BIN)" \
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

# 串口挂到 unix socket，给 tools/afp-flash.py 用
run-sock: image
	@echo "串口 socket: /tmp/aster.sock"
	$(QEMU) -M virt -cpu cortex-a72 -m 512M -display none \
	    -serial unix:/tmp/aster.sock,server,nowait -monitor none -kernel $(BOOT_ELF)

# 非交互冒烟测试：跳过 3 秒菜单，进内核跑命令，逐项断言
test: image
	@printf '\nhelp\nex3\nrun\nrunp\nrunf\nsched\nhash\nhalt\n' | timeout 45 \
	    $(QEMU) -M virt -cpu cortex-a72 -m 512M -nographic -kernel $(BOOT_ELF) \
	    | tee $(B)/test.log
	@grep -q "Aster 内核已接管" $(B)/test.log && echo "[TEST] PASS 内核接管" || (echo "[TEST] FAIL 内核未接管" && exit 1)
	@grep -q "原创校验值.*PASS" $(B)/test.log && echo "[TEST] PASS EX3" || (echo "[TEST] FAIL EX3" && exit 1)
	@grep -q "TASK 1" $(B)/test.log && echo "[TEST] PASS EL0 输出" || (echo "[TEST] FAIL EL0 无输出" && exit 1)
	@grep -q "全部任务结束，最后退出码 = 0" $(B)/test.log && echo "[TEST] PASS 系统调用/退出码" || (echo "[TEST] FAIL 退出码" && exit 1)
	@grep -q "数据中止(低 EL)" $(B)/test.log && echo "[TEST] PASS 用户越权被 MMU 拦下" || (echo "[TEST] FAIL 隔离失效" && exit 1)
	@grep -q "TASK 2" $(B)/test.log && echo "[TEST] PASS 双任务轮转" || (echo "[TEST] FAIL 无多任务输出" && exit 1)
	@awk '/BUSY start/{s=NR} /TASK 2/{t=NR} /BUSY end/{e=NR} END{if (s>0 && t>s && e>t) print "[TEST] PASS 抢占：纯计算期间另一任务仍在跑"; else {print "[TEST] FAIL 抢占未生效"; exit 1}}' $(B)/test.log

# ---------- 5. 树莓派 ----------
rpi4:
	$(MAKE) BOARD=rpi4 image
	@cp $(B)/aster-boot.bin $(B)/kernel8.img
	@echo "生成 $(B)/kernel8.img —— 拷到 SD 卡 boot 分区即可"

clean:
	rm -rf $(B)
	cd kernel && cargo clean 2>/dev/null || true

FORCE:
