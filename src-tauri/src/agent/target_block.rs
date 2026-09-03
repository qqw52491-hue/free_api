use serde::{Deserialize, Serialize};

/// 块状态
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockStatus {
    Pending,
    InProgress,
    Verifying,
    Completed,
    Failed,
}

impl Default for BlockStatus {
    fn default() -> Self {
        Self::Pending
    }
}

/// 执行模型在块验收时提交的自述
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BlockSelfReport {
    /// 核心改动说明
    #[serde(default)]
    pub what_changed: String,
    /// 改动原因与方案选型理由
    #[serde(default)]
    pub rationale: String,
    /// 存疑点、不确定的假设或潜在隐患（重点审查区）
    #[serde(default)]
    pub uncertainties: String,
}

/// 目标块（粗粒度任务单元）：
/// 难度和模型档位依附于目标块，细粒度步骤动作默认继承块的档位。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetBlock {
    pub id: String,
    pub title: String,
    /// 预定允许修改的文件/目录清单（边界控制，防止 Scope Creep 顺手改多）
    #[serde(default)]
    pub scope: Vec<String>,
    /// 模型推理档位："flash" 或 "pro"（默认 "flash"）
    #[serde(default = "default_tier")]
    pub tier: String,
    /// 验收标准清单（Checklist），Reviewer 将拿着逐条量取证据
    #[serde(default)]
    pub checklist: Vec<String>,
    /// 单块硬顶步数上限（默认 15 步，防止陷入死循环）
    #[serde(default = "default_hard_limit")]
    pub hard_step_limit: usize,
    /// 当前块已执行步数（宿主代码计数，不靠模型自觉）
    #[serde(default)]
    pub steps_count: usize,
    /// 允许主动呼叫 Pro 援助的剩余配额（默认 2 次）
    #[serde(default = "default_escalate_quota")]
    pub escalate_quota: usize,
    /// 当前步是否被主动升维至 Pro 支援
    #[serde(default)]
    pub active_escalation: bool,
    /// 审查失败/打回重试计数（用于防死循环熔断）
    #[serde(default)]
    pub review_retry_count: usize,
    /// 执行者提交的完成自述
    #[serde(default)]
    pub self_report: Option<BlockSelfReport>,
    /// 块状态
    #[serde(default)]
    pub status: BlockStatus,
}

pub const MAX_BLOCK_REVIEW_RETRIES: usize = 3;

fn default_tier() -> String {
    "flash".to_string()
}

fn default_hard_limit() -> usize {
    15
}

fn default_escalate_quota() -> usize {
    2
}

impl TargetBlock {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            scope: Vec::new(),
            tier: default_tier(),
            checklist: Vec::new(),
            hard_step_limit: default_hard_limit(),
            steps_count: 0,
            escalate_quota: default_escalate_quota(),
            active_escalation: false,
            review_retry_count: 0,
            self_report: None,
            status: BlockStatus::Pending,
        }
    }

    /// 执行一步，自增步数；如果上一步使用了逃生通道，在本步开始时自动重置
    pub fn on_step_start(&mut self) {
        self.steps_count = self.steps_count.saturating_add(1);
        if self.status == BlockStatus::Pending {
            self.status = BlockStatus::InProgress;
        }
    }

    /// 本步结束，复位单步升维标记
    pub fn on_step_finish(&mut self) {
        self.active_escalation = false;
    }

    /// 尝试申请临时升维 Pro 援助（消耗 1 次配额）
    pub fn request_pro_escalation(&mut self) -> Result<(), String> {
        if self.escalate_quota == 0 {
            return Err("本阶段呼叫 Pro 强模型援助的配额已用尽（每个阶段最多 2 次），请继续使用 Flash 执行或更新方案".to_string());
        }
        self.escalate_quota = self.escalate_quota.saturating_sub(1);
        self.active_escalation = true;
        Ok(())
    }

    /// 动态扩展本块的允许改动范围 (Scope)
    pub fn expand_scope(&mut self, paths: &[String]) {
        for p in paths {
            let norm = p.trim().replace('\\', "/");
            if !norm.is_empty() && !self.scope.iter().any(|s| s.replace('\\', "/") == norm) {
                self.scope.push(norm);
            }
        }
    }

    /// 记录一次审查不合格/打回：返回 true 表示达到 3 次上限，触发熔断升维！
    pub fn record_review_failure(&mut self) -> bool {
        self.review_retry_count = self.review_retry_count.saturating_add(1);
        if self.review_retry_count >= MAX_BLOCK_REVIEW_RETRIES {
            // 审查死循环熔断：强制永久将该块升维至 Pro 强模型接管重构！
            self.tier = "pro".to_string();
            true
        } else {
            false
        }
    }

    /// 审查成功时复位重试计数
    pub fn reset_review_retries(&mut self) {
        self.review_retry_count = 0;
    }

    /// 检查是否已达到硬顶步数上限
    pub fn is_hard_limit_exceeded(&self) -> bool {
        self.steps_count >= self.hard_step_limit
    }

    /// 校验实际改动的文件是否超出了 scope 范围（如果 scope 为空则视为不限）
    pub fn find_scope_violations<'a>(&self, modified_files: &'a [String]) -> Vec<&'a String> {
        if self.scope.is_empty() {
            return Vec::new();
        }
        modified_files
            .iter()
            .filter(|file| {
                let norm = file.replace('\\', "/");
                !self.scope.iter().any(|allowed| {
                    let allowed_norm = allowed.replace('\\', "/");
                    norm == allowed_norm || norm.starts_with(&format!("{}/", allowed_norm.trim_end_matches('/')))
                })
            })
            .collect()
    }
}

