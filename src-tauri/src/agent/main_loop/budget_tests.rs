    use super::{effective_max_steps, token_budget_stop_reason, AgentRunBudget, MAX_STEPS};

    #[test]
    fn effective_max_steps_defaults_to_host_constant() {
        assert_eq!(effective_max_steps(None), MAX_STEPS);
        let budget = AgentRunBudget { max_steps: None, max_prompt_tokens: Some(1) };
        assert_eq!(effective_max_steps(Some(&budget)), MAX_STEPS);
    }

    #[test]
    fn effective_max_steps_case_override_wins() {
        let budget = AgentRunBudget { max_steps: Some(15), max_prompt_tokens: None };
        assert_eq!(effective_max_steps(Some(&budget)), 15);
    }

    #[test]
    fn token_budget_fires_only_at_or_above_cap() {
        let budget = AgentRunBudget { max_steps: None, max_prompt_tokens: Some(1000) };
        assert!(token_budget_stop_reason(Some(&budget), 999).is_none());
        let reason = token_budget_stop_reason(Some(&budget), 1000).unwrap();
        assert!(reason.contains("token 上限"));
        assert!(reason.contains("1000"));
    }

    #[test]
    fn token_budget_absent_means_unbounded() {
        let budget = AgentRunBudget { max_steps: Some(5), max_prompt_tokens: None };
        assert!(token_budget_stop_reason(Some(&budget), i64::MAX).is_none());
        assert!(token_budget_stop_reason(None, i64::MAX).is_none());
    }
