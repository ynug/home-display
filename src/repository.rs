//! In-memory store of the latest good data per source plus its status.
//!
//! Services write into the repository; every change publishes a cheap clone of
//! the snapshot (all payloads are `Arc`ed) to a subscriber — the UI bridge.
//! The last good payload is kept when a refresh fails, so the UI can keep
//! showing it with a "stale" marker instead of going blank.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use crate::model::{CalendarData, EarthquakeData, Holidays, RadarData, WeatherData};
use crate::status::{Origin, SourceError, SourceId, SourceStatus};

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub calendar: Option<Arc<CalendarData>>,
    pub weather: Option<Arc<WeatherData>>,
    pub earthquake: Option<Arc<EarthquakeData>>,
    pub radar: Option<Arc<RadarData>>,
    pub holidays: Option<Arc<Holidays>>,
    pub status: BTreeMap<SourceId, SourceStatus>,
    /// Name of the fixture scenario, if any source shows sample data.
    pub fixture_scenario: Option<String>,
    /// Monotonic counter, bumped on every change.
    pub revision: u64,
}

impl Snapshot {
    pub fn status(&self, id: SourceId) -> Option<&SourceStatus> {
        self.status.get(&id)
    }

    pub fn any_sample(&self) -> bool {
        self.status.values().any(|s| s.origin == Origin::Fixture)
    }
}

/// Payload for one source.
#[derive(Debug, Clone)]
pub enum Payload {
    Calendar(Arc<CalendarData>),
    Weather(Arc<WeatherData>),
    Earthquake(Arc<EarthquakeData>),
    Radar(Arc<RadarData>),
    Holidays(Arc<Holidays>),
}

impl Payload {
    pub fn source(&self) -> SourceId {
        match self {
            Payload::Calendar(_) => SourceId::Calendar,
            Payload::Weather(_) => SourceId::Weather,
            Payload::Earthquake(_) => SourceId::Earthquake,
            Payload::Radar(_) => SourceId::Radar,
            Payload::Holidays(_) => SourceId::Holiday,
        }
    }
}

type Subscriber = Box<dyn Fn(Snapshot) + Send + Sync>;

pub struct Repository {
    inner: Mutex<Snapshot>,
    subscriber: Mutex<Option<Subscriber>>,
}

impl Repository {
    pub fn new(initial: Snapshot) -> Arc<Self> {
        Arc::new(Repository {
            inner: Mutex::new(initial),
            subscriber: Mutex::new(None),
        })
    }

    pub fn subscribe(&self, f: impl Fn(Snapshot) + Send + Sync + 'static) {
        *self.subscriber.lock().unwrap() = Some(Box::new(f));
        self.publish();
    }

    pub fn snapshot(&self) -> Snapshot {
        self.inner.lock().unwrap().clone()
    }

    fn publish(&self) {
        let snap = self.snapshot();
        if let Some(f) = self.subscriber.lock().unwrap().as_ref() {
            f(snap);
        }
    }

    fn mutate(&self, f: impl FnOnce(&mut Snapshot)) {
        {
            let mut s = self.inner.lock().unwrap();
            f(&mut s);
            s.revision += 1;
        }
        self.publish();
    }

    pub fn set_status(&self, id: SourceId, status: SourceStatus) {
        self.mutate(|s| {
            s.status.insert(id, status);
        });
    }

    pub fn update_status(&self, id: SourceId, f: impl FnOnce(&mut SourceStatus)) {
        self.mutate(|s| {
            if let Some(st) = s.status.get_mut(&id) {
                f(st);
            }
        });
    }

    pub fn set_fixture_scenario(&self, name: Option<String>) {
        self.mutate(|s| s.fixture_scenario = name);
    }

    pub fn mark_loading(&self, id: SourceId) {
        self.update_status(id, |st| st.loading = true);
    }

    /// Store new data and mark the source healthy.
    pub fn store(&self, payload: Payload, now: DateTime<Utc>, next: Option<DateTime<Utc>>) {
        let id = payload.source();
        self.mutate(|s| {
            match payload {
                Payload::Calendar(d) => s.calendar = Some(d),
                Payload::Weather(d) => s.weather = Some(d),
                Payload::Earthquake(d) => s.earthquake = Some(d),
                Payload::Radar(d) => s.radar = Some(d),
                Payload::Holidays(d) => s.holidays = Some(d),
            }
            if let Some(st) = s.status.get_mut(&id) {
                st.record_success(now);
                st.next_attempt = next;
            }
        });
    }

    /// Record a failure; previously stored data is intentionally kept.
    pub fn fail(&self, id: SourceId, err: SourceError, now: DateTime<Utc>, next: Option<DateTime<Utc>>) {
        self.mutate(|s| {
            if let Some(st) = s.status.get_mut(&id) {
                st.record_failure(now, err);
                st.next_attempt = next;
            }
        });
    }

    /// Store data loaded from a cache: shown, but with its original timestamp.
    /// Forget a source's data and status history (e.g. after the location
    /// changed), so the UI shows "loading" instead of data for the old place.
    pub fn reset(&self, id: SourceId) {
        self.mutate(|s| {
            match id {
                SourceId::Calendar => s.calendar = None,
                SourceId::Weather => s.weather = None,
                SourceId::Earthquake => s.earthquake = None,
                SourceId::Radar => s.radar = None,
                SourceId::Holiday => s.holidays = None,
            }
            if let Some(st) = s.status.get_mut(&id) {
                *st = SourceStatus::new(st.origin.clone(), st.stale_after);
                st.loading = true;
            }
        });
    }

    pub fn store_cached(&self, payload: Payload, fetched_at: DateTime<Utc>) {
        let id = payload.source();
        self.mutate(|s| {
            match payload {
                Payload::Calendar(d) => s.calendar = Some(d),
                Payload::Weather(d) => s.weather = Some(d),
                Payload::Earthquake(d) => s.earthquake = Some(d),
                Payload::Radar(d) => s.radar = Some(d),
                Payload::Holidays(d) => s.holidays = Some(d),
            }
            if let Some(st) = s.status.get_mut(&id) {
                st.last_success = Some(fetched_at);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{ErrorKind, Health};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn failure_keeps_last_good_data() {
        let repo = Repository::new(Snapshot::default());
        repo.set_status(
            SourceId::Calendar,
            SourceStatus::new(Origin::Live("ical".into()), chrono::Duration::minutes(60)),
        );
        let t0 = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        repo.store(Payload::Calendar(Arc::new(CalendarData::default())), t0, None);
        repo.fail(SourceId::Calendar, SourceError::new(ErrorKind::Network, "x"), t0, None);
        let s = repo.snapshot();
        assert!(s.calendar.is_some());
        assert_eq!(s.status(SourceId::Calendar).unwrap().health(t0), Health::Retrying);
    }

    #[test]
    fn subscriber_sees_every_revision() {
        let repo = Repository::new(Snapshot::default());
        let seen = Arc::new(AtomicU64::new(0));
        let s2 = seen.clone();
        repo.subscribe(move |snap| s2.store(snap.revision, Ordering::SeqCst));
        repo.mark_loading(SourceId::Weather);
        repo.set_status(
            SourceId::Weather,
            SourceStatus::new(Origin::Fixture, chrono::Duration::minutes(1)),
        );
        assert_eq!(seen.load(Ordering::SeqCst), 2);
    }
}