/// 宿主管理的块状态机
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BlockManager {
    pub blocks: Vec<TargetBlock>,
    pub active_index: Option<usize>,
}

impl BlockManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_blocks(&mut self, blocks: Vec<TargetBlock>) {
        self.blocks = blocks;
        if !self.blocks.is_empty() {
            self.active_index = Some(0);
            self.blocks[0].status = BlockStatus::InProgress;
        } else {
            self.active_index = None;
        }
    }

    pub fn active_block(&self) -> Option<&TargetBlock> {
        let idx = self.active_index?;
        self.blocks.get(idx)
    }

    pub fn active_block_mut(&mut self) -> Option<&mut TargetBlock> {
        let idx = self.active_index?;
        self.blocks.get_mut(idx)
    }

    /// 推动到下一个块
    pub fn advance_to_next(&mut self) -> Option<&TargetBlock> {
        if let Some(idx) = self.active_index {
            if let Some(current) = self.blocks.get_mut(idx) {
                current.status = BlockStatus::Completed;
            }
            let next_idx = idx + 1;
            if next_idx < self.blocks.len() {
                self.active_index = Some(next_idx);
                if let Some(next) = self.blocks.get_mut(next_idx) {
                    next.status = BlockStatus::InProgress;
                }
                return self.blocks.get(next_idx);
            }
        }
        self.active_index = None;
        None
    }

    /// 标记所有块均已完成
    pub fn is_all_completed(&self) -> bool {
        !self.blocks.is_empty() && self.blocks.iter().all(|b| b.status == BlockStatus::Completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_block_lifecycle_and_quota() {
        let mut block = TargetBlock::new("block-1", "测试功能");
        assert_eq!(block.status, BlockStatus::Pending);
        assert_eq!(block.escalate_quota, 2);
        assert!(!block.active_escalation);

        // 第 1 步启动
        block.on_step_start();
        assert_eq!(block.status, BlockStatus::InProgress);
        assert_eq!(block.steps_count, 1);

        // 申请 Pro 支援逃生通道（第 1 次）
        assert!(block.request_pro_escalation().is_ok());
        assert!(block.active_escalation);
        assert_eq!(block.escalate_quota, 1);

        // 步数结算后，单步升维标记复位
        block.on_step_finish();
        assert!(!block.active_escalation);

        // 申请 Pro 支援逃生通道（第 2 次）
        assert!(block.request_pro_escalation().is_ok());
        assert_eq!(block.escalate_quota, 0);

        // 配额用尽后无法重复申请
        assert!(block.request_pro_escalation().is_err());
    }

    #[test]
    fn test_review_retry_circuit_breaker() {
        let mut block = TargetBlock::new("block-breaker", "熔断测试");
        assert_eq!(block.tier, "flash");

        // 前两次打回未达上限
        assert!(!block.record_review_failure());
        assert!(!block.record_review_failure());
        assert_eq!(block.tier, "flash");

        // 第三次打回触发熔断！自动永久升维至 pro 接管重写
        assert!(block.record_review_failure());
        assert_eq!(block.tier, "pro");
    }

    #[test]
    fn test_scope_violations_and_expansion() {
        let mut block = TargetBlock::new("block-scope", "边界检查");
        block.scope = vec!["src/agent/router.rs".to_string()];

        let modified = vec![
            "src/agent/router.rs".to_string(),
            "src/agent/types.rs".to_string(), // 越界
        ];

        // 初始未扩展时报越界
        let violations = block.find_scope_violations(&modified);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0], &"src/agent/types.rs".to_string());

        // 动态申请扩展 scope
        block.expand_scope(&["src/agent/types.rs".to_string()]);
        let violations_after = block.find_scope_violations(&modified);
        assert!(violations_after.is_empty(), "动态扩展后应合法放行");
    }

    #[test]
    fn test_block_manager_transitions() {
        let mut manager = BlockManager::new();
        let b1 = TargetBlock::new("b1", "阶段一");
        let b2 = TargetBlock::new("b2", "阶段二");
        manager.set_blocks(vec![b1, b2]);

        assert_eq!(manager.active_block().map(|b| b.id.as_str()), Some("b1"));
        assert!(!manager.is_all_completed());

        // 推进到下一个
        let next = manager.advance_to_next();
        assert_eq!(next.map(|b| b.id.as_str()), Some("b2"));
        assert_eq!(manager.blocks[0].status, BlockStatus::Completed);
        assert_eq!(manager.blocks[1].status, BlockStatus::InProgress);

        // 再次推进，完成所有
        let end = manager.advance_to_next();
        assert!(end.is_none());
        assert!(manager.is_all_completed());
    }
}

