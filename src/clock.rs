//! Wall clock with an optional fixed offset (`--now`) so fixture screens and
//! screenshots can be reproduced at a chosen time while still ticking.

use std::sync::OnceLock;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;

static OFFSET: OnceLock<Duration> = OnceLock::new();

/// Pretend the current time is `at`. Can only be set once, before startup.
pub fn set_fake_now(at: DateTime<Utc>) {
    let _ = OFFSET.set(at - Utc::now());
}

pub fn is_faked() -> bool {
    OFFSET.get().is_some()
}

pub fn now_utc() -> DateTime<Utc> {
    Utc::now() + OFFSET.get().copied().unwrap_or_else(Duration::zero)
}

pub fn now_in(tz: Tz) -> DateTime<Tz> {
    now_utc().with_timezone(&tz)
}
