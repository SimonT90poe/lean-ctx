//! Runtime integration helpers for the Context Control Kernel.

use super::enforce::{KernelMode, enforce_plan, resolve_mode};
use super::orchestrator::ContextKernel;
use super::policy::ContextPolicy;
use super::types::{ContextPlanV1, ContextReceiptV1, PlanEntry, ReceiptOutcome, RetrievalContext};

/// Result of kernel gating: what to add and what to suppress.
#[derive(Debug, Clone)]
pub struct KernelVerdict {
    /// Cross-store context to supplement (hard-capped, never exceeds budget).
    pub supplement: Option<String>,
    /// Content identifiers that are already in context and should not be resent.
    pub suppress: Vec<String>,
    /// Tokens consumed by the kernel supplement.
    pub budget_used: usize,
}
/// Result of kernel enrichment for compose integration.
#[derive(Debug, Clone)]
pub struct KernelEnrichment {
    /// The selection plan that produced the injected blocks.
    pub plan: ContextPlanV1,
    /// Human-readable blocks suitable for compose output injection.
    pub blocks: String,
    /// Gate decision accompanying the backward-compatible blocks.
    pub verdict: KernelVerdict,
    /// Kernel policy mode used while producing this enrichment.
    pub enforced_mode: KernelMode,
}
/// Check whether `path` should be suppressed as already delivered.
/// Returns `false` until the orchestrator exposes recent-delivery state.
pub fn kernel_gate(_path: &str, _project_root: &str) -> bool {
    false
}
/// Enrich a compose response with kernel-selected context.
///
/// Returns cross-store context that the compose pipeline misses, or `None`.
pub fn kernel_enrich(
    task: &str,
    project_root: &str,
    budget_tokens: usize,
) -> Option<KernelEnrichment> {
    let capped_budget = budget_tokens.min(150);
    let kernel = ContextKernel::for_project(project_root);
    let ctx = RetrievalContext {
        query: task.to_owned(),
        task: Some(task.to_owned()),
        project_root: project_root.to_owned(),
        budget: crate::core::context_field::TokenBudget {
            total: capped_budget,
            used: 0,
        },
        max_candidates: 20,
    };
    let mode = resolve_mode(project_root);
    let plan = enforce_plan_for_mode(kernel.plan(&ctx), &ContextPolicy::default(), mode);
    let enrichments: Vec<&PlanEntry> = plan
        .selected
        .iter()
        .filter(|entry| entry.provider != "context.ledger")
        .collect();

    let blocks = format_enrichment_blocks(&enrichments);
    enrichment_from_plan(plan, blocks, capped_budget, mode)
}

fn enforce_plan_for_mode(
    plan: ContextPlanV1,
    policy: &ContextPolicy,
    mode: KernelMode,
) -> ContextPlanV1 {
    if mode != KernelMode::Enforce {
        return plan;
    }

    let result = enforce_plan(&plan, policy, mode);
    if !result.blocked.is_empty() {
        tracing::debug!(
            blocked = result.blocked.len(),
            "kernel enforce: plan entries blocked by policy"
        );
    }

    ContextPlanV1 {
        selected: result.allowed,
        ..plan
    }
}

fn enrichment_from_plan(
    plan: ContextPlanV1,
    blocks: String,
    budget: usize,
    enforced_mode: KernelMode,
) -> Option<KernelEnrichment> {
    let verdict = verdict_from_blocks(blocks, budget);
    let blocks = verdict.supplement.clone()?;
    Some(KernelEnrichment {
        plan,
        blocks,
        verdict,
        enforced_mode,
    })
}

fn verdict_from_blocks(blocks: String, budget: usize) -> KernelVerdict {
    let blocks = truncate_to_token_budget(blocks, budget);
    let supplement = (!blocks.is_empty()).then(|| blocks.clone());
    let budget_used = supplement
        .as_deref()
        .map_or(0, crate::core::tokens::count_tokens);
    KernelVerdict {
        supplement,
        suppress: Vec::new(),
        budget_used,
    }
}

