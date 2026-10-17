//! Retained debugger evidence. Overflow affects delivery, never native stop ownership.
use serde_json::Value;
use std::collections::VecDeque;
use std::io::{self, Write};

pub(super) const MAX_EVENTS: usize = 256;
pub(super) const MAX_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub(super) struct PendingEvents {
    entries: VecDeque<(Value, usize)>,
    bytes: usize,
    dropped: u64,
}

impl PendingEvents {
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn push(&mut self, event: Value) {
        let mut counter = EventBytes(0);
        if serde_json::to_writer(&mut counter, &event).is_err() {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        while self.entries.len() >= MAX_EVENTS || counter.0 > MAX_EVENT_BYTES - self.bytes {
            self.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.bytes += counter.0;
        self.entries.push_back((event, counter.0));
    }

    pub(super) fn pop_front(&mut self) -> Option<Value> {
        self.entries.pop_front().map(|(event, bytes)| {
            self.bytes -= bytes;
            event
        })
    }

    pub(super) fn append(&mut self, mut newer: Self) {
        self.dropped = self.dropped.saturating_add(newer.take_dropped());
        while let Some(event) = newer.pop_front() {
            self.push(event);
        }
    }

    pub(super) fn take_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.dropped)
    }

    pub(super) fn take_matching(&mut self, filter: Option<u64>) -> Vec<Value> {
        let mut selected = Vec::new();
        for _ in 0..self.entries.len() {
            let (event, bytes) = self.entries.pop_front().unwrap();
            if filter.is_none_or(|id| event.get("id").and_then(Value::as_u64) == Some(id)) {
                self.bytes -= bytes;
                selected.push(event);
            } else {
                self.entries.push_back((event, bytes));
            }
        }
        selected
    }
}

struct EventBytes(usize);

impl Write for EventBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_EVENT_BYTES - self.0 {
            return Err(io::Error::other("event exceeds retained byte budget"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
impl std::ops::Index<usize> for PendingEvents {
    type Output = Value;
    fn index(&self, index: usize) -> &Value {
        &self.entries[index].0
    }
}
