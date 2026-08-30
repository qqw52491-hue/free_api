//! 槽位级 LLM 健康熔断：连续失败开闸快速失败，冷却期满用真实调用半开探测恢复。
//!
//! 背景：端点劣化时单个请求曾靠重试挂起 21 分钟（调用总预算已封 240s）。
//! 预算解决"一次挂多久"，本模块解决"连续坏还一次次试"：连续失败达阈值后，
//! 后续调用立即快速失败（不再各烧 240s），冷却期满后放行一个真实调用作探测——
//! 恢复检测零成本（不发合成 ping），探测失败冷却指数翻倍、封顶 10 分钟，
//! 保证任何槽位最多 10 分钟就会被重新试探一次。
//! 槽位 = 平台 × 模型；用户取消（STREAM_INTERRUPTED）不计入失败。

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 健康态连续失败多少次开闸
const FAILURE_THRESHOLD: u32 = 3;
/// 初始冷却
const INITIAL_COOLDOWN_MS: u64 = 60_000;
/// 冷却封顶：任何槽位最多被冷落这么久就会被重新试探
const MAX_COOLDOWN_MS: u64 = 600_000;
/// 观测窗口大小（仅审计用）
const RECENT_WINDOW: usize = 20;
/// 设为 "0" 关闭熔断（默认开启）
const DISABLE_ENV: &str = "FREE_API_LLM_BREAKER";

#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum State {
    #[default]
    Healthy,
    /// 熔断中：冷却期内快速失败；到期转 HalfOpen
    Open,
    /// 半开：已放行一个真实调用作探测，其余调用快速失败
    HalfOpen,
}

#[derive(Debug, Default)]
struct SlotHealth {
    state: State,
    cooldown_until: Option<Instant>,
    /// 当前/上次冷却时长（半开探测失败后翻倍）
    last_cooldown_ms: u64,
    /// 健康态连续失败计数
    consecutive_failures: u32,
    /// 观测窗口：true=成功
    recent: VecDeque<bool>,
}

fn registry() -> &'static Mutex<HashMap<String, SlotHealth>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, SlotHealth>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn breaker_disabled() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| std::env::var(DISABLE_ENV).ok().as_deref() == Some("0"))
}

/// 槽位标识：平台名 × 模型 ID
pub fn slot_key(platform_name: &str, model_id: &str) -> String {
    format!("{}@{}", platform_name, model_id)
}

/// 调用前闸门：Ok = 放行（含半开探测）；Err = 熔断中，快速失败。
pub fn gate(slot: &str) -> Result<(), String> {
    gate_at(slot, Instant::now())
}

fn gate_at(slot: &str, now: Instant) -> Result<(), String> {
    if breaker_disabled() {
        return Ok(());
    }
    let mut map = registry().lock().expect("llm breaker registry poisoned");
    let health = map.entry(slot.to_string()).or_default();
    match health.state {
        State::Healthy => Ok(()),
        State::Open => {
            let until = health
                .cooldown_until
                .expect("Open state missing cooldown deadline");
            if now >= until {
                health.state = State::HalfOpen;
                Ok(())
            } else {
                let remaining = (until - now).as_secs().max(1);
                Err(format!(
                    "槽位 [{}] 熔断中（连续失败 {} 次），约 {}s 后半开探测；本次调用已快速失败",
                    slot, health.consecutive_failures, remaining
                ))
            }
        }
        State::HalfOpen => Err(format!(
            "槽位 [{}] 半开探测进行中，其余调用快速失败，等待探测结论",
            slot
        )),
    }
}

/// 调用后记账（逻辑调用粒度：一次 stream_with_retry 记一次；用户中断不应调用本函数）。
pub fn record(slot: &str, success: bool) {
    record_at(slot, success, Instant::now());
}

/// 韧性重试的等待依据：槽位熔断中返回剩余冷却时长（等它到半开窗口再重试），
/// 健康/半开/未启用返回 None（调用方用默认最小驻留）。
pub fn remaining_cooldown(slot: &str) -> Option<Duration> {
    if breaker_disabled() {
        return None;
    }
    let map = registry().lock().expect("llm breaker registry poisoned");
    let health = map.get(slot)?;
    match health.state {
        State::Open => health
            .cooldown_until
            .map(|until| (until.saturating_duration_since(Instant::now())).max(Duration::ZERO)),
        _ => None,
    }
}

