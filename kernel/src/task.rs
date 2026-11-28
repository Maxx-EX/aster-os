// Aster Kernel — 用户态任务与调度启动（阶段二·抢占）
//
// run  ：把同一份用户程序装进槽 A、槽 B，登记两个任务，开定时器，切 EL0。
//        之后 CPU 每 2 ms 被定时器打断一次，调度器在陷阱帧上换人 ——
//        这就是"一个死循环程序不会锁死整机"的底气。
// runf ：单任务，故意越权读内核地址，验证隔离。
use crate::{exceptions, mmu, sched, timer};

include!(concat!(env!("OUT_DIR"), "/user_image.rs"));

use core::sync::atomic::{AtomicBool, Ordering};

/// 用户程序优先从 AsterFS 读（`tools/mkafs.py` 把 build/user/*.bin 打进镜像根目录），
/// 盘不在/文件没了才退回内核里那份内嵌副本 —— 这样 QEMU 不带盘也照样能跑测试。
static FROM_DISK: AtomicBool = AtomicBool::new(false);

struct PBuf(core::cell::UnsafeCell<[u8; 4096]>);
unsafe impl Sync for PBuf {}
static PBUF: PBuf = PBuf(core::cell::UnsafeCell::new([0u8; 4096]));

fn pick(disk: &str, emb: &'static [u8], emb_size: usize) -> (&'static [u8], usize) {
    let buf = unsafe { &mut *PBUF.0.get() };
    if emb_size != 0 {
        if let Ok(n) = crate::afs::read_file(disk, buf) {
            if n > 0 {
                FROM_DISK.store(true, Ordering::Relaxed);
                crate::println!("[TASK] 程序来源：磁盘 {}（{} 字节）", disk, n);
                return (&buf[..n], n);
            }
        }
        crate::println!("[TASK] 程序来源：内核内嵌 {}（盘上没有）", disk);
    }
    (emb, emb_size)
}

/// 新建地址空间并把程序装进去
fn make_space(bin: &[u8], size: usize) -> Result<mmu::Space, &'static str> {
    if size == 0 {
        return Err("内核构建时没有内嵌该用户程序（先 make user）");
    }
    let sp = mmu::new_space().ok_or("页帧分配器耗尽（池 128 MiB）")?;
    mmu::load_image(&sp, bin)?;
    Ok(sp)
}

/// 两个任务 + 定时器抢占
pub fn run_two() -> Result<u64, &'static str> {
    sched::reset();
    let sa = { let (b, n) = pick("hello", USER_HELLO, USER_HELLO_SIZE); make_space(b, n)? };
    let sb = { let (b, n) = pick("hello", USER_HELLO, USER_HELLO_SIZE); make_space(b, n)? };

    if sched::spawn(sa).is_none() || sched::spawn(sb).is_none() {
        return Err("任务表已满");
    }

    crate::println!(
        "[TASK] 已登记 {} 个任务，时间片 {} ms（GICv2 PPI 30）",
        sched::count(),
        timer::SLICE_MS
    );
    let (used, total, bytes) = crate::frame::stats();
    crate::println!(
        "[MM] 页帧池 {} / {} 页已用（{} KiB），每个任务独占 5 页",
        used, total, bytes / 1024
    );
    crate::println!("[TASK] 切换 EL0 —— 两个任务 VA 相同、物理页不同，靠换 TTBR0 隔离");

    timer::arm();
    mmu::activate(&sa);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}

/// 抢占验证：槽 A 放纯计算任务（不调 syscall），槽 B 放打印任务。
/// 如果 B 的钱印落在 A 的 BUSY start / BUSY end 之间，说明抢占真的在发生。
pub fn run_preempt_test() -> Result<u64, &'static str> {
    sched::reset();
    let sa = { let (b, n) = pick("busy", USER_BUSY, USER_BUSY_SIZE); make_space(b, n)? };
    let sb = { let (b, n) = pick("hello", USER_HELLO, USER_HELLO_SIZE); make_space(b, n)? };

    if sched::spawn(sa).is_none() || sched::spawn(sb).is_none() {
        return Err("任务表已满");
    }
    crate::println!("[TASK] 任务 A = 纯计算（无 syscall），任务 B = 打印任务，观察输出顺序");
    timer::arm();
    mmu::activate(&sa);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}

