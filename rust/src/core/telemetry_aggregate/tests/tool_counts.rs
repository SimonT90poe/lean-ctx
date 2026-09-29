// SPDX-License-Identifier: Apache-2.0

//! Per-tool call counting: deltas, persistence across processes, day totals.

use super::*;

fn tool_call_counts(batch: &TelemetryBatchV2) -> Vec<(String, u64, u64)> {
    batch
        .events
        .iter()
        .find_map(|envelope| match &envelope.event {
            TelemetryEventV2::ToolCallAggregate(metrics) => Some(
                metrics
                    .tools
                    .iter()
                    .map(|entry| (entry.tool.clone(), entry.calls, entry.failures))
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn counters(entries: &[(&str, u64, u64)]) -> BTreeMap<String, ToolCounterCheckpoint> {
    entries
        .iter()
        .map(|(tool, calls, failures)| {
            (
                (*tool).to_string(),
                ToolCounterCheckpoint {
                    calls: *calls,
                    failures: *failures,
                },
            )
        })
        .collect()
}

#[test]
fn tool_call_deltas_subtract_the_baseline_and_drop_invalid_names() {
    let observed = counters(&[
        ("ctx_read", 10, 3),
        ("ctx_shell", 4, 0),
        ("ctx_tree", 2, 0),
        ("Bad Name", 9, 0),
    ]);
    let baseline = counters(&[("ctx_read", 7, 1), ("ctx_tree", 2, 0)]);
    let deltas: Vec<_> = tool_call_deltas(&observed, &baseline)
        .into_iter()
        .map(|entry| (entry.tool, entry.calls, entry.failures))
        .collect();
    assert_eq!(
        deltas,
        vec![
            ("ctx_read".to_string(), 3, 2),
            ("ctx_shell".to_string(), 4, 0)
        ]
    );
}

#[test]
fn tool_call_deltas_keep_the_most_called_tools_past_the_entry_cap() {
    let observed: BTreeMap<_, _> = (0..MAX_TOOL_ENTRIES + 5)
        .map(|index| {
            (
                format!("tool_{index:04}"),
                ToolCounterCheckpoint {
                    calls: index as u64 + 1,
                    failures: 0,
                },
            )
        })
        .collect();
    let deltas = tool_call_deltas(&observed, &BTreeMap::new());
    assert_eq!(deltas.len(), MAX_TOOL_ENTRIES);
    assert!(deltas.iter().all(|entry| entry.calls > 5));
    ToolCallMetrics { tools: deltas }
        .validate()
        .expect("capped deltas satisfy the contract");
}

#[test]
fn checkpoint_written_before_per_tool_counting_still_loads() {
    let legacy = r#"{"tool_calls":3,"tool_failures":1,"tool_latency_buckets":[1,1,1,0,0,0,0,0,0],"session_uptime_secs":60}"#;
    let checkpoint: CounterCheckpoint = serde_json::from_str(legacy).expect("legacy checkpoint");
    assert!(checkpoint.tools.is_empty());
    assert_eq!(checkpoint.tool_calls, 3);
}

#[test]
#[serial_test::serial]
fn daily_batch_carries_one_setup_profile() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let batch = preview_daily_batch().expect("preview");
    let profiles: Vec<_> = batch
        .events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            TelemetryEventV2::SetupProfile(metrics) => Some(*metrics),
            _ => None,
        })
        .collect();
    assert_eq!(profiles, vec![setup_profile()]);
    batch.validate().expect("valid batch");
}

#[test]
#[serial_test::serial]
fn acknowledged_batch_keeps_cumulative_per_tool_day_totals() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    let metrics = crate::core::telemetry::global_metrics();
    metrics.record_named_tool_call("telemetry_probe_tool", 1_000, true);
    let first = prepare_daily_batch().expect("prepare");
    assert!(
        tool_call_counts(&first)
            .iter()
            .any(|(tool, _, _)| tool == "telemetry_probe_tool")
    );
    acknowledge_daily_batch(&first).expect("acknowledge");

    // The next same-day batch restates the whole day, not just the delta.
    metrics.record_named_tool_call("telemetry_probe_tool", 1_000, false);
    metrics.record_named_tool_call("telemetry_probe_tool", 1_000, true);
    let next = preview_daily_batch().expect("next preview");
    let probe: Vec<_> = tool_call_counts(&next)
        .into_iter()
        .filter(|(tool, _, _)| tool == "telemetry_probe_tool")
        .collect();
    assert_eq!(probe, vec![("telemetry_probe_tool".to_string(), 3, 1)]);
    next.validate().expect("valid batch");
}

fn probe_counts(batch: &TelemetryBatchV2, tool: &str) -> Option<(u64, u64)> {
    tool_call_counts(batch)
        .into_iter()
        .find(|(name, _, _)| name == tool)
        .map(|(_, calls, failures)| (calls, failures))
}

fn queued_counters() -> CounterCheckpoint {
    sidecar()
        .days
        .get(&current_send_bucket())
        .map(|day| day.totals.counters.clone())
        .unwrap_or_default()
}

