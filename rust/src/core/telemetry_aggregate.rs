// SPDX-License-Identifier: Apache-2.0

//! Privacy-safe daily telemetry aggregation.

use sha2::Digest;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::installation_id;
use super::telemetry_v2::{
    Architecture, ClientFamily, DecisionMetrics, DistributionChannel, EmbeddingsState,
    ErrorCategory, ErrorMetrics, HeartbeatMetrics, Histogram, IntegrationMode, MAX_COUNT,
    MAX_TOOL_ENTRIES, OccurrenceMetrics, OperatingSystem, SCHEMA_VERSION, SessionMetrics,
    SetupProfileMetrics, SyncMetrics, TelemetryBatchV2, TelemetryEnvelopeV2, TelemetryEventV2,
    ToolCallCount, ToolCallMetrics, ToolUsageMetrics, VersionUpgradeMetrics, valid_tool_name,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CounterCheckpoint {
    tool_calls: u64,
    tool_failures: u64,
    tool_latency_buckets: [u64; crate::core::telemetry::TOOL_LATENCY_BUCKET_UPPER_MS.len()],
    session_uptime_secs: u64,
    /// Per-tool counters. Absent in state written before per-tool counting.
    #[serde(default)]
    tools: BTreeMap<String, ToolCounterCheckpoint>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCounterCheckpoint {
    calls: u64,
    failures: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AggregateState {
    #[serde(default)]
    installation_id: String,
    /// Legacy in-process baseline, kept so older state files still load.
    /// Unsent counters now live durably in [`QueuedOneShots::counters`].
    process_nonce: String,
    acknowledged: CounterCheckpoint,
    pending: Option<PendingBatch>,
    #[serde(default)]
    last_sent_bucket: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OneShotState {
    #[serde(default)]
    installation_id: String,
    setup_recorded: bool,
    configured_integrations: BTreeSet<String>,
    observed_major: Option<u16>,
    #[serde(default)]
    last_acknowledged_batch: Option<String>,
    queued: QueuedOneShots,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueuedOneShots {
    setup_completed: bool,
    integrations_detected: u64,
    version_upgrade: Option<VersionTransition>,
    #[serde(default)]
    sync: SyncMetrics,
    #[serde(default)]
    autopilot: DecisionMetrics,
    #[serde(default)]
    autopilot_fallback: DecisionMetrics,
    #[serde(default)]
    checkout_started: u64,
    #[serde(default)]
    error_categories: [u64; 8],
    /// Tool counters folded in by every process and not yet acknowledged, so
    /// short sessions and calls after the daily send reach the next batch.
    #[serde(default)]
    counters: CounterCheckpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionTransition {
    from_major: u16,
    to_major: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingBatch {
    batch: TelemetryBatchV2,
    #[serde(default)]
    acknowledgement_id: String,
    observed: CounterCheckpoint,
    process_nonce: String,
    #[serde(default)]
    included_one_shots: QueuedOneShots,
}

pub struct DailySendLease {
    batch: TelemetryBatchV2,
    state_path: PathBuf,
    one_shot_path: PathBuf,
    _lock: std::fs::File,
}

impl DailySendLease {
    pub fn batch(&self) -> &TelemetryBatchV2 {
        &self.batch
    }

    pub fn commit(self) -> Result<(), String> {
        let current_state = load_state_at(&self.state_path)?;
        let pending = current_state
            .pending
            .as_ref()
            .ok_or_else(|| "no prepared telemetry batch to acknowledge".to_string())?;
        let included = pending.included_one_shots.clone();
        let acknowledgement_id = pending_acknowledgement_id(pending)?;
        let state = acknowledge_state(current_state, &self.batch)?;
        acknowledge_one_shots_at(&self.one_shot_path, &included, &acknowledgement_id)?;
        write_state(&self.state_path, &state)?;
        Ok(())
    }
}

pub fn pending_daily_batch() -> Result<TelemetryBatchV2, String> {
    preview_daily_batch()
}

/// Build the exact currently eligible payload without advancing durable state.
pub fn preview_daily_batch() -> Result<TelemetryBatchV2, String> {
    let path = state_path()?;
    ensure_parent(&path)?;
    let lock = open_state_lock(&path)?;
    lock.try_lock_exclusive().map_err(|error| {
        if super::file_lock::is_contended(&error) {
            "exact telemetry preview unavailable while a send is in progress".to_string()
        } else {
            format!("cannot lock telemetry aggregate state for preview: {error}")
        }
    })?;
    let state = state_for_current_identity(load_state_at(&path)?)?;
    if let Some(pending) = state.pending {
        return Ok(pending.batch);
    }
    let sidecar_path = one_shot_path()?;
    ensure_parent(&sidecar_path)?;
    let sidecar_lock = open_sidecar_lock(&sidecar_path)?;
    // Tool calls hold this lock briefly to persist counters; wait out that
    // window instead of failing a concurrent preview.
    lock_telemetry_file(&sidecar_lock, "one-shot").map_err(|error| {
        format!("exact telemetry preview unavailable: cannot lock one-shot state: {error}")
    })?;
    let mut one_shots = one_shots_for_current_identity(load_one_shots_at(&sidecar_path)?)?;
    // Fold in memory only: the preview must not advance durable state.
    fold_process_counters(&sidecar_path, &mut one_shots);
    build_from_queued(&one_shots.queued)
}

/// Freeze one payload until the sender explicitly acknowledges success.
#[cfg(test)]
fn prepare_daily_batch() -> Result<TelemetryBatchV2, String> {
    with_locked_state(|mut state| {
        if let Some(pending) = &state.pending {
            return Ok((state.clone(), pending.batch.clone()));
        }
        let sidecar_path = one_shot_path()?;
        ensure_parent(&sidecar_path)?;
        let sidecar_lock = open_sidecar_lock(&sidecar_path)?;
        lock_telemetry_file(&sidecar_lock, "one-shot")?;
        let mut one_shots = one_shots_for_current_identity(load_one_shots_at(&sidecar_path)?)?;
        let observed = fold_process_counters(&sidecar_path, &mut one_shots);
        write_one_shots(&sidecar_path, &one_shots)?;
        mark_folded(&sidecar_path, observed.clone());
        let batch = build_from_queued(&one_shots.queued)?;
        state.installation_id = batch_installation_id(&batch).to_string();
        state.pending = Some(PendingBatch {
            batch: batch.clone(),
            acknowledgement_id: uuid::Uuid::new_v4().to_string(),
            observed,
            process_nonce: process_nonce().to_string(),
            included_one_shots: one_shots.queued,
        });
        Ok((state, batch))
    })
}

/// Hold the cross-process state lease until network, ledger, and acknowledgement finish.
pub fn begin_daily_send() -> Result<DailySendLease, String> {
    begin_daily_send_in_bucket(None)
}

/// Resolve one UTC bucket under the lock for both admission and payload.
/// Tests inject a fixed bucket; pending batches retain their original bytes.
fn begin_daily_send_in_bucket(bucket: Option<&str>) -> Result<DailySendLease, String> {
    let path = state_path()?;
    let one_shot_path = one_shot_path()?;
    ensure_parent(&path)?;
    let lock = open_state_lock(&path)?;
    lock_telemetry_file(&lock, "aggregate")?;
    let mut state = state_for_current_identity(load_state()?)?;
    let batch = if let Some(pending) = &state.pending {
        // Retries remain at-least-once, including across UTC day boundaries.
        pending.batch.clone()
    } else {
        // A caller-side precheck cannot serialize competing senders.
        let bucket = match bucket {
            Some(bucket) => bucket.to_string(),
            None => current_send_bucket(),
        };
        if state.last_sent_bucket.as_deref() == Some(bucket.as_str()) {
            return Err(format!(
                "telemetry daily batch for {bucket} was already sent"
            ));
        }
        ensure_parent(&one_shot_path)?;
        let one_shot_lock = open_sidecar_lock(&one_shot_path)?;
        lock_telemetry_file(&one_shot_lock, "one-shot")?;
        let mut one_shots = one_shots_for_current_identity(load_one_shots_at(&one_shot_path)?)?;
        let observed = fold_process_counters(&one_shot_path, &mut one_shots);
        write_one_shots(&one_shot_path, &one_shots)?;
        mark_folded(&one_shot_path, observed.clone());
        let batch = build_in_bucket(&one_shots.queued, bucket)?;
        state.installation_id = batch_installation_id(&batch).to_string();
        state.pending = Some(PendingBatch {
            batch: batch.clone(),
            acknowledgement_id: uuid::Uuid::new_v4().to_string(),
            observed,
            process_nonce: process_nonce().to_string(),
            included_one_shots: one_shots.queued,
        });
        write_state(&path, &state)?;
        batch
    };
    Ok(DailySendLease {
        batch,
        state_path: path,
        one_shot_path,
        _lock: lock,
    })
}

/// Advance counters only when the exact frozen payload was accepted remotely.
#[cfg(test)]
fn acknowledge_daily_batch(batch: &TelemetryBatchV2) -> Result<(), String> {
    let path = state_path()?;
    ensure_parent(&path)?;
    let lock = open_state_lock(&path)?;
    lock.lock_exclusive()
        .map_err(|error| format!("cannot lock telemetry aggregate state: {error}"))?;
    let state = load_state_at(&path)?;
    let pending = state
        .pending
        .as_ref()
        .ok_or_else(|| "no prepared telemetry batch to acknowledge".to_string())?;
    let included = pending.included_one_shots.clone();
    let acknowledgement_id = pending_acknowledgement_id(pending)?;
    let state = acknowledge_state(state, batch)?;
    acknowledge_one_shots_at(&one_shot_path()?, &included, &acknowledgement_id)?;
    write_state(&path, &state)?;
    Ok(())
}

fn pending_acknowledgement_id(pending: &PendingBatch) -> Result<String, String> {
    if !pending.acknowledgement_id.is_empty() {
        return Ok(pending.acknowledgement_id.clone());
    }
    let encoded = serde_json::to_vec(&pending.batch)
        .map_err(|error| format!("cannot encode telemetry acknowledgement: {error}"))?;
    Ok(hex::encode(sha2::Sha256::digest(encoded)))
}

fn acknowledge_state(
    mut state: AggregateState,
    batch: &TelemetryBatchV2,
) -> Result<AggregateState, String> {
    let pending = state
        .pending
        .take()
        .ok_or_else(|| "no prepared telemetry batch to acknowledge".to_string())?;
    if pending.batch != *batch {
        return Err("telemetry acknowledgement does not match pending batch".to_string());
    }
    state.process_nonce = pending.process_nonce;
    state.acknowledged = pending.observed;
    state.last_sent_bucket = batch
        .events
        .first()
        .map(|event| event.timestamp_bucket.clone());
    Ok(state)
}

pub fn last_sent_bucket() -> Option<String> {
    load_state()
        .and_then(state_for_current_identity)
        .ok()
        .and_then(|state| state.last_sent_bucket)
}

/// UTC bucket shared by payload generation and the background caller's precheck.
pub(crate) fn current_send_bucket() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn state_for_current_identity(mut state: AggregateState) -> Result<AggregateState, String> {
    let (current, _) = installation_id::get_or_create_identity()
        .map_err(|error| format!("installation ID unavailable: {error}"))?;
    let pending_mismatch = state.pending.as_ref().is_some_and(|pending| {
        let pending_id = batch_installation_id(&pending.batch);
        pending_id.is_empty() || pending_id != current
    });
    if pending_mismatch || (!state.installation_id.is_empty() && state.installation_id != current) {
        state = AggregateState {
            installation_id: current,
            ..AggregateState::default()
        };
    } else if state.installation_id.is_empty() {
        state.installation_id = current;
    }
    Ok(state)
}

fn batch_installation_id(batch: &TelemetryBatchV2) -> &str {
    batch
        .events
        .first()
        .map_or("", |event| event.installation_id.as_str())
}

fn build_from_queued(queued: &QueuedOneShots) -> Result<TelemetryBatchV2, String> {
    build_in_bucket(queued, current_send_bucket())
}

/// Build the payload for one explicit bucket. Split out so a caller that has
/// already resolved the bucket under a lock stamps that exact value instead of
/// reading the clock a second time.
fn build_in_bucket(queued: &QueuedOneShots, bucket: String) -> Result<TelemetryBatchV2, String> {
    let (installation_id, deletion_token) = installation_id::get_or_create_identity()
        .map_err(|error| format!("installation ID unavailable: {error}"))?;
    build_daily_aggregate(
        installation_id,
        hex::encode(sha2::Sha256::digest(deletion_token.as_bytes())),
        bucket,
        distribution_channel(),
        client_family(),
        queued,
    )
}

fn build_daily_aggregate(
    installation_id: String,
    deletion_token_hash: String,
    timestamp_bucket: String,
    distribution_channel: DistributionChannel,
    client_family: ClientFamily,
    queued: &QueuedOneShots,
) -> Result<TelemetryBatchV2, String> {
    let observed = &queued.counters;
    let baseline = CounterCheckpoint::default();
    let latency_counts = bounded_histogram_delta(
        &observed.tool_latency_buckets,
        &baseline.tool_latency_buckets,
    );
    let calls = latency_counts.iter().sum();
    debug_assert_eq!(
        calls,
        observed
            .tool_calls
            .saturating_sub(baseline.tool_calls)
            .min(MAX_COUNT)
    );
    let failures = observed
        .tool_failures
        .saturating_sub(baseline.tool_failures)
        .min(calls);
    let duration = observed
        .session_uptime_secs
        .saturating_sub(baseline.session_uptime_secs)
        .min(MAX_COUNT);
    let mut batch = build_daily_heartbeat(
        installation_id,
        deletion_token_hash,
        timestamp_bucket,
        distribution_channel,
        client_family,
    )?;
    let common = batch.events[0].clone();
    batch.events.push(envelope_like(
        &common,
        TelemetryEventV2::SetupProfile(setup_profile()),
    ));
    batch.events.push(envelope_like(
        &common,
        TelemetryEventV2::SessionAggregate(SessionMetrics {
            sessions: 1,
            duration_seconds: single_observation_histogram(
                duration,
                &[
                    60,
                    300,
                    900,
                    3_600,
                    14_400,
                    86_400,
                    604_800,
                    31_536_000,
                    i64::MAX as u64,
                ],
                1,
            ),
        }),
    ));
    batch.events.push(envelope_like(
        &common,
        TelemetryEventV2::ToolUsageAggregate(ToolUsageMetrics {
            calls,
            failures,
            latency_milliseconds: Histogram {
                upper_bounds: crate::core::telemetry::TOOL_LATENCY_BUCKET_UPPER_MS.to_vec(),
                counts: latency_counts.to_vec(),
            },
        }),
    ));
    let tools = tool_call_deltas(&observed.tools, &baseline.tools);
    if !tools.is_empty() {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::ToolCallAggregate(ToolCallMetrics { tools }),
        ));
    }
    if queued.setup_completed {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::SetupCompleted(OccurrenceMetrics { count: 1 }),
        ));
    }
    if queued.integrations_detected > 0 {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::IntegrationDetected(OccurrenceMetrics {
                count: queued.integrations_detected.min(MAX_COUNT),
            }),
        ));
    }
    if let Some(transition) = queued.version_upgrade {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::VersionUpgrade(VersionUpgradeMetrics {
                from_major: transition.from_major,
                to_major: transition.to_major,
            }),
        ));
    }
    if queued.sync.attempts > 0 {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::SyncAggregate(queued.sync.clone()),
        ));
    }
    if queued.autopilot.admitted > 0 || queued.autopilot.denied > 0 || queued.autopilot.fallback > 0
    {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::AutopilotAggregate(queued.autopilot.clone()),
        ));
    }
    if queued.autopilot_fallback.fallback > 0 {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::AutopilotFallbackAggregate(queued.autopilot_fallback.clone()),
        ));
    }
    if queued.checkout_started > 0 {
        batch.events.push(envelope_like(
            &common,
            TelemetryEventV2::CheckoutStarted(OccurrenceMetrics {
                count: queued.checkout_started,
            }),
        ));
    }
    for (index, category) in ERROR_CATEGORIES.into_iter().enumerate() {
        let count = queued.error_categories[index].min(MAX_COUNT);
        if count > 0 {
            batch.events.push(envelope_like(
                &common,
                TelemetryEventV2::ErrorCategoryAggregate(ErrorMetrics { category, count }),
            ));
        }
    }
    batch
        .validate()
        .map_err(|error| format!("invalid telemetry batch: {error:?}"))?;
    Ok(batch)
}

