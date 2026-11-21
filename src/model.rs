use std::{collections::HashMap, time::Duration};

#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    pub upload: Option<f64>,
    pub download: Option<f64>,
    pub cpu: Option<f64>,
    pub memory: Option<f64>,
    pub disk: Option<f64>,
    pub traffic: u64,
    pub available_memory: Option<u64>,
    pub cpu_mhz: Option<f64>,
    pub battery: Option<f64>,
    pub uptime: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Upload,
    Download,
    Cpu,
    Memory,
    Disk,
    Traffic,
    AvailableMemory,
    CpuFrequency,
    Uptime,
    Battery,
}

impl Metric {
    pub const ALL: [Self; 10] = [
        Self::Upload,
        Self::Download,
        Self::Cpu,
        Self::Memory,
        Self::Disk,
        Self::Traffic,
        Self::AvailableMemory,
        Self::CpuFrequency,
        Self::Uptime,
        Self::Battery,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Download => "download",
            Self::Cpu => "cpu",
            Self::Memory => "memory",
            Self::Disk => "disk",
            Self::Traffic => "traffic",
            Self::AvailableMemory => "available_memory",
            Self::CpuFrequency => "cpu_frequency",
            Self::Battery => "battery",
            Self::Uptime => "uptime",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    order: [Metric; Metric::ALL.len()],
    visible: u16,
}

impl Metrics {
    pub fn parse(value: &str) -> Self {
        let mut metrics = Self {
            order: Metric::ALL,
            visible: 0,
        };
        let mut ordered = 0;
        let mut seen = 0;
        for entry in value.split(',').map(str::trim) {
            let hidden = entry.strip_prefix('-');
            let key = hidden.unwrap_or(entry);
            let Some(metric) = Metric::ALL.into_iter().find(|metric| metric.key() == key) else {
                continue;
            };
            let bit = 1 << metric.index();
            if seen & bit == 0 {
                seen |= bit;
                metrics.order[ordered] = metric;
                ordered += 1;
                metrics.set_visible(metric, hidden.is_none());
            }
        }
        if seen == 0 && !value.trim().is_empty() {
            return Self::default();
        }
        for metric in Metric::ALL
            .into_iter()
            .filter(|metric| seen & (1 << metric.index()) == 0)
        {
            metrics.order[ordered] = metric;
            ordered += 1;
        }
        metrics
    }

