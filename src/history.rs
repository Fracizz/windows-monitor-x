use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
};

use crate::model::{Metric, Sample};

pub const RETENTION: u64 = 7 * 24 * 60 * 60 * 1_000;
const HOUR: u64 = 60 * 60 * 1_000;
// Change the versioned history directory if the order or layout of stored values changes.
const STORED_METRICS: [Metric; 16] = Metric::ALL;
const RECORD_SIZE: usize = (2 + STORED_METRICS.len()) * 8;
pub const COLUMNS: usize = 960;

#[cfg(windows)]
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}

impl Range {
    pub fn recent(start: u64, end: u64, now: u64) -> Option<Self> {
        let start = start.max(now.saturating_sub(RETENTION));
        let end = end.min(now);
        (start <= end).then_some(Self { start, end })
    }
}

pub fn prune(directory: &Path, now: u64) -> io::Result<()> {
    let oldest_hour = now.saturating_sub(RETENTION) / HOUR;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "bin")
            && let Some(hour) = path.file_stem().and_then(|stem| stem.to_str())
            && hour.parse::<u64>().is_ok_and(|hour| hour < oldest_hour)
        {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn hour_path(directory: &Path, hour: u64) -> PathBuf {
    directory.join(format!("{hour}.bin"))
}

pub struct Writer {
    directory: PathBuf,
    current: Option<(u64, File)>,
}

impl Writer {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            current: None,
        }
    }

    pub fn record(
        &mut self,
        time: u64,
        cadence_ms: u64,
        sample: Sample,
        traffic: u64,
    ) -> io::Result<()> {
        let hour = time / HOUR;
        if self
            .current
            .as_ref()
            .is_none_or(|(current, _)| *current != hour)
        {
            fs::create_dir_all(&self.directory)?;
            let path = hour_path(&self.directory, hour);
            let file = OpenOptions::new().append(true).create(true).open(&path)?;
            let len = file.metadata()?.len();
            let complete = len - len % RECORD_SIZE as u64;
            if len != complete {
                // An interrupted process can leave a partial record at the end of the file.
                OpenOptions::new()
                    .write(true)
                    .open(path)?
                    .set_len(complete)?;
            }
            self.current = Some((hour, file));
        }
        let mut record = [0; RECORD_SIZE];
        record[..8].copy_from_slice(&time.to_le_bytes());
        record[8..16].copy_from_slice(&cadence_ms.to_le_bytes());
        for (bytes, value) in record[16..]
            .chunks_exact_mut(8)
            .zip(values(sample, traffic))
        {
            bytes.copy_from_slice(&value.unwrap_or(f64::NAN).to_le_bytes());
        }
        let result = self.current.as_mut().unwrap().1.write_all(&record);
        if result.is_err() {
            self.current = None;
        }
        result
    }
}

fn values(sample: Sample, traffic: u64) -> [Option<f64>; Metric::ALL.len()] {
    let memory = sample
        .memory
        .filter(|memory| memory.total > 0 && memory.available <= memory.total);
    let gpu = sample.gpu_memory.filter(|memory| memory.total > 0);
    STORED_METRICS.map(|metric| match metric {
        Metric::Upload => sample.upload,
        Metric::Download => sample.download,
        Metric::Cpu => sample.cpu,
        Metric::Memory => memory
            .map(|memory| 100.0 * (memory.total - memory.available) as f64 / memory.total as f64),
        Metric::Disk => sample.disk,
        Metric::Traffic => Some(traffic as f64),
        Metric::AvailableMemory => memory.map(|memory| memory.available as f64),
        Metric::CpuFrequency => sample.cpu_mhz.filter(|mhz| *mhz > 0.0),
        Metric::Uptime => sample.uptime.map(|duration| duration.as_secs_f64()),
        Metric::Battery => sample.battery,
        Metric::AvailableMemoryPercent => {
            memory.map(|memory| 100.0 * memory.available as f64 / memory.total as f64)
        }
        Metric::UsedMemory => memory.map(|memory| (memory.total - memory.available) as f64),
        Metric::DiskRead => sample.disk_read,
        Metric::DiskWrite => sample.disk_write,
        Metric::GpuMemoryPercent => {
            gpu.map(|memory| 100.0 * memory.used as f64 / memory.total as f64)
        }
        Metric::UsedGpuMemory => sample.gpu_memory.map(|memory| memory.used as f64),
    })
}

#[derive(Clone, Copy, Debug)]
pub struct Column {
    pub first: f64,
    pub last: f64,
    pub min: f64,
    pub max: f64,
    pub join: Option<usize>,
}

pub struct Series {
    pub range: Range,
    pub columns: Vec<Option<Column>>,
    pub count: u64,
    pub min: f64,
    pub max: f64,
    sum: f64,
}

impl Series {
    pub fn average(&self) -> f64 {
        self.sum / self.count as f64
    }
}

