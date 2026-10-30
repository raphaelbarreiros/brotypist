//! Truthful shared power sampling and once-per-discharge help policy.
use crate::system::PowerStatus;
use anyhow::Result;
use std::sync::{Arc, Condvar, LazyLock, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct LowBatteryHelpPolicy { asked: bool }
impl LowBatteryHelpPolicy {
    pub fn new() -> Self { Self::default() }
    /// Call only after the help surface has established its eligibility.
    pub fn should_ask(&mut self, reading: &PowerStatus) -> bool {
        if !reading.available || !reading.has_battery { return false; }
        if !reading.on_battery { self.asked = false; return false; }
        if reading.charging != Some(false) || !reading.percent.is_some_and(|p| p <= 20) || self.asked { return false; }
        self.asked = true;
        true
    }
}

struct State { reading: PowerStatus, sampled: Option<Instant>, stopping: bool }
impl State {
    fn delay(&self, now: Instant) -> Duration {
        self.sampled.map_or(Duration::ZERO, |when| Duration::from_secs(15).saturating_sub(now.saturating_duration_since(when)))
    }
    fn current(&self, now: Instant) -> PowerStatus {
        if self.stopping || self.sampled.is_none_or(|when| now.saturating_duration_since(when) >= Duration::from_secs(19)) { PowerStatus::default() } else { self.reading.clone() }
    }
}
struct Shared { state: Mutex<State>, wake: Condvar }
struct Owner { shared: Arc<Shared>, thread: Mutex<Option<JoinHandle<()>>> }
impl Drop for Owner {
    fn drop(&mut self) {
        { let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner()); state.stopping = true; }
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.get_mut().unwrap_or_else(|p| p.into_inner()).take() { let _ = thread.join(); }
    }
}
struct Registry { owner: Weak<Owner>, cache: Weak<Shared> }
static SAMPLER: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(Registry { owner: Weak::new(), cache: Weak::new() }));
/// App-lifetime sampler lease. UI and runtime share one sampler, and the final lease joins it.
#[derive(Clone)]
pub struct PowerSampler { owner: Option<Arc<Owner>> }
impl PowerSampler {
    pub fn start() -> Result<Self> {
        let mut slot = SAMPLER.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(owner) = slot.owner.upgrade() { return Ok(Self { owner: Some(owner) }); }
        let owner = spawn_sampler(crate::system::sample_power_status)?;
        slot.owner = Arc::downgrade(&owner);
        slot.cache = Arc::downgrade(&owner.shared);
        Ok(Self { owner: Some(owner) })
    }
    /// Release this lease; other owners remain valid. Last lease synchronously stops the worker.
    pub fn stop(&mut self) -> Result<()> { self.owner.take(); Ok(()) }
}

fn spawn_sampler(mut sample: impl FnMut() -> Result<PowerStatus> + Send + 'static) -> Result<Arc<Owner>> {
    let shared = Arc::new(Shared { state: Mutex::new(State { reading: PowerStatus::default(), sampled: None, stopping: false }), wake: Condvar::new() });
    let worker = shared.clone();
    let thread = std::thread::Builder::new().name("brotypist-power".into()).spawn(move || {
        loop {
            if worker.state.lock().unwrap_or_else(|p| p.into_inner()).stopping { break; }
            let reading = sample().unwrap_or_default();
            let mut state = worker.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.stopping { break; }
            state.reading = reading;
            state.sampled = Some(Instant::now());
            while !state.stopping {
                let remaining = state.delay(Instant::now());
                if remaining.is_zero() { break; }
                state = worker.wake.wait_timeout(state, remaining).unwrap_or_else(|p| p.into_inner()).0;
            }
            if state.stopping { break; }
        }
    })?;
    Ok(Arc::new(Owner { shared, thread: Mutex::new(Some(thread)) }))
}

