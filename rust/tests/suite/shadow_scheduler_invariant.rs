//! Invariants for the public shadow scheduler boundary.

use std::collections::BTreeMap;

use lean_ctx::core::ocla::{
    CatalogueEntry, PolicyConstraints, ReferenceScheduler, TechnicalCatalogue,
};
use lean_ctx_protocol::{
    CapabilityId, CapabilityKind, CapabilityManifestV1, DataClassification, DataMovement,
    Determinism, MeasurementSupportV1, Reversibility, SurfaceSupportV1, TaskComplexity,
    TaskEnvelopeV1,
};

fn manifest(capability_id: &str, provider: &str) -> CapabilityManifestV1 {
    CapabilityManifestV1 {
        schema_version: 1,
        capability_id: CapabilityId::try_from(capability_id.to_owned()).expect("capability id"),
        provider: provider.to_owned(),
        kind: CapabilityKind::Tool,
        version: "1.0.0".to_owned(),
        surfaces: vec!["context".to_owned()],
        support_matrix: BTreeMap::from([(
            "context".to_owned(),
            SurfaceSupportV1 {
                supported: true,
                input_schema_ref: None,
                output_schema_ref: None,
            },
        )]),
        local: true,
        remote: false,
        reversibility: Reversibility::Reversible,
        determinism: Determinism::Deterministic,
        data_movement: DataMovement::LocalOnly,
        supported_classifications: vec![DataClassification::Public],
        measurement_support: MeasurementSupportV1 {
            latency: true,
            tokens: true,
            quality: true,
        },
        input_schema_ref: None,
        output_schema_ref: None,
        conformance_version: 1,
        extra: BTreeMap::new(),
    }
}

fn envelope() -> TaskEnvelopeV1 {
    TaskEnvelopeV1 {
        schema_version: 1,
        task_id: "task-shadow".try_into().expect("task id"),
        trace_id: "trace-shadow".try_into().expect("trace id"),
        project_id: "project-shadow".try_into().expect("project id"),
        session_id: "session-shadow".try_into().expect("session id"),
        agent_id: "agent-shadow".try_into().expect("agent id"),
        complexity: TaskComplexity::Low,
        created_at: "2026-08-09T00:00:00Z".to_owned(),
        parent_task_id: None,
        tenant_id: None,
        intent: Some("inspect".to_owned()),
        task_class: None,
        risk_class: None,
        quality_requirement_milli: Some(700),
        cost_budget_micros: Some(100),
        latency_budget_ms: Some(500),
        data_classification: Some(DataClassification::Public),
        region_policy_ref: None,
        model_policy_ref: None,
        context_state_ref: None,
        outcome_contract_ref: None,
    }
}

#[test]
fn recommendation_does_not_execute_a_capability() {
    let capability = manifest("capability://shadow", "local");
    let catalogue = TechnicalCatalogue {
        capabilities: vec![CatalogueEntry {
            capability_id: capability.capability_id.as_str().to_owned(),
            version: capability.version.clone(),
            manifest: capability.clone(),
            available: true,
        }],
        ..TechnicalCatalogue::default()
    };
    let decision = ReferenceScheduler::new()
        .schedule(
            &envelope(),
            std::slice::from_ref(&capability),
            &catalogue,
            &PolicyConstraints::default(),
        )
        .expect("reference recommendation");

    assert_eq!(decision.selected.task_id, envelope().task_id);
    assert_ne!(decision.selected, decision.fallback);
    assert!(!decision.decision_ref.is_empty());
    assert!(!decision.rationale_code.is_empty());
    // A recommendation is a value only; no adapter invocation is reachable.
}

#[test]
fn fallback_and_candidate_accounting_are_always_present() {
    let decision = ReferenceScheduler::new()
        .schedule(
            &envelope(),
            &[],
            &TechnicalCatalogue::default(),
            &PolicyConstraints::default(),
        )
        .expect("fallback recommendation");
    assert_eq!(decision.selected, decision.fallback);
    assert_eq!(decision.candidates_evaluated, 0);
    assert_eq!(decision.candidates_excluded, 0);
    assert_eq!(decision.fallback.capability_ids.len(), 1);
}

#[test]
fn public_catalogue_contains_no_private_economic_fields() {
    let catalogue = TechnicalCatalogue {
        models: vec![lean_ctx::core::ocla::ModelEntry {
            model_id: "model-public".to_owned(),
            context_window: 128_000,
            supports_reasoning: true,
            supports_streaming: true,
        }],
        providers: vec![lean_ctx::core::ocla::ProviderEntry {
            provider_id: "provider-public".to_owned(),
            models_available: vec!["model-public".to_owned()],
            regions: vec!["CH".to_owned()],
        }],
        ..TechnicalCatalogue::default()
    };
    let output = serde_json::to_string(&catalogue).expect("catalogue serialization");
    for forbidden in [
        "price",
        "rate",
        "performance",
        "score",
        "weight",
        "capacity",
    ] {
        assert!(
            !output.contains(forbidden),
            "private field leaked: {forbidden}"
        );
    }
}
