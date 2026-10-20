//! Frontend-owned host pacing and frozen memory batches.
//!
//! This frontend owns the only `retro_run` loop, so it is the pacing owner: running frames and
//! synchronous frame advances both start at deadlines derived from guest frames since an anchor.
use std::time::Instant;

use super::*;
use crate::live::memory_batch::{self, BatchRange, MemoryBatchCapability, MemoryWindow};
use crate::live::pacing::{self, ExecutionSpeedCapability, SpeedRequest};

const HALT_KIND: &str = "frontend_frame_boundary";
/// Host lag, in target frame periods, after which the pacer re-anchors instead of catching up.
const MAX_LAG_PERIODS: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PacingPolicy {
    Limited { centi_percent: u64 },
    Unlimited,
}

/// Deadline source for frame starts. The anchor pairs a host instant with the guest frame that
/// starts there; frozen time and policy changes drop it so that neither a catch-up burst nor a
/// stale long sleep follows.
#[derive(Debug)]
pub(super) struct FramePacer {
    policy: PacingPolicy,
    revision: u64,
    anchor: Option<(Instant, u64)>,
}

impl FramePacer {
    pub(super) fn new() -> Self {
        Self {
            policy: PacingPolicy::Limited {
                centi_percent: 100 * 100,
            },
            revision: 1,
            anchor: None,
        }
    }

    pub(super) fn reanchor(&mut self) {
        self.anchor = None;
    }

    fn set(&mut self, policy: PacingPolicy) {
        self.policy = policy;
        self.revision += 1;
        self.anchor = None;
    }

    /// Host instant at which guest frame `frame` may start; `None` means no intentional wait.
    pub(super) fn frame_start(
        &mut self,
        frame: u64,
        frame_duration: Duration,
        now: Instant,
    ) -> Option<Instant> {
        let PacingPolicy::Limited { centi_percent } = self.policy else {
            return None;
        };
        let period = frame_duration.mul_f64(10_000.0 / centi_percent as f64);
        let (host, base) = *self.anchor.get_or_insert((now, frame));
        let target = host + period.mul_f64(frame.saturating_sub(base) as f64);
        if now > target + period * MAX_LAG_PERIODS {
            self.anchor = Some((now, frame));
            return Some(now);
        }
        Some(target)
    }
}

/// Why a synchronous frame advance stopped before its requested count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdvanceStop {
    HostDeadline,
    Cancelled,
}

impl AdvanceStop {
    pub(super) fn reason(stop: Option<Self>) -> Value {
        match stop {
            None => Value::Null,
            Some(Self::HostDeadline) => json!("host_deadline"),
            Some(Self::Cancelled) => json!("cancelled"),
        }
    }
}

impl AzaharHost {
    pub(super) fn memory_batch_capability() -> MemoryBatchCapability {
        MemoryBatchCapability {
            max_ranges: memory_batch::CORE_MAX_RANGES,
            max_range_bytes: 0x4000,
            max_total_bytes: memory_batch::CORE_MAX_TOTAL_BYTES,
            consistency: memory_batch::CONSISTENCY_FROZEN_BOUNDARY.into(),
            halt_kinds: vec![HALT_KIND.into()],
            windows: vec![MemoryWindow {
                memory_type: "virtual".into(),
                address: 0,
                length: VIRTUAL_SPACE_SIZE,
            }],
        }
    }

    pub(super) fn execution_speed_capability() -> ExecutionSpeedCapability {
        ExecutionSpeedCapability {
            modes: vec!["limited".into(), "unlimited".into()],
            percent: pacing::PercentDomain {
                min: Some(0.01),
                max: Some(10000.0),
                quantum: Some(0.01),
                values: None,
            },
            states: vec!["running".into(), "frozen".into()],
            scope: pacing::SCOPE_HOST_PACING.into(),
            source: "frontend".into(),
            control_service_ms: 50,
            host_constraints: vec![],
        }
    }

    pub(super) fn pacing_policy_value(&self) -> Value {
        let (mode, percent) = match self.pacer.policy {
            PacingPolicy::Limited { centi_percent } => {
                ("limited", pacing::percent_value(centi_percent))
            }
            PacingPolicy::Unlimited => ("unlimited", Value::Null),
        };
        json!({
            "mode": mode, "percent": percent, "source": "frontend",
            "policy_revision": self.pacer.revision.to_string(), "host_constraints": [],
        })
    }