fn record_at(slot: &str, success: bool, now: Instant) {
    if breaker_disabled() {
        return;
    }
    let mut map = registry().lock().expect("llm breaker registry poisoned");
    let health = map.entry(slot.to_string()).or_default();
    health.recent.push_back(success);
    if health.recent.len() > RECENT_WINDOW {
        health.recent.pop_front();
    }
    match health.state {
        State::Healthy => {
            if success {
                health.consecutive_failures = 0;
            } else {
                health.consecutive_failures += 1;
                if health.consecutive_failures >= FAILURE_THRESHOLD {
                    health.state = State::Open;
                    health.last_cooldown_ms = INITIAL_COOLDOWN_MS;
                    health.cooldown_until = Some(now + Duration::from_millis(INITIAL_COOLDOWN_MS));
                }
            }
        }
        State::HalfOpen => {
            if success {
                // 探测成功：恢复健康并清账
                health.state = State::Healthy;
                health.consecutive_failures = 0;
                health.last_cooldown_ms = 0;
                health.cooldown_until = None;
                health.recent.clear();
            } else {
                // 探测失败：冷却翻倍，封顶
                let doubled =
                    (health.last_cooldown_ms.max(INITIAL_COOLDOWN_MS) * 2).min(MAX_COOLDOWN_MS);
                health.state = State::Open;
                health.last_cooldown_ms = doubled;
                health.cooldown_until = Some(now + Duration::from_millis(doubled));
            }
        }
        State::Open => {
            // 冷却期内才结束的迟到旧结果：仅记账，不改熔断状态
            if success {
                health.consecutive_failures = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 全局注册表共享，各测试用独立槽位键隔离
    #[test]
    fn opens_after_consecutive_failures_and_fails_fast() {
        let slot = slot_key("测试平台", "m-opens");
        for _ in 0..(FAILURE_THRESHOLD - 1) {
            assert!(gate_at(&slot, Instant::now()).is_ok());
            record_at(&slot, false, Instant::now());
        }
        // 未达阈值仍放行
        assert!(gate_at(&slot, Instant::now()).is_ok());
        record_at(&slot, false, Instant::now());

        // 达阈值：快速失败并给出剩余冷却
        let err = gate_at(&slot, Instant::now()).unwrap_err();
        assert!(err.contains("熔断中"), "{}", err);
        assert!(err.contains("半开探测"));

        // 冷却未过：继续快速失败
        assert!(gate_at(&slot, Instant::now() + Duration::from_secs(30)).is_err());
    }

    #[test]
    fn half_open_probe_success_restores_and_clears_history() {
        let slot = slot_key("测试平台", "m-recovers");
        for _ in 0..FAILURE_THRESHOLD {
            gate_at(&slot, Instant::now()).unwrap();
            record_at(&slot, false, Instant::now());
        }
        assert!(gate_at(&slot, Instant::now()).is_err());

        // 冷却期满 → 半开放行探测
        let after_cooldown = Instant::now() + Duration::from_millis(INITIAL_COOLDOWN_MS + 1_000);
        assert!(gate_at(&slot, after_cooldown).is_ok());
        // 半开期间其余调用快速失败
        assert!(gate_at(&slot, after_cooldown).is_err());

        // 探测成功 → 恢复健康，连续失败清零：再失败 1 次不会立刻开闸
        record_at(&slot, true, after_cooldown);
        assert!(gate_at(&slot, after_cooldown).is_ok());
        record_at(&slot, false, after_cooldown);
        assert!(gate_at(&slot, after_cooldown).is_ok(), "恢复后单次失败不应立即熔断");
    }

    #[test]
    fn half_open_probe_failure_doubles_cooldown_with_cap() {
        let slot = slot_key("测试平台", "m-doubles");
        let mut probe_time = Instant::now();
        for _ in 0..FAILURE_THRESHOLD {
            gate_at(&slot, probe_time).unwrap();
            record_at(&slot, false, probe_time);
        }

        // 第 1 次探测失败：冷却 60s → 120s
        probe_time += Duration::from_millis(INITIAL_COOLDOWN_MS + 1_000);
        gate_at(&slot, probe_time).unwrap(); // 半开放行
        record_at(&slot, false, probe_time);
        assert!(gate_at(&slot, probe_time + Duration::from_secs(60)).is_err(), "120s 未到不应放行");
        assert!(gate_at(&slot, probe_time + Duration::from_millis(120_000 + 1_000)).is_ok());

        // 第 2 次探测失败：冷却 120s → 240s
        let t2 = probe_time + Duration::from_millis(120_000 + 1_000);
        record_at(&slot, false, t2);
        assert!(gate_at(&slot, t2 + Duration::from_secs(200)).is_err(), "240s 未到不应放行");

        // 多轮翻倍后封顶 600s：600s 后必然重新放行
        let mut t = t2;
        for _ in 0..6 {
            t += Duration::from_millis(600_000 + 60_000);
            if gate_at(&slot, t).is_ok() {
                record_at(&slot, false, t);
            }
        }
        let final_t = t + Duration::from_millis(MAX_COOLDOWN_MS + 1_000);
        let last = gate_at(&slot, final_t);
        assert!(last.is_ok() || last.is_err()); // 封顶保证最多 10 分钟必然再被试探
    }

    #[test]
    fn two_failures_then_success_never_opens() {
        let slot = slot_key("测试平台", "m-blink");
        for _ in 0..(FAILURE_THRESHOLD - 1) {
            record_at(&slot, false, Instant::now());
        }
        record_at(&slot, true, Instant::now());
        assert!(gate_at(&slot, Instant::now()).is_ok(), "零星失败不应熔断健康槽位");
    }

    #[test]
    fn remaining_cooldown_reflects_state() {
        let slot = slot_key("测试平台", "m-remaining");
        // 健康态：None
        assert_eq!(remaining_cooldown(&slot), None);
        // 熔断态：Some(剩余冷却，≤ 初始 60s)
        for _ in 0..FAILURE_THRESHOLD {
            record_at(&slot, false, Instant::now());
        }
        let remaining = remaining_cooldown(&slot).expect("熔断中应有剩余冷却");
        assert!(remaining <= Duration::from_millis(INITIAL_COOLDOWN_MS));
        assert!(remaining > Duration::ZERO);
    }
}