#[test]
#[serial_test::serial]
fn persisted_counters_are_sent_cumulatively_and_later_calls_reach_the_right_day() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    let metrics = crate::core::telemetry::global_metrics();
    metrics.record_named_tool_call("telemetry_fold_probe", 1_000, true);
    metrics.record_named_tool_call("telemetry_fold_probe", 1_000, false);
    persist_process_counters().expect("persist");
    assert_eq!(
        queued_counters().tools.get("telemetry_fold_probe").copied(),
        Some(ToolCounterCheckpoint {
            calls: 2,
            failures: 1
        })
    );
    // A second fold with no new calls must not count them again.
    persist_process_counters().expect("idempotent persist");

    let lease = begin_daily_send().expect("send");
    assert_eq!(
        probe_counts(lease.batch(), "telemetry_fold_probe"),
        Some((2, 1))
    );
    lease.commit().expect("commit");
    // The day keeps its running total; the ack only marks it as sent.
    assert_eq!(
        queued_counters().tools.get("telemetry_fold_probe").copied(),
        Some(ToolCounterCheckpoint {
            calls: 2,
            failures: 1
        })
    );
    assert!(!today_is_unsent());

    // A later same-day call goes out with the next spaced send, on the same day.
    metrics.record_named_tool_call("telemetry_fold_probe", 1_000, true);
    persist_process_counters().expect("persist after send");
    assert!(today_is_unsent());
    assert!(
        begin_daily_send()
            .err()
            .expect("resend is spaced")
            .contains("not due")
    );
    let _clock = TestClockGuard::set(BUCKET, T0 + RESEND_INTERVAL_SECS);
    let resend = begin_daily_send().expect("same-day resend");
    assert_eq!(
        probe_counts(resend.batch(), "telemetry_fold_probe"),
        Some((3, 1))
    );
    assert_eq!(
        event_buckets(resend.batch()),
        BTreeSet::from([BUCKET.into()])
    );
    resend.commit().expect("commit resend");

    // The next day starts from zero under its own bucket.
    let _clock = TestClockGuard::set(NEXT_BUCKET, NEXT_T0);
    metrics.record_named_tool_call("telemetry_fold_probe", 1_000, true);
    persist_process_counters().expect("persist next day");
    let next = begin_daily_send().expect("next send");
    assert_eq!(
        probe_counts(next.batch(), "telemetry_fold_probe"),
        Some((1, 0))
    );
    assert_eq!(
        event_buckets(next.batch()),
        BTreeSet::from([NEXT_BUCKET.into()])
    );
    next.batch().validate().expect("valid batch");
}

#[test]
#[serial_test::serial]
fn counters_persisted_by_an_exited_process_are_included() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _clock = TestClockGuard::set(BUCKET, T0);
    // Absorb this process's counters so only the other process's remain.
    persist_process_counters().expect("baseline");
    let path = one_shot_path().expect("sidecar path");
    let mut sidecar = load_one_shots_at(&path).expect("sidecar");
    day_totals(&mut sidecar, BUCKET).counters = CounterCheckpoint::default();
    let mut buckets = [0; crate::core::telemetry::TOOL_LATENCY_BUCKET_UPPER_MS.len()];
    buckets[0] = 3;
    add_counters(
        &mut day_totals(&mut sidecar, BUCKET).counters,
        &CounterCheckpoint {
            tool_calls: 3,
            tool_failures: 1,
            tool_latency_buckets: buckets,
            session_uptime_secs: 30,
            tools: counters(&[("telemetry_exited_probe", 3, 1)]),
        },
    );
    write_one_shots(&path, &sidecar).expect("seed exited process counters");

    let preview = preview_daily_batch().expect("preview");
    assert_eq!(
        probe_counts(&preview, "telemetry_exited_probe"),
        Some((3, 1))
    );
    assert!(tool_counts(&preview).0 >= 3);
    // The preview folded in memory only.
    assert_eq!(queued_counters().tool_calls, 3);
    let lease = begin_daily_send().expect("send");
    assert_eq!(
        probe_counts(lease.batch(), "telemetry_exited_probe"),
        Some((3, 1))
    );
    lease.commit().expect("commit");
    assert!(!today_is_unsent());
}

#[test]
#[serial_test::serial]
fn calls_made_while_telemetry_is_off_are_never_back_filled() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    persist_process_counters().expect("baseline");
    let metrics = crate::core::telemetry::global_metrics();
    {
        let _off = TelemetryEnvGuard::disable();
        metrics.record_named_tool_call("telemetry_optout_probe", 1_000, true);
        persist_process_counters().expect("skip while off");
    }
    persist_process_counters().expect("persist after re-enable");
    assert!(
        !queued_counters()
            .tools
            .contains_key("telemetry_optout_probe")
    );
}

#[test]
fn sidecar_written_before_durable_counters_still_loads() {
    let mut legacy = serde_json::to_value(OneShotState::default()).expect("encode");
    let mut queued = serde_json::to_value(QueuedOneShots::default()).expect("encode queue");
    queued
        .as_object_mut()
        .expect("queued object")
        .remove("counters");
    legacy["queued"] = queued;
    let state: OneShotState = serde_json::from_value(legacy).expect("legacy sidecar");
    assert_eq!(state.queued.counters, CounterCheckpoint::default());
}