/// 单任务越权演示
pub fn run_fault() -> Result<u64, &'static str> {
    sched::reset();
    let sa = { let (b, n) = pick("fault", USER_FAULT, USER_FAULT_SIZE); make_space(b, n)? };
    if sched::spawn(sa).is_none() {
        return Err("任务表已满");
    }
    crate::println!("[TASK] 装载越权样本，切 EL0 执行 ld 0x40200000");
    timer::arm();
    mmu::activate(&sa);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}


/// 地址空间隔离验证：两个任务写同一个虚拟地址，看会不会互相覆盖
pub fn run_iso() -> Result<u64, &'static str> {
    sched::reset();
    let sa = { let (b, n) = pick("iso", USER_ISO, USER_ISO_SIZE); make_space(b, n)? };
    let sb = { let (b, n) = pick("iso", USER_ISO, USER_ISO_SIZE); make_space(b, n)? };
    if sched::spawn(sa).is_none() || sched::spawn(sb).is_none() {
        return Err("任务表已满");
    }
    crate::println!("[ISO] 两个任务写同一 VA 0x4440_2200，各自读回：'.' = 隔离成功，'X' = 串了");
    timer::arm();
    mmu::activate(&sa);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}


/// 浮点上下文验证：两个任务各持一个 d0，被抢占后看是否仍属于自己
pub fn run_fp() -> Result<u64, &'static str> {
    sched::reset();
    let sa = { let (b, n) = pick("fp", USER_FP, USER_FP_SIZE); make_space(b, n)? };
    let sb = { let (b, n) = pick("fp", USER_FP, USER_FP_SIZE); make_space(b, n)? };
    if sched::spawn(sa).is_none() || sched::spawn(sb).is_none() {
        return Err("任务表已满");
    }
    crate::println!("[FP] 两个任务各持一个 d0（1.5 / 2.5），被抢占后比较：'.' = 没串，'X' = 串了");
    timer::arm();
    mmu::activate(&sa);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}


/// W^X 验证：从 EL0 写自己的代码页，应当被页表权限拦下
pub fn run_wx() -> Result<u64, &'static str> {
    sched::reset();
    let sa = { let (b, n) = pick("wx", USER_WX, USER_WX_SIZE); make_space(b, n)? };
    if sched::spawn(sa).is_none() {
        return Err("任务表已满");
    }
    crate::println!("[WX] 代码页 AP=0b10（EL0 只读），试着 store 进去看会不会被拦");
    timer::arm();
    mmu::activate(&sa);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}


/// sched_yield 验证：两个任务每打印一行就让出，输出应严格交替
pub fn run_yield() -> Result<u64, &'static str> {
    sched::reset();
    let sa = { let (b, n) = pick("yield", USER_YIELD, USER_YIELD_SIZE); make_space(b, n)? };
    let sb = { let (b, n) = pick("yield", USER_YIELD, USER_YIELD_SIZE); make_space(b, n)? };
    if sched::spawn(sa).is_none() || sched::spawn(sb).is_none() {
        return Err("任务表已满");
    }
    crate::println!("[YIELD] 两个任务各打印 8 行、每行后 sched_yield，期望 Y1 Y2 严格交替");
    timer::arm();
    mmu::activate(&sa);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}

/// 单任务：走 fd 系统调用读文件，验证 VFS 的 open/read/close
pub fn run_file() -> Result<u64, &'static str> {
    sched::reset();
    let s = { let (b, n) = pick("file", USER_FILE, USER_FILE_SIZE); make_space(b, n)? };
    if sched::spawn(s).is_none() {
        return Err("任务表已满");
    }
    crate::vfs::reset();
    crate::println!(
        "[VFS] fd 表 {} 槽就绪（0/1/2 留给标准流），切 EL0 执行 openat → read → close",
        crate::vfs::MAX_FD
    );
    timer::arm();
    mmu::activate(&s);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}

/// 从内存里的用户程序镜像直接起一个任务（`load` 用它把盘上的 .bin 跑起来）。
/// 与 run_xxx 的区别只有一个：字节从文件系统来，不再来自 include_bytes!。
pub fn run_from(bytes: &[u8]) -> Result<u64, &'static str> {
    if bytes.is_empty() {
        return Err("文件是空的");
    }
    sched::reset();
    let sp = make_space(bytes, bytes.len())?;
    if sched::spawn(sp).is_none() {
        return Err("任务表已满");
    }
    crate::println!(
        "[LOAD] {} 字节装入新地址空间（code {:#x}），切 EL0",
        bytes.len(),
        mmu::user_va::CODE
    );
    timer::arm();
    mmu::activate(&sp);
    let code = exceptions::enter_user_state(mmu::user_va::CODE, mmu::user_va::STACK_TOP);
    Ok(code)
}
