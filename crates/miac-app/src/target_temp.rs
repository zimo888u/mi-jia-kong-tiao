use std::time::{Duration, Instant};

pub const SETTLE_AFTER: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    WriteProp(f64),
    ApplyPreset(f64),
    SavePending(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Manual,
    Preset,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    temp: f64,
    ready_at: Instant,
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    Continue,
    RequestSnapshot,
}

#[derive(Debug, Default)]
pub struct State {
    override_temp: Option<f64>,
    pending: Option<Pending>,
    in_flight: Option<Pending>,
}

impl State {
    pub fn input(&mut self, temp: f64, now: Instant) {
        self.override_temp = Some(temp);
        self.pending = Some(Pending {
            temp,
            ready_at: now + SETTLE_AFTER,
            kind: Kind::Manual,
        });
    }

    pub fn preset(&mut self, temp: f64, now: Instant) {
        self.override_temp = Some(temp);
        self.pending = Some(Pending {
            temp,
            ready_at: now,
            kind: Kind::Preset,
        });
    }

    pub fn start_preset(&mut self, temp: f64, now: Instant) -> Option<Command> {
        if self.in_flight.is_some() {
            // Keep the preset behind the current write.  Returning `None`
            // without retaining it would make the power-on acknowledgement
            // consume the saved temperature permanently.
            self.preset(temp, now);
            return None;
        }
        if self.pending.is_none() {
            self.override_temp = Some(temp);
        }
        let pending = Pending {
            temp,
            ready_at: now,
            kind: Kind::Preset,
        };
        self.in_flight = Some(pending);
        Some(Command::ApplyPreset(temp))
    }

    pub fn poll(&mut self, now: Instant, connected: bool, powered_on: bool) -> Option<Command> {
        let pending = self.pending?;
        if !connected || self.in_flight.is_some() || now < pending.ready_at {
            return None;
        }
        self.pending = None;
        if !powered_on {
            return Some(Command::SavePending(pending.temp));
        }
        self.in_flight = Some(pending);
        Some(match pending.kind {
            Kind::Manual => Command::WriteProp(pending.temp),
            Kind::Preset => Command::ApplyPreset(pending.temp),
        })
    }

    pub fn finish_write(
        &mut self,
        ok: bool,
        now: Instant,
        connected: bool,
        powered_on: bool,
    ) -> (Option<Command>, Completion) {
        let Some(in_flight) = self.in_flight.take() else {
            return (None, Completion::RequestSnapshot);
        };

        if !ok {
            if self.pending.is_none() {
                self.override_temp = None;
                return (None, Completion::RequestSnapshot);
            }
            return (None, Completion::Continue);
        }

        // Applying a preset is also the successful power-on acknowledgement. A
        // snapshot can still report `on=false` when the user changes the
        // temperature during that slow operation, but the queued value should
        // be sent to the now-running device immediately.
        let queued_preset = self
            .pending
            .is_some_and(|pending| pending.kind == Kind::Preset);
        let powered_on = powered_on || in_flight.kind == Kind::Preset || queued_preset;
        if let Some(next) = self.poll(now, connected, powered_on) {
            return (Some(next), Completion::Continue);
        }
        if self.pending.is_none() {
            (None, Completion::RequestSnapshot)
        } else {
            (None, Completion::Continue)
        }
    }

    pub fn snapshot(&mut self, actual: Option<f64>) -> bool {
        if self.in_flight.is_some() || self.pending.is_some() {
            return false;
        }
        let Some(local) = self.override_temp else {
            return false;
        };
        if actual.is_some_and(|value| (local - value).abs() <= 0.25) {
            self.override_temp = None;
            return true;
        }
        false
    }

    pub fn override_temp(&self) -> Option<f64> {
        self.override_temp
    }

    pub fn in_flight_is_preset(&self) -> bool {
        self.in_flight
            .is_some_and(|pending| pending.kind == Kind::Preset)
    }

    pub fn has_pending_input(&self) -> bool {
        self.pending.is_some()
    }

    pub fn blocks_snapshot(&self) -> bool {
        self.pending.is_some() || self.in_flight.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, Completion, State, SETTLE_AFTER};
    use std::time::{Duration, Instant};

    #[test]
    fn rapid_plus_minus_inputs_only_dispatch_the_final_temperature() {
        let start = Instant::now();
        let mut state = State::default();
        for i in 0..100 {
            let now = start + Duration::from_millis(i * 10);
            state.input(if i % 2 == 0 { 26.5 } else { 26.0 }, now);
            assert_eq!(state.poll(now, true, true), None);
            assert!(!state.snapshot(Some(25.0)));
        }
        let settled = start + Duration::from_millis(990) + SETTLE_AFTER;
        assert_eq!(state.override_temp(), Some(26.0));
        assert_eq!(state.poll(settled, true, true), Some(Command::WriteProp(26.0)));
        assert_eq!(state.poll(settled, true, true), None);
    }

    #[test]
    fn rapid_powered_off_inputs_save_only_the_last_pending_temperature() {
        let start = Instant::now();
        let mut state = State::default();
        for i in 0..31 {
            let now = start + Duration::from_millis(i * 10);
            state.input(16.0 + i as f64 * 0.5, now);
            assert_eq!(state.poll(now, true, false), None);
        }
        let settled = start + Duration::from_millis(300) + SETTLE_AFTER;
        assert_eq!(state.poll(settled, true, false), Some(Command::SavePending(31.0)));
        assert_eq!(state.poll(settled, true, false), None);
        assert_eq!(state.override_temp(), Some(31.0));
    }

