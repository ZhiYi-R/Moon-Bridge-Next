//! 插件沙箱配额：指令计数、内存上限、执行超时、body 降级阈值。
//!
//! 目标是在「每插件独立 Lua 状态」之上再加硬性资源边界，防止单个插件：
//! - 死循环 / 超长计算拖垮请求线程（指令计数 + wall-clock 超时，经 debug hook 中止）；
//! - 无界分配内存（`Lua::set_memory_limit`）；
//! - 被超大 raw body 撑爆（body 降级：超阈值不展开为 Lua table，仅告知插件已截断）。
//!
//! 配额检查全部在 hook 内以无锁原子量完成，避免每次钩子调用的额外同步开销。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// 进程级时间基准：用于把 wall-clock 截止时间以原子整数存储，供高频 hook 无锁读取。
fn epoch() -> &'static Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now)
}

/// 距进程基准的纳秒数（单调，不回退）。
fn now_ns() -> u64 {
    epoch().elapsed().as_nanos() as u64
}

/// 沙箱配额上限（含面向本地网关的合理默认值）。
#[derive(Debug, Clone)]
pub struct SandboxLimits {
    /// 单次钩子调用允许执行的 Lua 指令数上限（防死循环）。
    pub max_instructions: u64,
    /// 指令计数 hook 的触发间隔：每执行 N 条指令检查一次（越大开销越低、精度越粗）。
    pub instruction_step: u32,
    /// 单个 Lua 状态的内存上限（字节）；越界分配触发 `MemoryError`。
    pub max_memory_bytes: usize,
    /// 单次钩子调用的 wall-clock 超时（覆盖 hook 内 await 宿主调用的耗时）。
    pub call_timeout: Duration,
    /// raw body 降级阈值（字节）：超过则不展开为 Lua table，改以截断标记告知插件。
    pub max_body_bytes: usize,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        SandboxLimits {
            // 2 亿指令：足够正常插件跑完，又能秒级掐断死循环
            max_instructions: 200_000_000,
            instruction_step: 2000,
            // 256 MB：单插件 Lua 状态内存上限
            max_memory_bytes: 256 * 1024 * 1024,
            call_timeout: Duration::from_secs(10),
            // 4 MB：超过此体积的 raw body 不展开为 Lua table
            max_body_bytes: 4 * 1024 * 1024,
        }
    }
}

/// 单次调用的执行预算：指令计数 + wall-clock 截止时间。
///
/// 由宿主在每次钩子调用前 [`ExecutionBudget::begin`] 重置，Lua debug hook 通过
/// [`ExecutionBudget::charge`] 累加并判定是否越界。以原子量共享，hook 闭包与宿主
/// 各持一份 `Arc`。
pub struct ExecutionBudget {
    count: AtomicU64,
    deadline_ns: AtomicU64,
    max_instructions: u64,
}

impl ExecutionBudget {
    /// 以指令上限构造一份预算（初始截止时间设为「无限远」，加载脚本阶段不误伤）。
    pub fn new(max_instructions: u64) -> Arc<Self> {
        Arc::new(ExecutionBudget {
            count: AtomicU64::new(0),
            deadline_ns: AtomicU64::new(u64::MAX),
            max_instructions,
        })
    }

    /// 每次钩子调用前重置：清零指令计数并设定 wall-clock 截止时间。
    pub fn begin(&self, timeout: Duration) {
        self.count.store(0, Ordering::Relaxed);
        let deadline = now_ns().saturating_add(timeout.as_nanos() as u64);
        self.deadline_ns.store(deadline, Ordering::Relaxed);
    }

    /// hook 内调用：累加本次触发的指令数，越界（指令或超时）则返回错误消息。
    pub fn charge(&self, step: u64) -> Result<(), String> {
        let prev = self.count.fetch_add(step, Ordering::Relaxed);
        if prev.saturating_add(step) > self.max_instructions {
            return Err(format!(
                "插件指令数超过上限 {}（疑似死循环），已中止",
                self.max_instructions
            ));
        }
        if now_ns() > self.deadline_ns.load(Ordering::Relaxed) {
            return Err("插件执行超时，已中止".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_aborts_after_instruction_limit() {
        let b = ExecutionBudget::new(10);
        b.begin(Duration::from_secs(60));
        assert!(b.charge(5).is_ok());
        assert!(b.charge(5).is_ok(), "恰好到上限仍放行");
        let err = b.charge(5).unwrap_err();
        assert!(err.contains("指令数超过上限"), "{err}");
    }

    #[test]
    fn charge_aborts_after_deadline() {
        let b = ExecutionBudget::new(u64::MAX);
        // 截止时间设在「过去」：立即超时
        b.count.store(0, Ordering::Relaxed);
        b.deadline_ns.store(1, Ordering::Relaxed);
        let err = b.charge(1).unwrap_err();
        assert!(err.contains("超时"), "{err}");
    }

    #[test]
    fn begin_resets_budget() {
        let b = ExecutionBudget::new(10);
        b.begin(Duration::from_secs(60));
        assert!(b.charge(100).is_err());
        b.begin(Duration::from_secs(60));
        assert!(b.charge(5).is_ok(), "重置后应重新计数");
    }
}
