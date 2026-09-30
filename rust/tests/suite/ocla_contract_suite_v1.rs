//! Golden serde fixtures for the seven V1 contract records.
//!
//! The canonical typed records are exported by `lean-ctx-protocol`.  The
//! unknown-field cases are also parsed as JSON at the wire boundary because
//! the first five V1 projections currently use closed structs; capability and
//! knowledge projections retain additive fields with `serde(flatten)`.

use lean_ctx::core::conformance::{
    ConformanceResult, check_invocation_conformance, check_manifest_conformance,
};
use lean_ctx::core::ocla::invocation::CapabilityResult;
use lean_ctx_protocol::{
    AcceptedOutcomeV1, CapabilityManifestV1, DecisionRecordV1, ExecutionPlanV1, ExecutionReceiptV1,
    KnowledgeObjectV1, TaskEnvelopeV1, ValidationError,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};

fn fixture_path(directory: &str, name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/ocla_contract_suite/v1")
        .join(directory)
        .join(name)
}

fn fixture_body(directory: &str, name: &str) -> String {
    let path = fixture_path(directory, name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn conformance_fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/ocla_contract_suite/v1/conformance")
        .join(name)
}

fn parse_json(directory: &str, name: &str) -> Value {
    let path = fixture_path(directory, name);
    serde_json::from_str(&fixture_body(directory, name))
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn typed_round_trip<T, F>(directory: &str, validate: F)
where
    T: DeserializeOwned + Serialize + Debug + PartialEq,
    F: Fn(&T) -> Result<(), ValidationError>,
{
    let minimal: T = serde_json::from_str(&fixture_body(directory, "valid_minimal.json"))
        .unwrap_or_else(|error| panic!("{directory} minimal should deserialize: {error}"));
    validate(&minimal).unwrap_or_else(|error| panic!("{directory} minimal invalid: {error}"));
    let serialized = serde_json::to_string(&minimal)
        .unwrap_or_else(|error| panic!("{directory} minimal should serialize: {error}"));
    let decoded: T = serde_json::from_str(&serialized)
        .unwrap_or_else(|error| panic!("{directory} minimal reparse failed: {error}"));
    assert_eq!(minimal, decoded, "{directory} minimal round trip");

    let maximal: T = serde_json::from_str(&fixture_body(directory, "valid_maximal.json"))
        .unwrap_or_else(|error| panic!("{directory} maximal should deserialize: {error}"));
    validate(&maximal).unwrap_or_else(|error| panic!("{directory} maximal invalid: {error}"));
    let serialized = serde_json::to_string(&maximal)
        .unwrap_or_else(|error| panic!("{directory} maximal should serialize: {error}"));
    let decoded: T = serde_json::from_str(&serialized)
        .unwrap_or_else(|error| panic!("{directory} maximal reparse failed: {error}"));
    assert_eq!(maximal, decoded, "{directory} maximal round trip");

    assert!(
        serde_json::from_str::<T>(&fixture_body(directory, "invalid_missing_required.json"))
            .is_err(),
        "{directory} missing-required fixture must fail"
    );
    assert!(
        serde_json::from_str::<T>(&fixture_body(directory, "invalid_bad_enum.json")).is_err(),
        "{directory} bad-enum fixture must fail"
    );

    let unknown = parse_json(directory, "valid_unknown_field.json");
    assert!(
        unknown
            .as_object()
            .is_some_and(|object| object.keys().any(|key| key.starts_with("future_"))),
        "{directory} unknown fixture must contain an additive field"
    );
}

#[test]
fn task_envelope_v1_fixtures() {
    typed_round_trip::<TaskEnvelopeV1, _>("task-envelope", TaskEnvelopeV1::validate);
}

#[test]
fn execution_plan_v1_fixtures() {
    typed_round_trip::<ExecutionPlanV1, _>("execution-plan", ExecutionPlanV1::validate);
}

#[test]
fn execution_receipt_v1_fixtures() {
    typed_round_trip::<ExecutionReceiptV1, _>("execution-receipt", ExecutionReceiptV1::validate);
}

#[test]
fn accepted_outcome_v1_fixtures() {
    typed_round_trip::<AcceptedOutcomeV1, _>("accepted-outcome", AcceptedOutcomeV1::validate);
}

#[test]
fn capability_manifest_v1_fixtures() {
    typed_round_trip::<CapabilityManifestV1, _>(
        "capability-manifest",
        CapabilityManifestV1::validate,
    );
    let unknown: CapabilityManifestV1 = serde_json::from_str(&fixture_body(
        "capability-manifest",
        "valid_unknown_field.json",
    ))
    .expect("capability unknown field should deserialize");
    assert!(unknown.extra.contains_key("future_conformance_extension"));
}

#[test]
fn decision_record_v1_fixtures() {
    typed_round_trip::<DecisionRecordV1, _>("decision-record", DecisionRecordV1::validate);
}

#[test]
fn knowledge_object_v1_fixtures() {
    typed_round_trip::<KnowledgeObjectV1, _>("knowledge-object", KnowledgeObjectV1::validate);
    let unknown: KnowledgeObjectV1 = serde_json::from_str(&fixture_body(
        "knowledge-object",
        "valid_unknown_field.json",
    ))
    .expect("knowledge unknown field should deserialize");
    assert!(unknown.extra.contains_key("future_provenance"));
}

#[test]
fn conformance_fixtures_match_rust_golden_outputs() {
    let names = [
        "manifest_valid.json",
        "manifest_invalid_classification.json",
        "invocation_success.json",
        "invocation_failure_timeout.json",
        "invocation_failure_policy.json",
    ];
    let golden: Value =
        serde_json::from_str(include_str!("../fixtures/ocla_conformance_golden.json"))
            .expect("conformance golden should be valid JSON");

    for name in names {
        let path = conformance_fixture_path(name);
        let fixture: Value = serde_json::from_str(
            &fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
        )
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        let manifest: CapabilityManifestV1 = serde_json::from_value(fixture["manifest"].clone())
            .unwrap_or_else(|error| panic!("{name} manifest should deserialize: {error}"));
        let actual = if fixture["kind"] == "manifest" {
            check_manifest_conformance(&manifest)
        } else {
            let invocation: CapabilityResult = serde_json::from_value(fixture["result"].clone())
                .unwrap_or_else(|error| panic!("{name} result should deserialize: {error}"));
            check_invocation_conformance(&manifest, &invocation)
        };
        let expected: ConformanceResult = serde_json::from_value(fixture["expected"].clone())
            .unwrap_or_else(|error| panic!("{name} expected result should deserialize: {error}"));
        assert_eq!(actual, expected, "fixture {name} differs from Rust output");
        assert_eq!(
            serde_json::to_value(&actual).expect("conformance result should serialize"),
            golden[name],
            "fixture {name} differs from pinned Rust golden output"
        );

        let manifest_round_trip: CapabilityManifestV1 = serde_json::from_value(
            serde_json::to_value(&manifest).expect("manifest should serialize"),
        )
        .expect("manifest round trip should deserialize");
        assert_eq!(manifest, manifest_round_trip, "manifest {name} round trip");
        if fixture["kind"] == "invocation" {
            let invocation: CapabilityResult = serde_json::from_value(fixture["result"].clone())
                .expect("invocation should deserialize");
            let invocation_round_trip: CapabilityResult = serde_json::from_value(
                serde_json::to_value(&invocation).expect("invocation should serialize"),
            )
            .expect("invocation round trip should deserialize");
            assert_eq!(
                invocation, invocation_round_trip,
                "invocation {name} round trip"
            );
        }
    }
}