    #[test]
    fn latest_manual_input_is_sent_after_the_slow_write_finishes() {
        let start = Instant::now();
        let mut state = State::default();

        state.input(24.0, start);
        assert_eq!(
            state.poll(start + SETTLE_AFTER, true, true),
            Some(Command::WriteProp(24.0))
        );

        state.input(25.0, start + SETTLE_AFTER + Duration::from_millis(1));
        assert_eq!(
            state.poll(start + SETTLE_AFTER + Duration::from_millis(2), true, true),
            None
        );

        let (next, completion) = state.finish_write(
            true,
            start + SETTLE_AFTER * 2 + Duration::from_millis(2),
            true,
            true,
        );
        assert_eq!(next, Some(Command::WriteProp(25.0)));
        assert_eq!(completion, Completion::Continue);
        assert!(state.blocks_snapshot());
    }

    #[test]
    fn old_snapshot_cannot_unlock_a_write_that_is_still_in_flight() {
        let start = Instant::now();
        let mut state = State::default();
        state.input(24.0, start);
        assert_eq!(
            state.poll(start + SETTLE_AFTER, true, true),
            Some(Command::WriteProp(24.0))
        );

        assert!(!state.snapshot(Some(24.0)));
        assert!(state.blocks_snapshot());
        assert_eq!(state.override_temp(), Some(24.0));
    }

    #[test]
    fn failed_write_clears_stale_optimism_and_requests_recovery() {
        let start = Instant::now();
        let mut state = State::default();
        state.input(24.0, start);
        assert_eq!(
            state.poll(start + SETTLE_AFTER, true, true),
            Some(Command::WriteProp(24.0))
        );

        let (next, completion) = state.finish_write(false, start + SETTLE_AFTER, true, true);
        assert_eq!(next, None);
        assert_eq!(completion, Completion::RequestSnapshot);
        assert!(!state.blocks_snapshot());
        assert_eq!(state.override_temp(), None);
        assert!(!state.blocks_snapshot());
    }

    #[test]
    fn a_new_value_survives_failure_of_the_previous_write() {
        let start = Instant::now();
        let mut state = State::default();
        state.input(24.0, start);
        assert_eq!(
            state.poll(start + SETTLE_AFTER, true, true),
            Some(Command::WriteProp(24.0))
        );
        state.input(25.0, start + SETTLE_AFTER + Duration::from_millis(1));

        let (next, completion) = state.finish_write(false, start + SETTLE_AFTER * 2, true, true);
        assert_eq!(next, None);
        assert_eq!(completion, Completion::Continue);
        assert_eq!(state.override_temp(), Some(25.0));

        assert_eq!(
            state.poll(
                start + SETTLE_AFTER * 2 + Duration::from_millis(400),
                true,
                true,
            ),
            Some(Command::WriteProp(25.0))
        );
    }

    #[test]
    fn preset_and_manual_temperature_writes_share_completion_and_snapshot_rules() {
        let start = Instant::now();
        let mut manual = State::default();
        manual.input(24.0, start);
        assert_eq!(
            manual.poll(start + SETTLE_AFTER, true, true),
            Some(Command::WriteProp(24.0))
        );
        assert_eq!(
            manual.finish_write(true, start + SETTLE_AFTER, true, true),
            (None, Completion::RequestSnapshot)
        );

        let mut preset = State::default();
        preset.preset(26.5, start);
        assert_eq!(
            preset.poll(start, true, true),
            Some(Command::ApplyPreset(26.5))
        );
        assert_eq!(
            preset.finish_write(true, start, true, true),
            (None, Completion::RequestSnapshot)
        );
    }

    #[test]
    fn refresh_is_blocked_while_input_or_write_is_pending() {
        let start = Instant::now();
        let mut state = State::default();
        assert!(!state.blocks_snapshot());
        state.input(24.0, start);
        assert!(state.blocks_snapshot());
        assert_eq!(
            state.poll(start + SETTLE_AFTER, true, true),
            Some(Command::WriteProp(24.0))
        );
        assert!(state.blocks_snapshot());
        assert_eq!(
            state.finish_write(true, start + SETTLE_AFTER, true, true),
            (None, Completion::RequestSnapshot)
        );
        assert!(!state.blocks_snapshot());
    }

    #[test]
    fn latest_input_wins_when_power_on_preset_finishes_before_snapshot_catches_up() {
        let start = Instant::now();
        let mut state = State::default();
        assert_eq!(
            state.start_preset(26.0, start),
            Some(Command::ApplyPreset(26.0))
        );

        state.input(24.0, start + Duration::from_millis(1));
        let (next, completion) = state.finish_write(
            true,
            start + SETTLE_AFTER + Duration::from_millis(1),
            true,
            false,
        );
        assert_eq!(next, Some(Command::WriteProp(24.0)));
        assert_eq!(completion, Completion::Continue);
        assert!(state.blocks_snapshot());
    }

    #[test]
    fn preset_is_queued_when_another_temperature_write_is_in_flight() {
        let start = Instant::now();
        let mut state = State::default();
        state.input(24.0, start);
        assert_eq!(
            state.poll(start + SETTLE_AFTER, true, true),
            Some(Command::WriteProp(24.0))
        );

        assert_eq!(state.start_preset(26.5, start + SETTLE_AFTER), None);
        assert_eq!(state.override_temp(), Some(26.5));

        let (next, completion) = state.finish_write(
            true,
            start + SETTLE_AFTER + Duration::from_millis(1),
            true,
            false,
        );
        assert_eq!(next, Some(Command::ApplyPreset(26.5)));
        assert_eq!(completion, Completion::Continue);
    }
}
