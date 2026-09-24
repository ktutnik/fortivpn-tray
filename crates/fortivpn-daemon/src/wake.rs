//! Notices that the machine slept, from the daemon's own two clocks.
//!
//! `Instant` stops while the machine sleeps (macOS: `CLOCK_UPTIME_RAW`,
//! Linux: `CLOCK_MONOTONIC`); the wall clock keeps going. A tick that finds the
//! wall clock far ahead of the monotonic clock means the process was frozen in
//! between — the machine slept and has just woken.
//!
//! Every tokio timer runs on the monotonic clock as well, so nothing else in the
//! daemon notices a sleep on its own: the LCP echo budget, a reconnect backoff
//! and a half-finished connect all resume as if no time had passed.

use std::time::{Duration, Instant, SystemTime};

use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// How often the clocks are compared — also the worst-case delay between
/// waking and noticing.
const TICK: Duration = Duration::from_secs(3);

/// Smallest clock gap treated as a sleep. Well above scheduling jitter, far
/// below any real sleep (a dark wake cycle is minutes long).
const MIN_SLEEP: Duration = Duration::from_secs(10);

/// Start the detector. Each wake is broadcast with how long the machine slept.
pub fn spawn(wake_tx: broadcast::Sender<Duration>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(TICK);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_mono = Instant::now();
        let mut last_wall = SystemTime::now();
        loop {
            timer.tick().await;
            let (mono, wall) = (Instant::now(), SystemTime::now());
            // A wall clock stepped backwards (NTP) reads as no sleep.
            let wall_elapsed = wall.duration_since(last_wall).unwrap_or_default();
            if let Some(slept) = sleep_gap(mono.duration_since(last_mono), wall_elapsed) {
                log::info!(
                    target: "daemon",
                    "System woke after {:.1} min asleep",
                    slept.as_secs_f64() / 60.0
                );
                let _ = wake_tx.send(slept);
            }
            last_mono = mono;
            last_wall = wall;
        }
    })
}

/// How long the machine slept between two ticks, if it did.
fn sleep_gap(mono_elapsed: Duration, wall_elapsed: Duration) -> Option<Duration> {
    let gap = wall_elapsed.saturating_sub(mono_elapsed);
    (gap >= MIN_SLEEP).then_some(gap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sleep_gap_ignores_an_ordinary_tick() {
        assert_eq!(sleep_gap(TICK, TICK + Duration::from_millis(40)), None);
    }

    #[test]
    fn test_sleep_gap_reports_a_sleep() {
        let slept = Duration::from_secs(2 * 3600);
        assert_eq!(sleep_gap(TICK, TICK + slept), Some(slept));
    }

    #[test]
    fn test_sleep_gap_ignores_small_wall_clock_corrections() {
        assert_eq!(sleep_gap(TICK, TICK + Duration::from_secs(2)), None);
    }

    #[test]
    fn test_sleep_gap_wall_clock_behind_is_not_a_sleep() {
        assert_eq!(sleep_gap(TICK, Duration::ZERO), None);
    }
}
