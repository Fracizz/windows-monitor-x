use std::{collections::HashMap, time::Duration};

#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    pub upload: Option<f64>,
    pub download: Option<f64>,
    pub cpu: Option<f64>,
    pub memory: Option<f64>,
    pub disk: Option<f64>,
    pub traffic: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bytes {
    pub received: u64,
    pub sent: u64,
}

pub fn traffic_delta(previous: &HashMap<u64, Bytes>, current: &HashMap<u64, Bytes>) -> Bytes {
    let mut delta = Bytes::default();
    for (id, new) in current {
        if let Some(old) = previous.get(id)
            && let (Some(received), Some(sent)) = (
                new.received.checked_sub(old.received),
                new.sent.checked_sub(old.sent),
            )
        {
            delta.received = delta.received.saturating_add(received);
            delta.sent = delta.sent.saturating_add(sent);
        }
    }
    delta
}

pub fn rate(bytes: u64, elapsed: Duration, interval: Duration) -> Option<f64> {
    (!elapsed.is_zero() && elapsed <= interval * 2 + Duration::from_secs(2))
        .then(|| bytes as f64 / elapsed.as_secs_f64())
}

#[derive(Clone, Copy, Default)]
pub struct CpuTime {
    pub idle: u64,
    pub kernel: u64,
    pub user: u64,
}

impl CpuTime {
    pub fn utilization(self, previous: Self) -> Option<f64> {
        let idle = self.idle.checked_sub(previous.idle)?;
        let kernel = self.kernel.checked_sub(previous.kernel)?;
        let user = self.user.checked_sub(previous.user)?;
        let total = kernel.checked_add(user)?;
        let busy = total.checked_sub(idle)?;
        (total > 0).then(|| 100.0 * busy as f64 / total as f64)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    pub number: String,
    pub unit: &'static str,
}

impl Reading {
    pub fn unavailable() -> Self {
        Self {
            number: "—".into(),
            unit: "",
        }
    }

    fn percent(value: Option<f64>) -> Self {
        match value.filter(|value| value.is_finite()) {
            Some(value) => Self {
                number: format!("{:.0}", value.clamp(0.0, 100.0)),
                unit: "%",
            },
            None => Self::unavailable(),
        }
    }

    fn rate(value: Option<f64>) -> Self {
        match value {
            Some(value) => Self::scale(value, &["B/s", "KB/s", "MB/s", "GB/s", "TB/s"], false),
            None => Self::unavailable(),
        }
    }

    fn traffic(bytes: u64) -> Self {
        Self::scale(
            bytes as f64,
            &["B", "KB", "MB", "GB", "TB", "PB", "EB"],
            true,
        )
    }

    fn scale(mut value: f64, units: &[&'static str], total: bool) -> Self {
        for (index, &unit) in units.iter().enumerate() {
            let precision = match (index, total) {
                (0, _) => 0,
                (_, true) => 2,
                (_, false) if value < 10.0 => 1,
                _ => 0,
            };
            let factor = 10_f64.powi(precision as i32);
            let rounds_to_next_unit = (value * factor).round() >= 1000.0 * factor;
            if index + 1 == units.len() || !rounds_to_next_unit {
                return Self {
                    number: format!("{value:.precision$}"),
                    unit,
                };
            }
            value /= 1000.0;
        }
        unreachable!("byte units are nonempty")
    }
}

pub fn readings(sample: Sample, total: u64) -> [Reading; 6] {
    [
        Reading::rate(sample.upload),
        Reading::rate(sample.download),
        Reading::percent(sample.cpu),
        Reading::percent(sample.memory),
        Reading::percent(sample.disk),
        Reading::traffic(total),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_rebases_new_and_reset_interfaces() {
        let counters = |received, sent| Bytes { received, sent };
        let old = HashMap::from([
            (1, counters(90, 10)),
            (2, counters(200, 100)),
            (4, counters(2, 2)),
        ]);
        let new = HashMap::from([
            (1, counters(110, 15)),
            (2, counters(3, 1)),
            (3, counters(100_000, 100)),
        ]);
        assert_eq!(traffic_delta(&old, &new), counters(20, 5));
    }

    #[test]
    fn rates_use_elapsed_time_and_recover_from_sleep() {
        assert_eq!(
            rate(300, Duration::from_millis(1500), Duration::from_secs(1)),
            Some(200.0)
        );
        assert_eq!(rate(300, Duration::ZERO, Duration::from_secs(1)), None);
        assert_eq!(
            rate(300, Duration::from_secs(20), Duration::from_secs(5)),
            None
        );
        assert_eq!(
            rate(500, Duration::from_secs(5), Duration::from_secs(5)),
            Some(100.0)
        );
    }

    #[test]
    fn cpu_idle_is_included_in_kernel_time() {
        let current = CpuTime {
            idle: 70,
            kernel: 90,
            user: 10,
        };
        let first = CpuTime::default();
        assert_eq!(current.utilization(first), Some(30.0));
        assert_eq!(current.utilization(current), None);
        assert_eq!(first.utilization(current), None);
    }

    #[test]
    fn metrics_use_consistent_units_including_at_boundaries() {
        let sample = Sample {
            upload: Some(999_900.0),
            download: Some(21_000.0),
            cpu: Some(12.3),
            memory: Some(57.0),
            disk: None,
            traffic: 16_360_000_000,
        };
        let displayed = readings(sample, sample.traffic);
        let pairs = displayed
            .each_ref()
            .map(|value| (value.number.as_str(), value.unit));
        assert_eq!(
            pairs,
            [
                ("1.0", "MB/s"),
                ("21", "KB/s"),
                ("12", "%"),
                ("57", "%"),
                ("—", ""),
                ("16.36", "GB")
            ]
        );
    }
}
