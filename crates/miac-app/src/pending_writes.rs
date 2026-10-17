//! Keep asynchronous controls stable until acknowledgements and readback agree.
use std::{collections::{BTreeMap, VecDeque}, time::{Duration, Instant}};
use miac_core::controller::PropValue;
use serde_json::Value;

struct Pending {
    value: PropValue,
    queued: VecDeque<Value>,
    deadline: Instant,
}

#[derive(Default)]
pub struct PendingWrites(BTreeMap<String, Pending>);

impl PendingWrites {
    pub fn input(&mut self, name: &str, value: Value, now: Instant) {
        let pending = self.0.entry(name.into()).or_insert_with(|| Pending {
            value: PropValue::Ok(value.clone()), queued: VecDeque::new(), deadline: now,
        });
        pending.value = PropValue::Ok(value.clone());
        pending.queued.push_back(value);
        pending.deadline = now + Duration::from_secs(30);
    }

    pub fn value(&self, name: &str) -> Option<&PropValue> {
        self.0.get(name).map(|p| &p.value)
    }

    pub fn complete(&mut self, name: &str, ok: bool, now: Instant) {
        let Some(pending) = self.0.get_mut(name) else { return; };
        pending.queued.pop_front();
        // A failure for an older input must not erase a newer selection.
        if !ok && pending.queued.is_empty() { self.0.remove(name); }
        else { pending.deadline = now + Duration::from_secs(10); }
    }

    pub fn snapshot(&mut self, values: &[(&str, PropValue)]) {
        self.0.retain(|name, pending| {
            !pending.queued.is_empty() || !values.iter().any(|(key, value)| {
                *key == name && matches!((&pending.value, value),
                    (PropValue::Ok(expected), PropValue::Ok(observed)) if expected == observed)
            })
        });
    }

    pub fn expire(&mut self, now: Instant) -> bool {
        let old = self.0.len();
        self.0.retain(|_, pending| pending.deadline > now);
        old != self.0.len()
    }

    pub fn clear(&mut self) { self.0.clear(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_snapshots_and_failed_older_writes_preserve_the_latest_selection() {
        let now = Instant::now();
        let mut pending = PendingWrites::default();
        pending.input("mode", json!(2), now);
        pending.input("mode", json!(3), now);
        pending.snapshot(&[("mode", PropValue::Ok(json!(1)))]);
        pending.complete("mode", false, now);
        assert_eq!(pending.value("mode").unwrap().as_i64(), Some(3));
        pending.complete("mode", true, now);
        pending.snapshot(&[("mode", PropValue::Ok(json!(2)))]);
        assert!(pending.value("mode").is_some());
        pending.snapshot(&[("mode", PropValue::Ok(json!(3)))]);
        assert!(pending.value("mode").is_none());
    }

    #[test]
    fn latest_failure_or_missing_readback_cannot_leave_permanent_optimism() {
        let now = Instant::now();
        let mut pending = PendingWrites::default();
        pending.input("eco", json!(true), now);
        pending.complete("eco", false, now);
        assert!(pending.value("eco").is_none());
        pending.input("eco", json!(true), now);
        pending.complete("eco", true, now);
        assert!(pending.expire(now + Duration::from_secs(11)));
    }
}
