//! Generation-owned successful controller responses and bounded provenance.
use super::*;
use std::sync::{Mutex, OnceLock};
pub(super) static POLL_COUNT: AtomicU64 = AtomicU64::new(0);
const CAPACITY: usize = 4096;
#[derive(Clone)]
struct Poll {
    sequence: u64,
    word: u32,
    frame: u64,
    vi: u64,
}
fn history() -> &'static Mutex<VecDeque<Poll>> {
    static HISTORY: OnceLock<Mutex<VecDeque<Poll>>> = OnceLock::new();
    HISTORY.get_or_init(|| Mutex::new(VecDeque::with_capacity(CAPACITY)))
}
pub(super) fn reset() {
    POLL_COUNT.store(0, Ordering::Release);
    history().lock().unwrap().clear();
}
pub(super) extern "C" fn completed(word: u32) {
    let sequence = POLL_COUNT.load(Ordering::Acquire) + 1;
    {
        let mut ring = history().lock().unwrap_or_else(|e| e.into_inner());
        if ring.len() == CAPACITY {
            ring.pop_front();
        }
        ring.push_back(Poll {
            sequence,
            word,
            frame: FRAME_COUNT.load(Ordering::Acquire),
            vi: VI_COUNT.load(Ordering::Acquire),
        });
        POLL_COUNT.store(sequence, Ordering::Release);
    }
    frame::poll_callback(sequence);
}
pub(super) fn capability() -> Value {
    json!({"port":0,"domain":"port0_successful_controller_read_response","counter":"generation_monotonic",
        "boundary":"pif_response_written_before_si_dma_and_guest_copy","trace_capacity":CAPACITY,
        "trace_method":"read_input_polls","step_method":"step_input_polls"})
}
impl Mupen64PlusHost {
    pub(super) fn read_input_polls(&self, params: &Value) -> N64Result<Value> {
        self.require_connected()?;
        let after = optional_num(params, "after_sequence")?.unwrap_or(0);
        let limit = optional_num(params, "limit")?.unwrap_or(128);
        if !(1..=CAPACITY as u64).contains(&limit) {
            return Err(N64Error::BadParams(
                "poll trace limit must be 1..4096".into(),
            ));
        }
        let ring = history().lock().unwrap_or_else(|e| e.into_inner());
        let (first, last, selected) = select(&ring, after, limit as usize)?;
        drop(ring);
        let entries = selected
            .iter()
            .map(|p| {
                json!({
            "sequence":p.sequence,"port":0,"controller_word":p.word,"bytes":p.word.to_le_bytes(),
            "rendered_frame":p.frame,"vi_count":p.vi})
            })
            .collect::<Vec<_>>();
        let next = entries
            .last()
            .and_then(|p| p["sequence"].as_u64())
            .unwrap_or(after);
        Ok(
            json!({"generation":self.launch_id,"domain":"port0_successful_controller_read_response",
            "encoding":"mupen64plus-controller-word-le","first_available":first,"last_sequence":last,
            "next_sequence":next,"dropped_before_cursor":first.saturating_sub(after.saturating_add(1)),"entries":entries}),
        )
    }
}

fn select(ring: &VecDeque<Poll>, after: u64, limit: usize) -> N64Result<(u64, u64, Vec<Poll>)> {
    let last = ring.back().map_or(0, |p| p.sequence);
    if after > last {
        return Err(N64Error::BadParams(
            "poll cursor is ahead of this generation".into(),
        ));
    }
    let first = ring.front().map_or(1, |p| p.sequence);
    Ok((
        first,
        last,
        ring.iter()
            .filter(|p| p.sequence > after)
            .take(limit)
            .cloned()
            .collect(),
    ))
}
#[cfg(test)]
#[path = "n64_adapter_poll_tests.rs"]
mod tests;