pub(crate) fn cached_status() -> PowerStatus {
    let shared = SAMPLER.lock().unwrap_or_else(|p| p.into_inner()).cache.upgrade();
    let Some(shared) = shared else { return PowerStatus::default(); };
    // Upgrading only the cache cannot become the last sampler owner and join on a typing thread.
    shared.state.lock().unwrap_or_else(|p| p.into_inner()).current(Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn low(percent: u32) -> PowerStatus { PowerStatus { available: true, has_battery: true, on_battery: true, charging: Some(false), percent: Some(percent) } }
    #[test]
    fn twenty_percent_inclusive_and_unknown_never_rearms() {
        let mut policy = LowBatteryHelpPolicy::new();
        assert!(!policy.should_ask(&low(21)));
        assert!(policy.should_ask(&low(20)));
        assert!(!policy.should_ask(&PowerStatus::default()));
        assert!(!policy.should_ask(&low(19)));
        assert!(!policy.should_ask(&PowerStatus { available: false, on_battery: false, ..low(19) }));
        assert!(!policy.should_ask(&low(0)));
        assert!(!policy.should_ask(&PowerStatus { on_battery: false, charging: None, percent: None, ..low(19) }));
        assert!(policy.should_ask(&low(0)));
        assert!(!policy.should_ask(&low(0)));
    }
    #[test]
    fn charging_and_missing_percent_never_ask() {
        for value in [PowerStatus::default(), PowerStatus { charging: None, ..low(19) }, PowerStatus { charging: Some(true), ..low(19) }, PowerStatus { percent: None, ..low(19) }, low(101)] {
            assert!(!LowBatteryHelpPolicy::new().should_ask(&value));
        }
    }
    #[test]
    fn cache_cadence_is_fifteen_seconds_from_sample_completion() {
        let completion = Instant::now();
        let mut state = State { reading: low(19), sampled: Some(completion), stopping: false };
        assert_eq!(state.delay(completion), Duration::from_secs(15));
        assert_eq!(state.delay(completion + Duration::from_millis(14999)), Duration::from_millis(1));
        assert_eq!(state.delay(completion + Duration::from_secs(15)), Duration::ZERO);
        assert_eq!(state.current(completion), low(19));
        state.reading = PowerStatus::default();
        state.sampled = Some(completion + Duration::from_secs(18));
        assert_eq!(state.current(completion + Duration::from_secs(20)), PowerStatus::default());
        assert_eq!(state.delay(completion + Duration::from_secs(20)), Duration::from_secs(13));
        state.reading = low(19);
        assert_eq!(state.current(completion + Duration::from_secs(37)), PowerStatus::default());
        state.stopping = true;
        assert_eq!(state.current(completion + Duration::from_secs(18)), PowerStatus::default());
    }
    #[test]
    fn sampling_is_off_caller_and_final_shared_lease_stops_and_joins() {
        let caller = std::thread::current().id();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let owner = spawn_sampler(move || {
            send.send(std::thread::current().id()).unwrap();
            Ok(low(19))
        }).unwrap();
        assert_ne!(receive.recv_timeout(Duration::from_secs(1)).unwrap(), caller);
        let weak = Arc::downgrade(&owner);
        let shared = owner.shared.clone();
        let mut first = PowerSampler { owner: Some(owner) };
        let mut second = first.clone();
        first.stop().unwrap();
        assert!(!shared.state.lock().unwrap().stopping);
        assert!(weak.upgrade().is_some());
        second.stop().unwrap();
        assert!(shared.state.lock().unwrap().stopping);
        assert!(weak.upgrade().is_none());
        first.stop().unwrap();
        second.stop().unwrap();
    }
    #[test]
    fn failed_sampler_replaces_low_reading_with_unknown_without_retry_burst() {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let owner = spawn_sampler(move || { send.send(()).unwrap(); anyhow::bail!("synthetic native failure"); }).unwrap();
        receive.recv_timeout(Duration::from_secs(1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let state = owner.shared.state.lock().unwrap();
            if state.sampled.is_some() {
                assert_eq!(state.reading, PowerStatus::default());
                assert!(state.delay(Instant::now()) > Duration::from_secs(14));
                break;
            }
            drop(state);
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(receive.try_recv().is_err());
        drop(owner);
    }
}