fn bounded_histogram_delta<const N: usize>(observed: &[u64; N], baseline: &[u64; N]) -> [u64; N] {
    let mut remaining = MAX_COUNT;
    std::array::from_fn(|index| {
        let count = observed[index]
            .saturating_sub(baseline[index])
            .min(remaining);
        remaining -= count;
        count
    })
}

/// Per-tool calls since the baseline. Names failing the contract format are
/// dropped; past [`MAX_TOOL_ENTRIES`] the most-called tools are kept. The
/// result is sorted by name, as the contract requires.
fn tool_call_deltas(
    observed: &BTreeMap<String, ToolCounterCheckpoint>,
    baseline: &BTreeMap<String, ToolCounterCheckpoint>,
) -> Vec<ToolCallCount> {
    let mut tools: Vec<ToolCallCount> = observed
        .iter()
        .filter(|(tool, _)| valid_tool_name(tool))
        .filter_map(|(tool, counter)| {
            let base = baseline.get(tool).copied().unwrap_or_default();
            let calls = counter.calls.saturating_sub(base.calls).min(MAX_COUNT);
            let failures = counter.failures.saturating_sub(base.failures).min(calls);
            (calls > 0).then(|| ToolCallCount {
                tool: tool.clone(),
                calls,
                failures,
            })
        })
        .collect();
    if tools.len() > MAX_TOOL_ENTRIES {
        tools.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.tool.cmp(&b.tool)));
        tools.truncate(MAX_TOOL_ENTRIES);
        tools.sort_by(|a, b| a.tool.cmp(&b.tool));
    }
    tools
}

