//! Logging: stderr (→ journald under systemd, which rotates) plus a small
//! fixed-size ring buffer of recent warnings/errors for the settings screen.
//! Nothing is written to unbounded log files.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

const CAPACITY: usize = 12;

static RECENT: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn buffer() -> &'static Mutex<VecDeque<String>> {
    RECENT.get_or_init(|| Mutex::new(VecDeque::with_capacity(CAPACITY)))
}

/// Most recent first.
pub fn recent() -> Vec<String> {
    buffer()
        .lock()
        .map(|b| b.iter().rev().cloned().collect())
        .unwrap_or_default()
}

pub fn push(line: String) {
    if let Ok(mut b) = buffer().lock() {
        if b.len() == CAPACITY {
            b.pop_front();
        }
        b.push_back(line);
    }
}

struct RingLayer;

#[derive(Default)]
struct Collect(String);

impl Visit for Collect {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();
        if level > Level::WARN {
            return;
        }
        let mut c = Collect::default();
        event.record(&mut c);
        let time = crate::clock::now_utc()
            .with_timezone(&chrono::Local)
            .format("%m/%d %H:%M:%S");
        let tag = if level == Level::ERROR { "ERROR" } else { "WARN " };
        let mut line = format!("{time} {tag} {}", crate::services::redact_urls(&c.0));
        line.truncate(line.floor_char_boundary(240));
        push(line);
    }
}

/// `RUST_LOG` overrides the default `info` level.
pub fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,reqwest=warn,hyper=warn"));
    let journald = std::env::var_os("JOURNAL_STREAM").is_some();
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(!journald)
        .with_target(false);
    // journald adds its own timestamps.
    let fmt = if journald {
        fmt.without_time().boxed()
    } else {
        fmt.boxed()
    };
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt)
        .with(RingLayer)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_is_bounded() {
        for i in 0..(CAPACITY + 5) {
            push(format!("line {i}"));
        }
        let r = recent();
        assert_eq!(r.len(), CAPACITY);
        assert_eq!(r[0], format!("line {}", CAPACITY + 4));
    }
}
