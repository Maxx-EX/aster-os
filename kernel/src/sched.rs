// Aster Kernel — 任务表与轮转调度（阶段二·抢占）
//
// 上下文用统一的"陷阱帧"表示，34 个 u64：
//   [0..30]  x0..x30
//   [31]     SP_EL0
//   [32]     ELR_EL1
//   [33]     SPSR_EL1
//
// 这样 IRQ 抢占时不需要另造一套保存逻辑：把帧存进 Task.ctx，
// 把下一个任务的 ctx 写回同一帧，`eret` 出去就已经是另一个任务了。
//
// 注意：这一版所有任务共用同一张 TTBR0 页表，只是各自占不同的 2 MiB 区
// （A: 0x4440_0000，B: 0x4460_0000）。任务之间互相读得到对方的页 ——
// 真正的地址空间隔离要等每个任务一张页表，见 docs/roadmap.md 阶段二 #4。
#![allow(static_mut_refs)]

pub const FRAME_WORDS: usize = 34;
pub const IX_SP_EL0: usize = 31;
pub const IX_ELR: usize = 32;
pub const IX_SPSR: usize = 33;

#[derive(Clone, Copy, PartialEq)]
pub enum State {
    Ready,
    Dead,
}

#[derive(Clone, Copy)]
pub struct Task {
    pub ctx: [u64; FRAME_WORDS],
    pub state: State,
    pub id: u32,
    pub switches: u32,
}

const MAX_TASKS: usize = 4;

impl Task {
    const EMPTY: Task = Task {
        ctx: [0; FRAME_WORDS],
        state: State::Dead,
        id: 0,
        switches: 0,
    };
}

static mut TASKS: [Task; MAX_TASKS] = [Task::EMPTY; MAX_TASKS];
static mut NTASKS: usize = 0;
static mut CURRENT: usize = 0;
static mut TOTAL_SWITCHES: u32 = 0;

/// 登记一个任务：入口地址 + 用户栈顶，返回任务号（同时作为 getpid 的返回值）
pub fn spawn(entry: u64, sp_el0: u64) -> Option<u32> {
    unsafe {
        if NTASKS >= MAX_TASKS {
            return None;
        }
        let i = NTASKS;
        let id = (i + 1) as u32;
        TASKS[i] = Task::EMPTY;
        TASKS[i].state = State::Ready;
        TASKS[i].id = id;
        TASKS[i].ctx[IX_SP_EL0] = sp_el0;
        TASKS[i].ctx[IX_ELR] = entry;
        TASKS[i].ctx[IX_SPSR] = 0; // EL0t、DAIF 全开
        NTASKS += 1;
        Some(id)
    }
}

/// 重新开始一批任务（shell 里第二次 run 之前调用）
pub fn reset() {
    unsafe {
        NTASKS = 0;
        CURRENT = 0;
        TOTAL_SWITCHES = 0;
        for t in TASKS.iter_mut() {
            *t = Task::EMPTY;
        }
    }
}

pub fn count() -> usize {
    unsafe { NTASKS }
}

pub fn current_id() -> u32 {
    unsafe {
        if CURRENT < NTASKS {
            TASKS[CURRENT].id
        } else {
            0
        }
    }
}

unsafe fn pick_next() -> Option<usize> {
    if NTASKS == 0 {
        return None;
    }
    for k in 1..=NTASKS {
        let i = (CURRENT + k) % NTASKS;
        if TASKS[i].state == State::Ready {
            return Some(i);
        }
    }
    None
}

unsafe fn load_into(frame: &mut [u64], idx: usize) {
    frame.copy_from_slice(&TASKS[idx].ctx);
    TASKS[idx].switches = TASKS[idx].switches.saturating_add(1);
    CURRENT = idx;
    TOTAL_SWITCHES = TOTAL_SWITCHES.saturating_add(1);
}

/// 定时器中断里调用：保存当前任务、切到下一个 Ready 任务
pub fn tick(frame: *mut u64) {
    unsafe {
        let mut f = core::slice::from_raw_parts_mut(frame, FRAME_WORDS);

        if CURRENT < NTASKS && TASKS[CURRENT].state == State::Ready {
            TASKS[CURRENT].ctx.copy_from_slice(&f);
            TASKS[CURRENT].state = State::Ready;
        }

        if let Some(n) = pick_next() {
            load_into(&mut f, n);
        } else {
            // 没有别的任务可跑：保持当前上下文原样（等价于这次 tick 白给）
            if CURRENT < NTASKS {
                TASKS[CURRENT].ctx.copy_from_slice(&f);
            }
        }
    }
}

/// 当前任务调用 exit：标记 Dead 并把 CPU 交给下一个任务。
/// 返回 true = 还有任务在跑（帧已换人）；false = 全跑完了，该回内核。
pub fn exit_current(frame: *mut u64) -> bool {
    unsafe {
        let mut f = core::slice::from_raw_parts_mut(frame, FRAME_WORDS);
        if CURRENT < NTASKS {
            TASKS[CURRENT].state = State::Dead;
        }
        match pick_next() {
            Some(n) => {
                load_into(&mut f, n);
                true
            }
            None => false,
        }
    }
}

pub fn stats() -> &'static str {
    struct Buf {
        d: [u8; 96],
        n: usize,
    }
    impl core::fmt::Write for Buf {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for &b in s.as_bytes() {
                if self.n < 95 {
                    self.d[self.n] = b;
                    self.n += 1;
                }
            }
            Ok(())
        }
    }
    unsafe {
        static mut B: Buf = Buf { d: [0; 96], n: 0 };
        B.n = 0;
        let _ = core::fmt::write(
            &mut B,
            format_args!("任务数 {}，总切换 {}", NTASKS, TOTAL_SWITCHES),
        );
        B.d[B.n] = 0;
        core::str::from_utf8_unchecked(&B.d[..B.n])
    }
}