fn envelope_like(template: &TelemetryEnvelopeV2, event: TelemetryEventV2) -> TelemetryEnvelopeV2 {
    TelemetryEnvelopeV2 {
        schema_version: template.schema_version,
        timestamp_bucket: template.timestamp_bucket.clone(),
        installation_id: template.installation_id.clone(),
        account_id: template.account_id.clone(),
        organization_id: template.organization_id.clone(),
        app_version: template.app_version.clone(),
        event,
    }
}

fn single_observation_histogram(value: u64, bounds: &[u64], count: u64) -> Histogram {
    let mut counts = vec![0; bounds.len()];
    let index = bounds
        .iter()
        .position(|bound| value <= *bound)
        .unwrap_or(bounds.len() - 1);
    counts[index] = count.min(MAX_COUNT);
    Histogram {
        upper_bounds: bounds.to_vec(),
        counts,
    }
}

fn current_checkpoint() -> CounterCheckpoint {
    let snapshot = crate::core::telemetry::global_metrics().daily_telemetry_snapshot();
    CounterCheckpoint {
        tool_calls: snapshot.tool_calls,
        tool_failures: snapshot.tool_failures,
        tool_latency_buckets: snapshot.tool_latency_buckets,
        session_uptime_secs: snapshot.session_uptime_secs,
        tools: snapshot
            .per_tool
            .into_iter()
            .map(|(tool, counter)| {
                (
                    tool.to_string(),
                    ToolCounterCheckpoint {
                        calls: counter.calls,
                        failures: counter.failures,
                    },
                )
            })
            .collect(),
    }
}