    pub fn to_config(self) -> String {
        self.order
            .into_iter()
            .map(|metric| {
                if self.contains(metric) {
                    metric.key().to_owned()
                } else {
                    format!("-{}", metric.key())
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    pub const fn ordered(self) -> [Metric; Metric::ALL.len()] {
        self.order
    }

    pub const fn contains(self, metric: Metric) -> bool {
        self.visible & (1 << metric.index()) != 0
    }

    pub fn set_visible(&mut self, metric: Metric, visible: bool) {
        let bit = 1 << metric.index();
        if visible {
            self.visible |= bit;
        } else {
            self.visible &= !bit;
        }
    }

    pub fn move_to(&mut self, from: usize, to: usize) {
        if from < to {
            self.order[from..=to].rotate_left(1);
        } else if from > to {
            self.order[to..=from].rotate_right(1);
        }
    }

    pub fn iter(self) -> impl Iterator<Item = Metric> {
        self.order
            .into_iter()
            .filter(move |&metric| self.contains(metric))
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            order: Metric::ALL,
            visible: (1 << Metric::AvailableMemory.index()) - 1,
        }
    }
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
            Some(value) => Self::scale(value, &["B/s", "KB/s", "MB/s", "GB/s", "TB/s"]),
            None => Self::unavailable(),
        }
    }

    fn bytes(bytes: u64) -> Self {
        Self::scale(bytes as f64, &["B", "KB", "MB", "GB", "TB", "PB", "EB"])
    }

    fn uptime(duration: Duration) -> Self {
        let seconds = duration.as_secs();
        let days = seconds / 86_400;
        let hours = seconds / 3_600;
        let minutes = seconds / 60;
        if days >= 1000 {
            Self {
                number: days.to_string(),
                unit: "天",
            }
        } else if days > 0 {
            Self {
                number: format!("{days}天 {}时", hours % 24),
                unit: "",
            }
        } else if hours > 0 {
            Self {
                number: format!("{hours}时 {}分", minutes % 60),
                unit: "",
            }
        } else if minutes > 0 {
            Self {
                number: minutes.to_string(),
                unit: "分钟",
            }
        } else {
            Self {
                number: seconds.to_string(),
                unit: "秒",
            }
        }
    }

    fn frequency(mhz: Option<f64>) -> Self {
        match mhz.filter(|mhz| mhz.is_finite() && *mhz > 0.0) {
            Some(mhz) => Self {
                number: format!("{:.2}", mhz / 1000.0),
                unit: "GHz",
            },
            None => Self::unavailable(),
        }
    }

    fn scale(mut value: f64, units: &[&'static str]) -> Self {
        for (index, &unit) in units.iter().enumerate() {
            if index + 1 == units.len() || value.round() < 1000.0 {
                return Self {
                    number: format!("{value:.0}"),
                    unit,
                };
            }
            value /= 1000.0;
        }
        unreachable!("byte units are nonempty")
    }
}

pub fn readings(sample: Sample, total: u64) -> [Reading; Metric::ALL.len()] {
    Metric::ALL.map(|metric| match metric {
        Metric::Upload => Reading::rate(sample.upload),
        Metric::Download => Reading::rate(sample.download),
        Metric::Cpu => Reading::percent(sample.cpu),
        Metric::Memory => Reading::percent(sample.memory),
        Metric::Disk => Reading::percent(sample.disk),
        Metric::Traffic => Reading::bytes(total),
        Metric::AvailableMemory => sample
            .available_memory
            .map_or_else(Reading::unavailable, Reading::bytes),
        Metric::CpuFrequency => Reading::frequency(sample.cpu_mhz),
        Metric::Battery => Reading::percent(sample.battery),
        Metric::Uptime => sample
            .uptime
            .map_or_else(Reading::unavailable, Reading::uptime),
    })
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
            available_memory: Some(8_530_000_000),
            cpu_mhz: Some(3424.0),
            battery: None,
            uptime: Some(Duration::from_secs(86_400 + 3_600)),
        };
        let displayed = readings(sample, sample.traffic);
        let pairs = displayed
            .each_ref()
            .map(|value| (value.number.as_str(), value.unit));
        assert_eq!(
            pairs,
            [
                ("1", "MB/s"),
                ("21", "KB/s"),
                ("12", "%"),
                ("57", "%"),
                ("—", ""),
                ("16", "GB"),
                ("9", "GB"),
                ("3.42", "GHz"),
                ("1天 1时", ""),
                ("—", ""),
            ]
        );
    }

    #[test]
    fn custom_order_and_visibility_round_trip() {
        let mut metrics = Metrics::parse("cpu, -uptime, available_memory, unknown, cpu");
        assert_eq!(
            metrics.iter().collect::<Vec<_>>(),
            [Metric::Cpu, Metric::AvailableMemory]
        );
        metrics.move_to(2, 0);
        metrics.set_visible(Metric::Cpu, false);
        metrics.set_visible(Metric::Uptime, true);
        assert_eq!(
            metrics.iter().collect::<Vec<_>>(),
            [Metric::AvailableMemory, Metric::Uptime]
        );
        assert_eq!(metrics.ordered()[1], Metric::Cpu);
        assert_eq!(Metrics::parse(&metrics.to_config()), metrics);
        let empty = Metrics::parse("");
        assert_eq!(empty.iter().count(), 0);
        assert_eq!(Metrics::parse(&empty.to_config()), empty);
        assert_eq!(Metrics::parse("unknown"), Metrics::default());
    }
}
