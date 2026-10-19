    use super::is_gate_block_route;

    #[test]
    fn deadlock_routes_count_toward_escape_hatch() {
        // P3 三锁推诿循环的全部组成路由都应计入
        for route in [
            "plan_gatekeeper:reviewer_retry_limit",
            "plan_gatekeeper:need_human_approval",
            "plan_gatekeeper:review_revisions_required",
            "stage_review:blocking_open",
            "finish_review:blocking",
            "host_rejection:ask_human_cooldown",
            "host_rejection:ask_human_quota_exhausted",
            "host_rejection:ask_human_duplicate",
            "host_rejection:repeat_read",
            "parallel_contract_rejected",
            "plan_tree_parallel_rejected",
            "scope_guard:outside",
            "watch:rejected",
        ] {
            assert!(is_gate_block_route(route), "应计入死锁: {route}");
        }
    }

    #[test]
    fn real_work_and_real_failures_reset_counter() {
        // 成功产出与"模型尝试过但失败"的工具错误都不计入——后者由失败重试/无进展兜底
        for route in [
            "local_file:read:success",
            "local_file:edit:target_not_found",
            "local_file:missing_workspace",
            "tool_input:invalid_parameters",
            "child_worker:spawned",
            "child_reviewer:rejected_or_failed",
            "wait_jobs:resolved",
            "finish",
            "retrieval",
            "web_search_failed",
            "host_rejection:missing_child_run_id",
            "unknown",
        ] {
            assert!(!is_gate_block_route(route), "不应计入死锁: {route}");
        }
    }