fn process_nonce() -> &'static str {
    static NONCE: OnceLock<String> = OnceLock::new();
    NONCE.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// This process's counters already folded into each sidecar, keyed by path so
/// an isolated state directory starts from zero.
fn folded_baselines() -> &'static Mutex<HashMap<PathBuf, CounterCheckpoint>> {
    static FOLDED: OnceLock<Mutex<HashMap<PathBuf, CounterCheckpoint>>> = OnceLock::new();
    FOLDED.get_or_init(Mutex::default)
}

/// Add this process's not-yet-folded counters to `one_shots.queued` in memory
/// and return the observed checkpoint. Callers that persist the result must
/// then [`mark_folded`] it while still holding the sidecar lock.
fn fold_process_counters(
    path: &std::path::Path,
    one_shots: &mut OneShotState,
) -> CounterCheckpoint {
    let observed = current_checkpoint();
    let folded = folded_baselines()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .cloned()
        .unwrap_or_default();
    add_counters(
        &mut one_shots.queued.counters,
        &counter_delta(&observed, &folded),
    );
    observed
}

fn mark_folded(path: &std::path::Path, observed: CounterCheckpoint) {
    folded_baselines()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path.to_path_buf(), observed);
}

/// Persist this process's unsent tool counters so they survive process exit
/// and reach the next daily batch. While telemetry is disabled the counters
/// are skipped instead, so re-enabling never back-fills opted-out activity.
pub fn persist_process_counters() -> Result<(), String> {
    let path = one_shot_path()?;
    if !telemetry_collection_eligible() {
        mark_folded(&path, current_checkpoint());
        return Ok(());
    }
    ensure_parent(&path)?;
    let lock = open_sidecar_lock(&path)?;
    lock_telemetry_file(&lock, "one-shot")?;
    let mut one_shots = one_shots_for_current_identity(load_one_shots_at(&path)?)?;
    let before = one_shots.queued.counters.clone();
    let observed = fold_process_counters(&path, &mut one_shots);
    if one_shots.queued.counters != before {
        write_one_shots(&path, &one_shots)?;
    }
    mark_folded(&path, observed);
    Ok(())
}