pub fn query(directory: &Path, range: Range, metric: Metric) -> io::Result<Series> {
    let mut data = Series {
        range,
        columns: vec![None; COLUMNS],
        count: 0,
        min: f64::INFINITY,
        max: 0.0,
        sum: 0.0,
    };
    let mut previous: Option<(u64, u64, usize)> = None;
    let offset = 16 + 8 * metric.index();
    for hour in (range.start / HOUR)..=(range.end / HOUR) {
        let file = match File::open(hour_path(directory, hour)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let mut reader = BufReader::new(file);
        let mut record = [0; RECORD_SIZE];
        loop {
            match reader.read_exact(&mut record) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error),
            }
            let time = u64::from_le_bytes(record[..8].try_into().unwrap());
            if time < range.start || time > range.end {
                continue;
            }
            let cadence = u64::from_le_bytes(record[8..16].try_into().unwrap());
            let value = f64::from_le_bytes(record[offset..offset + 8].try_into().unwrap());
            if !value.is_finite() || value < 0.0 {
                previous = None;
                continue;
            }
            data.count += 1;
            data.min = data.min.min(value);
            data.max = data.max.max(value);
            data.sum += value;
            let index = ((time - range.start) * (COLUMNS as u64 - 1)
                / (range.end - range.start).max(1)) as usize;
            let column = &mut data.columns[index];
            if let Some(column) = column {
                column.last = value;
                column.min = column.min.min(value);
                column.max = column.max.max(value);
            } else {
                let join = previous.and_then(|(last, last_cadence, last_index)| {
                    let tolerance = last_cadence.max(cadence).min(5_000) * 2;
                    (cadence > 0 && last_index < index && time - last <= tolerance)
                        .then_some(last_index)
                });
                *column = Some(Column {
                    first: value,
                    last: value,
                    min: value,
                    max: value,
                    join,
                });
            }
            previous = Some((time, cadence, index));
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "windows-monitor-history-{}-{nonce}",
                std::process::id()
            )))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn history_reopens_filters_at_the_exact_cutoff_and_prunes_old_files() {
        let dir = TestDir::new();
        let now = RETENTION + 2 * HOUR + 15;
        let cutoff = now - RETENTION;
        let range = Range::recent(0, now + HOUR, now).unwrap();
        assert_eq!(
            range,
            Range {
                start: cutoff,
                end: now
            }
        );
        assert!(Range::recent(now + 1, now + HOUR, now).is_none());
        assert!(Range::recent(0, cutoff - 1, now).is_none());
        let mut writer = Writer::new(dir.0.clone());
        for time in [HOUR, cutoff - 1, cutoff, now - 10] {
            writer.record(time, 1_000, Sample::default(), 17).unwrap();
        }
        drop(writer);
        let last_path = hour_path(&dir.0, now / HOUR);
        OpenOptions::new()
            .append(true)
            .open(&last_path)
            .unwrap()
            .write_all(&[1, 2, 3])
            .unwrap();
        assert_eq!(query(&dir.0, range, Metric::Traffic).unwrap().count, 2);
        Writer::new(dir.0.clone())
            .record(now, 1_000, Sample::default(), 20)
            .unwrap();
        prune(&dir.0, now).unwrap();
        assert!(!hour_path(&dir.0, 1).exists());
        let data = query(&dir.0, range, Metric::Traffic).unwrap();
        assert_eq!(data.range, range);
        assert_eq!(data.count, 3);
        assert_eq!((data.min, data.average(), data.max), (17.0, 18.0, 20.0));
    }

    #[test]
    fn chart_keeps_extrema_and_breaks_at_missing_samples_or_restarts() {
        let dir = TestDir::new();
        let mut writer = Writer::new(dir.0.clone());
        for (time, cadence, cpu) in [
            (0, 0, Some(0.5)),
            (1_000, 1_000, Some(99.75)),
            (2_000, 1_000, Some(10.0)),
            (3_000, 1_000, None),
            (4_000, 1_000, Some(40.0)),
            (5_000, 0, Some(50.0)),
            (6_000, 1_000, Some(60.0)),
            (20_000, 1_000, Some(20.0)),
        ] {
            writer
                .record(
                    time,
                    cadence,
                    Sample {
                        cpu,
                        ..Default::default()
                    },
                    0,
                )
                .unwrap();
        }
        let second = query(
            &dir.0,
            Range {
                start: 0,
                end: 20_000,
            },
            Metric::Cpu,
        )
        .unwrap();
        let columns: Vec<_> = second.columns.iter().flatten().collect();
        assert_eq!(second.count, 7);
        assert_eq!(
            columns
                .iter()
                .map(|column| column.join.is_some())
                .collect::<Vec<_>>(),
            [false, true, true, false, false, true, false]
        );
        let week = query(
            &dir.0,
            Range {
                start: 0,
                end: RETENTION,
            },
            Metric::Cpu,
        )
        .unwrap();
        let first = week.columns[0].unwrap();
        assert_eq!(
            (first.first, first.min, first.max, first.last),
            (0.5, 0.5, 99.75, 20.0)
        );
    }
}