    pub(super) fn execution_speed(&mut self, params: &Value) -> AzaharResult<Value> {
        let mode = match params.get("mode") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| AzaharError::BadParams("mode must be a string".into()))?,
            ),
        };
        let percent = match params.get("percent") {
            None => None,
            Some(value) => Some(
                value
                    .as_f64()
                    .ok_or_else(|| AzaharError::BadParams("percent must be a number".into()))?,
            ),
        };
        let request = pacing::parse_request(mode, percent).map_err(AzaharError::BadParams)?;
        Self::execution_speed_capability()
            .admit(request)
            .map_err(AzaharError::BadParams)?;
        let policy = match request {
            SpeedRequest::Query => return Ok(self.pacing_policy_value()),
            SpeedRequest::Unlimited => PacingPolicy::Unlimited,
            SpeedRequest::Limited { centi_percent } => PacingPolicy::Limited { centi_percent },
        };
        let previous = self.pacing_policy_value();
        self.pacer.set(policy);
        Ok(json!({
            "status": "completed",
            "state": if self.frozen { "frozen" } else { "running" },
            "previous": previous,
            "execution_speed": self.pacing_policy_value(),
            "frame": self.frame,
        }))
    }

    /// Run up to `count` frames, each starting at its pacing deadline, inside the synchronous
    /// operation budget. A frame whose start would pass the budget is not run.
    pub(super) fn run_paced_frames(
        &mut self,
        count: u64,
    ) -> AzaharResult<(u64, Option<AdvanceStop>)> {
        let budget = Instant::now() + crate::live::temporal::MAX_SYNC_OPERATION_TIME;
        let frame_duration = self.frame_duration();
        self.pacer.reanchor();
        let mut completed = 0;
        let mut stop = None;
        while completed < count {
            if self.request_cancellation.is_cancelled() {
                stop = Some(AdvanceStop::Cancelled);
                break;
            }
            let start = self
                .pacer
                .frame_start(self.frame, frame_duration, Instant::now());
            // The software renderer can run slower than real time, so also stop when the
            // previous frames already consumed the budget.
            if Instant::now() >= budget || start.is_some_and(|start| start > budget) {
                stop = Some(AdvanceStop::HostDeadline);
                break;
            }
            if let Some(start) = start {
                if !wait_for_frame_start(start, &self.request_cancellation) {
                    stop = Some(AdvanceStop::Cancelled);
                    break;
                }
            }
            self.run_one_frame()?;
            completed += 1;
        }
        self.pacer.reanchor();
        Ok((completed, stop))
    }

    /// Next running-frame start for the frontend loop; `None` runs without an intentional wait.
    pub fn next_frame_start(&mut self, now: Instant) -> Option<Instant> {
        let frame_duration = self.frame_duration();
        self.pacer.frame_start(self.frame, frame_duration, now)
    }

    pub(super) fn read_memory_batch(&mut self, params: &Value) -> AzaharResult<Value> {
        let ranges = params
            .get("ranges")
            .and_then(Value::as_array)
            .ok_or_else(|| AzaharError::BadParams("ranges must be an array".into()))?
            .iter()
            .map(|range| {
                Ok(BatchRange {
                    memory_type: range
                        .get("memory_type")
                        .and_then(Value::as_str)
                        .ok_or_else(|| AzaharError::BadParams("memory_type is required".into()))?
                        .to_string(),
                    address: required_num(range, "address")?,
                    length: required_num(range, "length")?,
                })
            })
            .collect::<AzaharResult<Vec<_>>>()?;
        Self::memory_batch_capability()
            .admit(&ranges)
            .map_err(AzaharError::BadParams)?;
        self.require_frozen("read_memory_batch")?;
        // Read everything before publishing anything: one unmapped range rejects the batch.
        let mut reads = Vec::with_capacity(ranges.len());
        for (index, range) in ranges.iter().enumerate() {
            let bytes = self
                .read_virtual(range.address as u32, range.length as usize)
                .map_err(|_| {
                    AzaharError::BadParams(format!(
                        "range {index} at {:#x} is not mapped in the application",
                        range.address
                    ))
                })?;
            reads.push(json!({"index": index, "memory_type": range.memory_type,
                "address": range.address, "length": range.length, "hex": hex::encode(bytes)}));
        }
        let epoch = format!("f{}#s{}", self.frame, self.boundary_seq);
        Ok(json!({
            "state": "frozen",
            "consistency": memory_batch::CONSISTENCY_FROZEN_BOUNDARY,
            "boundary": {
                "runtime_generation": self.launch_id.clone()
                    .unwrap_or_else(|| format!("unmanaged-pid:{}", std::process::id())),
                "stop_epoch": epoch,
                "memory_mapping_epoch": epoch,
                "clocks": [{"domain": "azahar.retro_run_frame", "value": self.frame}],
            },
            "total_bytes": ranges.iter().map(|range| range.length).sum::<u64>(),
            "reads": reads,
        }))
    }
}

// The guest owner stays outside retro_run throughout this host-only wait.
fn wait_for_frame_start(
    start: Instant,
    cancellation: &crate::live::link::RequestCancellation,
) -> bool {
    loop {
        if cancellation.is_cancelled() {
            return false;
        }
        let now = Instant::now();
        if now >= start {
            return true;
        }
        std::thread::sleep((start - now).min(Duration::from_millis(10)));
    }
}