fn counter_delta(observed: &CounterCheckpoint, baseline: &CounterCheckpoint) -> CounterCheckpoint {
    CounterCheckpoint {
        tool_calls: observed.tool_calls.saturating_sub(baseline.tool_calls),
        tool_failures: observed
            .tool_failures
            .saturating_sub(baseline.tool_failures),
        tool_latency_buckets: std::array::from_fn(|index| {
            observed.tool_latency_buckets[index]
                .saturating_sub(baseline.tool_latency_buckets[index])
        }),
        session_uptime_secs: observed
            .session_uptime_secs
            .saturating_sub(baseline.session_uptime_secs),
        tools: observed
            .tools
            .iter()
            .filter_map(|(tool, counter)| {
                let base = baseline.tools.get(tool).copied().unwrap_or_default();
                let delta = ToolCounterCheckpoint {
                    calls: counter.calls.saturating_sub(base.calls),
                    failures: counter.failures.saturating_sub(base.failures),
                };
                (delta.calls > 0 || delta.failures > 0).then(|| (tool.clone(), delta))
            })
            .collect(),
    }
}

fn add_counters(total: &mut CounterCheckpoint, delta: &CounterCheckpoint) {
    total.tool_calls = total.tool_calls.saturating_add(delta.tool_calls);
    total.tool_failures = total.tool_failures.saturating_add(delta.tool_failures);
    for (bucket, added) in total
        .tool_latency_buckets
        .iter_mut()
        .zip(delta.tool_latency_buckets)
    {
        *bucket = bucket.saturating_add(added);
    }
    total.session_uptime_secs = total
        .session_uptime_secs
        .saturating_add(delta.session_uptime_secs);
    for (tool, added) in &delta.tools {
        if !total.tools.contains_key(tool) && total.tools.len() >= MAX_PERSISTED_TOOLS {
            continue;
        }
        let entry = total.tools.entry(tool.clone()).or_default();
        entry.calls = entry.calls.saturating_add(added.calls);
        entry.failures = entry.failures.saturating_add(added.failures);
    }
}

/// Remove exactly what an acknowledged batch carried. Totals are re-derived
/// from the latency buckets so they stay consistent even if an identity
/// reset zeroed the queue between prepare and acknowledgement.
fn subtract_counters(total: &mut CounterCheckpoint, included: &CounterCheckpoint) {
    for (bucket, sent) in total
        .tool_latency_buckets
        .iter_mut()
        .zip(included.tool_latency_buckets)
    {
        *bucket = bucket.saturating_sub(sent);
    }
    total.tool_calls = total.tool_latency_buckets.iter().sum();
    total.tool_failures = total
        .tool_failures
        .saturating_sub(included.tool_failures)
        .min(total.tool_calls);
    total.session_uptime_secs = total
        .session_uptime_secs
        .saturating_sub(included.session_uptime_secs);
    for (tool, sent) in &included.tools {
        if let Some(entry) = total.tools.get_mut(tool) {
            entry.calls = entry.calls.saturating_sub(sent.calls);
            entry.failures = entry
                .failures
                .saturating_sub(sent.failures)
                .min(entry.calls);
        }
    }
    total.tools.retain(|_, counter| counter.calls > 0);
}

/// Upper bound on distinct tool names kept in the sidecar; the batch itself
/// keeps at most [`MAX_TOOL_ENTRIES`] of them.
const MAX_PERSISTED_TOOLS: usize = 256;

fn state_path() -> Result<PathBuf, String> {
    crate::core::paths::state_dir().map(|dir| dir.join("telemetry_v2_aggregate.json"))
}

fn one_shot_path() -> Result<PathBuf, String> {
    crate::core::paths::state_dir().map(|dir| dir.join("telemetry_v2_one_shots.json"))
}

pub fn record_setup_completion(
    integration_ids: impl IntoIterator<Item = String>,
) -> Result<(), String> {
    if !telemetry_collection_eligible() {
        return Ok(());
    }
    with_locked_one_shots(|mut state| {
        if !state.setup_recorded {
            state.setup_recorded = true;
            state.queued.setup_completed = true;
        }
        for integration_id in integration_ids {
            if state.configured_integrations.len() >= 64 {
                break;
            }
            if integration_id.is_empty() || integration_id.len() > 128 {
                continue;
            }
            let stable_id = hex::encode(sha2::Sha256::digest(integration_id.as_bytes()));
            if state.configured_integrations.insert(stable_id) {
                state.queued.integrations_detected = state
                    .queued
                    .integrations_detected
                    .saturating_add(1)
                    .min(MAX_COUNT);
            }
        }
        Ok((state, ()))
    })
}

pub fn record_sync_result(success: bool) -> Result<(), String> {
    if !telemetry_collection_eligible() {
        return Ok(());
    }
    with_locked_one_shots(|mut state| {
        if state.queued.sync.attempts >= MAX_COUNT {
            return Ok((state, ()));
        }
        state.queued.sync.attempts += 1;
        if success {
            state.queued.sync.successes = state
                .queued
                .sync
                .successes
                .saturating_add(1)
                .min(state.queued.sync.attempts);
        } else {
            state.queued.sync.failures = state.queued.sync.failures.saturating_add(1).min(
                state
                    .queued
                    .sync
                    .attempts
                    .saturating_sub(state.queued.sync.successes),
            );
        }
        Ok((state, ()))
    })
}

pub fn record_autopilot_decisions(admitted: u64, denied: u64) -> Result<(), String> {
    if !telemetry_collection_eligible() {
        return Ok(());
    }
    with_locked_one_shots(|mut state| {
        state.queued.autopilot.admitted = state
            .queued
            .autopilot
            .admitted
            .saturating_add(admitted)
            .min(MAX_COUNT);
        state.queued.autopilot.denied = state
            .queued
            .autopilot
            .denied
            .saturating_add(denied)
            .min(MAX_COUNT);
        Ok((state, ()))
    })
}

pub fn record_autopilot_fallback() -> Result<(), String> {
    if !telemetry_collection_eligible() {
        return Ok(());
    }
    with_locked_one_shots(|mut state| {
        state.queued.autopilot.fallback = state
            .queued
            .autopilot
            .fallback
            .saturating_add(1)
            .min(MAX_COUNT);
        state.queued.autopilot_fallback.fallback = state
            .queued
            .autopilot_fallback
            .fallback
            .saturating_add(1)
            .min(MAX_COUNT);
        Ok((state, ()))
    })
}

pub fn record_checkout_started() -> Result<(), String> {
    if !telemetry_collection_eligible() {
        return Ok(());
    }
    with_locked_one_shots(|mut state| {
        state.queued.checkout_started = state
            .queued
            .checkout_started
            .saturating_add(1)
            .min(MAX_COUNT);
        Ok((state, ()))
    })
}

const ERROR_CATEGORIES: [ErrorCategory; 8] = [
    ErrorCategory::Authentication,
    ErrorCategory::Authorization,
    ErrorCategory::Configuration,
    ErrorCategory::Network,
    ErrorCategory::Provider,
    ErrorCategory::Timeout,
    ErrorCategory::Validation,
    ErrorCategory::Internal,
];

pub fn record_error_category(category: ErrorCategory) -> Result<(), String> {
    record_error_category_inner(category)
}

fn record_error_category_inner(category: ErrorCategory) -> Result<(), String> {
    if !telemetry_collection_eligible() {
        return Ok(());
    }
    let index = ERROR_CATEGORIES
        .iter()
        .position(|candidate| *candidate == category)
        .expect("closed error category");
    with_locked_one_shots(|mut state| {
        state.queued.error_categories[index] = state.queued.error_categories[index]
            .saturating_add(1)
            .min(MAX_COUNT);
        Ok((state, ()))
    })
}

fn telemetry_collection_eligible() -> bool {
    let config = crate::core::config::Config::load_global();
    let do_not_track = std::env::var("DO_NOT_TRACK").ok();
    let telemetry_override = std::env::var("LEAN_CTX_TELEMETRY").ok();
    !config.telemetry.explicitly_disabled()
        && !crate::core::config::TelemetryConfig::environment_disables(
            do_not_track.as_deref(),
            telemetry_override.as_deref(),
        )
}

pub fn record_current_version() -> Result<(), String> {
    record_current_version_value(env!("CARGO_PKG_VERSION"))
}

fn record_current_version_value(version: &str) -> Result<(), String> {
    let current = parse_major(version)
        .ok_or_else(|| "current app version has no valid major component".to_string())?;
    with_locked_one_shots(|mut state| {
        let previous = match state.observed_major {
            Some(previous) => Some(previous),
            None => crate::core::telemetry_ledger::latest_valid_version()?
                .as_deref()
                .and_then(parse_major),
        };
        match previous {
            Some(previous) if current > previous => {
                let from_major = state
                    .queued
                    .version_upgrade
                    .map_or(previous, |transition| transition.from_major);
                state.queued.version_upgrade = Some(VersionTransition {
                    from_major,
                    to_major: current,
                });
                state.observed_major = Some(current);
            }
            Some(previous) => state.observed_major = Some(previous.max(current)),
            None => state.observed_major = Some(current),
        }
        Ok((state, ()))
    })
}

fn parse_major(version: &str) -> Option<u16> {
    version.split('.').next()?.parse().ok()
}

pub fn purge_local_state() -> Result<(), String> {
    purge_local_state_then(|| Ok(()))
}

pub fn purge_local_state_then<T>(
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let path = state_path()?;
    ensure_parent(&path)?;
    let lock = open_state_lock(&path)?;
    lock_telemetry_file(&lock, "aggregate")?;
    let one_shot_path = one_shot_path()?;
    ensure_parent(&one_shot_path)?;
    let one_shot_lock = open_sidecar_lock(&one_shot_path)?;
    lock_telemetry_file(&one_shot_lock, "one-shot")?;
    remove_state_file(&path, "aggregate")?;
    remove_state_file(&one_shot_path, "one-shot")?;
    operation()
}

pub fn rotate_identity_state_then<T>(
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let path = state_path()?;
    let one_shot_path = one_shot_path()?;
    ensure_parent(&path)?;
    let lock = open_state_lock(&path)?;
    lock_telemetry_file(&lock, "aggregate")?;
    let one_shot_lock = open_sidecar_lock(&one_shot_path)?;
    lock_telemetry_file(&one_shot_lock, "one-shot")?;
    let mut one_shots = one_shots_for_current_identity(load_one_shots_at(&one_shot_path)?)?;
    write_one_shots(&one_shot_path, &one_shots)?;
    let value = operation()?;
    one_shots.installation_id = installation_id::get_or_create()
        .map_err(|error| format!("installation ID unavailable after rotation: {error}"))?;
    one_shots.queued.setup_completed |= one_shots.setup_recorded;
    one_shots.queued.integrations_detected = one_shots
        .configured_integrations
        .len()
        .try_into()
        .unwrap_or(MAX_COUNT)
        .min(MAX_COUNT);
    one_shots.queued.sync = SyncMetrics::default();
    one_shots.queued.autopilot = DecisionMetrics::default();
    one_shots.queued.autopilot_fallback = DecisionMetrics::default();
    one_shots.queued.checkout_started = 0;
    one_shots.queued.error_categories = [0; 8];
    one_shots.queued.counters = CounterCheckpoint::default();
    one_shots.last_acknowledged_batch = None;
    remove_state_file(&path, "aggregate")?;
    write_one_shots(&one_shot_path, &one_shots)?;
    Ok(value)
}

fn remove_state_file(path: &std::path::Path, kind: &str) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot purge telemetry {kind} state: {error}")),
    }
}

fn load_state() -> Result<AggregateState, String> {
    let path = state_path()?;
    load_state_at(&path)
}

fn load_state_at(path: &std::path::Path) -> Result<AggregateState, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid telemetry aggregate state: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AggregateState::default()),
        Err(error) => Err(format!("cannot read telemetry aggregate state: {error}")),
    }
}

fn load_one_shots_at(path: &std::path::Path) -> Result<OneShotState, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid telemetry one-shot state: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(OneShotState::default()),
        Err(error) => Err(format!("cannot read telemetry one-shot state: {error}")),
    }
}

fn one_shots_for_current_identity(mut state: OneShotState) -> Result<OneShotState, String> {
    let current = installation_id::get_or_create()
        .map_err(|error| format!("installation ID unavailable: {error}"))?;
    if state.installation_id.is_empty() {
        state.installation_id = current;
    } else if state.installation_id != current {
        state.installation_id = current;
        state.queued.setup_completed |= state.setup_recorded;
        state.queued.integrations_detected = state
            .configured_integrations
            .len()
            .try_into()
            .unwrap_or(MAX_COUNT)
            .min(MAX_COUNT);
        state.queued.sync = SyncMetrics::default();
        state.queued.autopilot = DecisionMetrics::default();
        state.queued.autopilot_fallback = DecisionMetrics::default();
        state.queued.checkout_started = 0;
        state.queued.error_categories = [0; 8];
        state.queued.counters = CounterCheckpoint::default();
        state.last_acknowledged_batch = None;
    }
    Ok(state)
}

fn with_locked_one_shots<T>(
    operation: impl FnOnce(OneShotState) -> Result<(OneShotState, T), String>,
) -> Result<T, String> {
    let path = one_shot_path()?;
    ensure_parent(&path)?;
    let lock = open_sidecar_lock(&path)?;
    lock_telemetry_file(&lock, "one-shot")?;
    let state = one_shots_for_current_identity(load_one_shots_at(&path)?)?;
    let (state, value) = operation(state)?;
    write_one_shots(&path, &state)?;
    Ok(value)
}

fn acknowledge_one_shots_at(
    path: &std::path::Path,
    included: &QueuedOneShots,
    acknowledgement_id: &str,
) -> Result<(), String> {
    ensure_parent(path)?;
    let lock = open_sidecar_lock(path)?;
    lock_telemetry_file(&lock, "one-shot")?;
    let mut state = one_shots_for_current_identity(load_one_shots_at(path)?)?;
    if state.last_acknowledged_batch.as_deref() == Some(acknowledgement_id) {
        return Ok(());
    }
    if included.setup_completed {
        state.queued.setup_completed = false;
    }
    state.queued.integrations_detected = state
        .queued
        .integrations_detected
        .saturating_sub(included.integrations_detected);
    state.queued.sync.attempts = state
        .queued
        .sync
        .attempts
        .saturating_sub(included.sync.attempts);
    state.queued.sync.successes = state
        .queued
        .sync
        .successes
        .saturating_sub(included.sync.successes);
    state.queued.sync.failures = state
        .queued
        .sync
        .failures
        .saturating_sub(included.sync.failures);
    subtract_decisions(&mut state.queued.autopilot, &included.autopilot);
    subtract_decisions(
        &mut state.queued.autopilot_fallback,
        &included.autopilot_fallback,
    );
    state.queued.checkout_started = state
        .queued
        .checkout_started
        .saturating_sub(included.checkout_started);
    for (queued, included) in state
        .queued
        .error_categories
        .iter_mut()
        .zip(included.error_categories)
    {
        *queued = queued.saturating_sub(included);
    }
    subtract_counters(&mut state.queued.counters, &included.counters);
    if let Some(sent) = included.version_upgrade {
        state.queued.version_upgrade = match state.queued.version_upgrade {
            Some(current) if current == sent => None,
            Some(current)
                if current.from_major == sent.from_major && current.to_major > sent.to_major =>
            {
                Some(VersionTransition {
                    from_major: sent.to_major,
                    to_major: current.to_major,
                })
            }
            current => current,
        };
    }
    state.last_acknowledged_batch = Some(acknowledgement_id.to_string());
    write_one_shots(path, &state)
}

fn subtract_decisions(current: &mut DecisionMetrics, included: &DecisionMetrics) {
    current.admitted = current.admitted.saturating_sub(included.admitted);
    current.denied = current.denied.saturating_sub(included.denied);
    current.fallback = current.fallback.saturating_sub(included.fallback);
}

#[cfg(test)]
fn with_locked_state<T>(
    operation: impl FnOnce(AggregateState) -> Result<(AggregateState, T), String>,
) -> Result<T, String> {
    let path = state_path()?;
    ensure_parent(&path)?;
    let lock = open_state_lock(&path)?;
    lock.lock_exclusive()
        .map_err(|error| format!("cannot lock telemetry aggregate state: {error}"))?;
    let (state, value) = operation(load_state_at(&path)?)?;
    write_state(&path, &state)?;
    Ok(value)
}

fn ensure_parent(path: &std::path::Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "telemetry state path has no parent".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create telemetry state directory: {error}"))
}

fn write_state(path: &std::path::Path, state: &AggregateState) -> Result<(), String> {
    let bytes = serde_json::to_vec(&state)
        .map_err(|error| format!("cannot serialize telemetry aggregate state: {error}"))?;
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions: Option<std::fs::Permissions> = None;
    crate::core::atomic_fs::try_atomic_write(&path, &bytes, permissions.as_ref())
        .map_err(|error| format!("cannot persist telemetry aggregate state: {error}"))
}

fn write_one_shots(path: &std::path::Path, state: &OneShotState) -> Result<(), String> {
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("cannot serialize telemetry one-shot state: {error}"))?;
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions: Option<std::fs::Permissions> = None;
    crate::core::atomic_fs::try_atomic_write(path, &bytes, permissions.as_ref())
        .map_err(|error| format!("cannot persist telemetry one-shot state: {error}"))
}

fn open_state_lock(path: &std::path::Path) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))
        .map_err(|error| format!("cannot open telemetry state lock: {error}"))
}

fn open_sidecar_lock(path: &std::path::Path) -> Result<std::fs::File, String> {
    open_state_lock(path)
}

/// Contention fails without mutating state; failed acknowledgements keep the
/// frozen batch retryable. This bounds acquisition, not filesystem I/O.
#[cfg(not(test))]
const SEND_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);
/// Same bounded path, still several retries: contention tests sit out the full
/// timeout while holding the global test env lock.
#[cfg(test)]
const SEND_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(150);
const SEND_LOCK_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// Shared acquisition for aggregate, one-shot and ledger locks, in that order.
pub(super) fn lock_telemetry_file(file: &std::fs::File, kind: &str) -> Result<(), String> {
    let deadline = std::time::Instant::now() + SEND_LOCK_TIMEOUT;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) if super::file_lock::is_contended(&error) => {
                if std::time::Instant::now() >= deadline {
                    return Err(format!(
                        "telemetry {kind} lock timed out after {}ms; another operation is active",
                        SEND_LOCK_TIMEOUT.as_millis()
                    ));
                }
                std::thread::sleep(SEND_LOCK_RETRY_INTERVAL);
            }
            Err(error) => return Err(format!("cannot lock telemetry {kind} state: {error}")),
        }
    }
}

pub fn build_daily_heartbeat(
    installation_id: String,
    deletion_token_hash: String,
    timestamp_bucket: String,
    distribution_channel: DistributionChannel,
    client_family: ClientFamily,
) -> Result<TelemetryBatchV2, String> {
    let batch = TelemetryBatchV2 {
        schema_version: SCHEMA_VERSION,
        deletion_token_hash,
        events: vec![TelemetryEnvelopeV2 {
            schema_version: SCHEMA_VERSION,
            timestamp_bucket,
            installation_id,
            account_id: None,
            organization_id: None,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            event: TelemetryEventV2::Heartbeat(HeartbeatMetrics {
                distribution_channel,
                client_family,
                operating_system: OperatingSystem::current(),
                architecture: Architecture::current(),
            }),
        }],
    };
    batch
        .validate()
        .map_err(|error| format!("invalid telemetry batch: {error:?}"))?;
    Ok(batch)
}

fn distribution_channel() -> DistributionChannel {
    match option_env!("LEAN_CTX_DISTRIBUTION_CHANNEL") {
        Some("cargo") => DistributionChannel::Cargo,
        Some("homebrew") => DistributionChannel::Homebrew,
        Some("npm") => DistributionChannel::Npm,
        Some("docker") => DistributionChannel::Docker,
        Some("source") => DistributionChannel::Source,
        _ => DistributionChannel::Unknown,
    }
}

fn setup_profile() -> SetupProfileMetrics {
    let integration_mode = match crate::core::config::Config::load().hook_mode_override() {
        None => IntegrationMode::Default,
        Some(crate::hooks::HookMode::Mcp) => IntegrationMode::Mcp,
        Some(crate::hooks::HookMode::Hybrid) => IntegrationMode::Hybrid,
        Some(crate::hooks::HookMode::Replace) => IntegrationMode::Replace,
    };
    SetupProfileMetrics {
        integration_mode,
        embeddings: embeddings_state(),
    }
}

#[cfg(feature = "embeddings")]
fn embeddings_state() -> EmbeddingsState {
    if crate::core::embeddings::EmbeddingEngine::is_available() {
        EmbeddingsState::Installed
    } else if crate::tools::ctx_knowledge::embeddings_auto_download_allowed() {
        EmbeddingsState::NotInstalled
    } else {
        EmbeddingsState::Disabled
    }
}

#[cfg(not(feature = "embeddings"))]
fn embeddings_state() -> EmbeddingsState {
    EmbeddingsState::Unsupported
}

/// Client that drives this installation: the MCP handshake of this process,
/// then the last handshake persisted by any process (the daemon and CLI never
/// see one themselves), then the host's environment variables.
fn client_family() -> ClientFamily {
    const PERSISTED_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;
    let handshake = Some(crate::core::client_capabilities::current().client_id)
        .filter(|id| id != "unknown")
        .or_else(|| {
            crate::core::client_capabilities::load_persisted(PERSISTED_MAX_AGE_SECS)
                .map(|caps| caps.client_id)
        });
    if let Some(family) = handshake.as_deref().and_then(ClientFamily::from_client_id) {
        return family;
    }
    if std::env::var_os("CLAUDECODE").is_some() {
        ClientFamily::Claude
    } else if std::env::var_os("CODEX_HOME").is_some() {
        ClientFamily::Codex
    } else if std::env::var_os("CURSOR_TRACE_ID").is_some() {
        ClientFamily::Cursor
    } else if std::env::var_os("GEMINI_CLI").is_some() {
        ClientFamily::Gemini
    } else {
        ClientFamily::Other
    }
}

#[cfg(test)]
mod tests;