fn truncate_to_token_budget(mut text: String, budget: usize) -> String {
    if crate::core::tokens::count_tokens(&text) <= budget {
        return text;
    }
    let mut low = 0;
    let mut high = text.len();
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let boundary = text.floor_char_boundary(middle);
        if crate::core::tokens::count_tokens(&text[..boundary]) <= budget {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    text.truncate(text.floor_char_boundary(low));
    text
}

fn format_enrichment_blocks(entries: &[&PlanEntry]) -> String {
    let mut out = String::new();
    append_provider_block(&mut out, entries, "knowledge.facts", "Relevant Knowledge");
    append_provider_block(&mut out, entries, "memory.episodic", "Relevant Episodes");
    append_provider_block(
        &mut out,
        entries,
        "memory.procedural",
        "Relevant Procedures",
    );
    append_provider_block(&mut out, entries, "session.state", "Relevant Session State");
    out
}

fn append_provider_block(
    output: &mut String,
    entries: &[&PlanEntry],
    provider: &str,
    heading: &str,
) {
    let mut found = false;
    for entry in entries
        .iter()
        .copied()
        .filter(|entry| entry.provider == provider)
        .filter(|entry| !entry.reason.trim().is_empty())
        .filter(|entry| entry.reason != "selected by compiler")
    {
        if !found {
            output.push_str("\n## ");
            output.push_str(heading);
            output.push('\n');
            found = true;
        }
        output.push_str("- ");
        output.push_str(&entry.reason);
        output.push_str(" (phi=");
        output.push_str(&format!("{:.2}", entry.phi));
        output.push_str(")\n");
    }
}

/// Update the bandit-learned FieldWeights based on a receipt outcome.
///
/// Accepted outcomes reinforce the balanced arm, rejected outcomes penalize
/// the aggressive arm, and partial outcomes inform the conservative arm.
pub fn apply_feedback(receipt: &ContextReceiptV1) {
    use crate::core::context_field::{FieldWeights, set_active_weights};

    let arm_name = match receipt.outcome {
        ReceiptOutcome::Accepted => "balanced",
        ReceiptOutcome::Rejected => "aggressive",
        ReceiptOutcome::Partial => "conservative",
        ReceiptOutcome::Unknown => return,
    };
    let mut bandit = crate::core::bandit::ThresholdBandit::default();
    bandit.update(arm_name, receipt.outcome == ReceiptOutcome::Accepted);

    let best_idx = bandit.best_arm_idx_by_mean();
    if let Some(best_arm) = bandit.arms.get(best_idx) {
        set_active_weights(FieldWeights::from_arm(best_arm));
    }
}

/// Format a plan as a compact human-readable summary.
pub fn format_plan_summary(plan: &ContextPlanV1) -> String {
    let mut out = String::new();
    let plan_prefix = &plan.plan_id[..plan.plan_id.len().min(8)];
    out.push_str(&format!(
        "[kernel] plan={plan_prefix} intent=\"{}\" budget={}/{}\n",
        plan.intent, plan.budget.used_tokens, plan.budget.total_tokens,
    ));
    out.push_str(&format!(
        "  selected={} excluded={} deferred={}\n",
        plan.selected.len(),
        plan.excluded.len(),
        plan.deferred.len(),
    ));

    let mut providers: Vec<_> = plan.provider_stats.iter().collect();
    providers.sort_unstable_by_key(|(k, _)| *k);
    for (provider, stat) in providers {
        out.push_str(&format!(
            "  {provider}: {}/{} candidates, {} tokens\n",
            stat.candidates_selected, stat.candidates_offered, stat.tokens_used,
        ));
    }
    out
}

#[cfg(test)]
pub mod tests {
    use std::collections::HashMap;

    use super::super::enforce::KernelMode;
    use super::super::policy::ContextPolicy;
    use super::super::types::PlanBudget;
    use super::{
        ContextPlanV1, PlanEntry, enforce_plan_for_mode, enrichment_from_plan,
        format_enrichment_blocks, kernel_gate, verdict_from_blocks,
    };

    fn plan(selected: Vec<PlanEntry>) -> ContextPlanV1 {
        ContextPlanV1 {
            plan_id: "plan".to_owned(),
            intent: "test".to_owned(),
            budget: PlanBudget {
                total_tokens: 150,
                used_tokens: 0,
                remaining_tokens: 150,
            },
            selected,
            excluded: Vec::new(),
            deferred: Vec::new(),
            provider_stats: HashMap::new(),
        }
    }

    fn entry(reason: &str) -> PlanEntry {
        PlanEntry {
            object_id: "fact".to_owned(),
            provider: "knowledge.facts".to_owned(),
            view: "summary".to_owned(),
            tokens: 1,
            phi: 0.8,
            reason: reason.to_owned(),
        }
    }

    #[test]
    fn budget_capped_at_150() {
        let item = entry(&"token ".repeat(1_000));
        let blocks = format_enrichment_blocks(&[&item]);
        let enrichment = enrichment_from_plan(plan(vec![item]), blocks, 150, KernelMode::Shadow)
            .expect("long enrichment should be truncated, not removed");
        assert!(enrichment.verdict.budget_used <= 150);
    }
    #[test]
    fn empty_supplement_when_no_candidates() {
        let verdict = verdict_from_blocks(String::new(), 150);
        assert!(verdict.supplement.is_none());
        assert_eq!(verdict.budget_used, 0);
    }

    #[test]
    fn compiler_selection_placeholders_are_omitted() {
        let placeholder = entry("selected by compiler");

        assert!(format_enrichment_blocks(&[&placeholder]).is_empty());
    }
    #[test]
    fn verdict_has_correct_budget_used() {
        let item = entry("Known constraint");
        let blocks = format_enrichment_blocks(&[&item]);
        let enrichment = enrichment_from_plan(plan(vec![item]), blocks, 150, KernelMode::Shadow)
            .expect("entry should produce enrichment");
        assert_eq!(
            enrichment.verdict.budget_used,
            crate::core::tokens::count_tokens(enrichment.verdict.supplement.as_deref().unwrap())
        );
    }
    #[test]
    fn kernel_gate_returns_false_by_default() {
        assert!(!kernel_gate("src/lib.rs", "/project"));
    }
    #[test]
    fn backward_compat_enrichment_still_works() {
        let item = entry("Known constraint");
        let blocks = format_enrichment_blocks(&[&item]);
        let enrichment = enrichment_from_plan(plan(vec![item]), blocks, 150, KernelMode::Shadow)
            .expect("entry should produce enrichment");
        assert_eq!(enrichment.blocks, enrichment.verdict.supplement.unwrap());
        assert_eq!(enrichment.enforced_mode, KernelMode::Shadow);
    }

    #[test]
    fn test_enforce_mode_blocks_policy_violations() {
        let mut blocked = entry("blocked");
        blocked.provider = "excluded.provider".to_owned();
        let policy = ContextPolicy {
            blocked_sources: vec![blocked.provider.clone()],
            ..ContextPolicy::default()
        };

        let enforced = enforce_plan_for_mode(
            plan(vec![entry("allowed"), blocked]),
            &policy,
            KernelMode::Enforce,
        );

        assert_eq!(enforced.selected.len(), 1);
        assert_eq!(enforced.selected[0].reason, "allowed");
    }

    #[test]
    fn test_shadow_mode_allows_all_entries() {
        let mut blocked = entry("blocked");
        blocked.provider = "excluded.provider".to_owned();
        let policy = ContextPolicy {
            blocked_sources: vec![blocked.provider.clone()],
            ..ContextPolicy::default()
        };

        let shadow = enforce_plan_for_mode(
            plan(vec![entry("allowed"), blocked]),
            &policy,
            KernelMode::Shadow,
        );

        assert_eq!(shadow.selected.len(), 2);
    }
}
