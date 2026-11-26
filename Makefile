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

.PHONY: all image kernel sign boot run run-sock test clean rpi4 info

all: image

info:
	@echo "BOARD=$(BOARD)  LOAD=$(LOAD_BASE)  KERNEL=$(KERN_BASE)"

# ---------- 1. 内核（Rust + C） ----------
kernel: $(KBIN)

$(KBIN): FORCE
	@mkdir -p $(B)
	@test -f boot/keys/boot.key || python3 tools/aster-sign.py keygen
	@python3 tools/aster-sign.py --build-id "aster-$(BOARD)" gen-ex3
	@sed 's/0x40200000/$(KERN_BASE)/' kernel/src/linker.ld > $(B)/kernel.ld
	cd kernel && RUSTFLAGS="-C link-arg=-T$(CURDIR)/$(B)/kernel.ld -C relocation-model=static" \
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

# 非交互冒烟测试：跳过 3 秒菜单，进内核跑几条命令
test: image
	@printf '\nhelp\nex3\nhash\nhalt\n' | timeout 25 \
	    $(QEMU) -M virt -cpu cortex-a72 -m 512M -nographic -kernel $(BOOT_ELF) \
	    | tee $(B)/test.log
	@grep -q "Aster 内核已接管" $(B)/test.log && echo "[TEST] PASS: 内核已启动" || (echo "[TEST] FAIL" && exit 1)

# ---------- 5. 树莓派 ----------
rpi4:
	$(MAKE) BOARD=rpi4 image
	@cp $(B)/aster-boot.bin $(B)/kernel8.img
	@echo "生成 $(B)/kernel8.img —— 拷到 SD 卡 boot 分区即可"

clean:
	rm -rf $(B)
	cd kernel && cargo clean 2>/dev/null || true

FORCE:
