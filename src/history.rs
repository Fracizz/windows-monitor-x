use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Serialize, Serializer, ser::SerializeMap};

use crate::model::{Metric, Sample};

pub const RETENTION: u64 = 7 * 24 * 60 * 60 * 1_000;
const HOUR: u64 = 60 * 60 * 1_000;
// Change the versioned history directory if the order or layout of stored values changes.
const STORED_METRICS: [Metric; 16] = Metric::ALL;
const RECORD_SIZE: usize = (2 + STORED_METRICS.len()) * 8;
static NEXT_EXPORT: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
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

pub fn export_json(directory: &Path, destination: &Path, now: u64) -> io::Result<u64> {
    let export = NEXT_EXPORT.fetch_add(1, Ordering::Relaxed);
    let temporary = destination.with_file_name(format!(
        ".windows-monitor-export-{}-{now}-{export}.tmp",
        std::process::id()
    ));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        let mut output = BufWriter::new(file);
        let count = write_json(directory, now, &mut output)?;
        output.flush()?;
        drop(output);
        fs::rename(&temporary, destination)?;
        Ok(count)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[derive(Serialize)]
struct ExportSample {
    timestamp_ms: u64,
    interval_ms: u64,
    #[serde(serialize_with = "serialize_metrics")]
    metrics: [Option<f64>; STORED_METRICS.len()],
}

fn serialize_metrics<S: Serializer>(
    values: &[Option<f64>; STORED_METRICS.len()],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut map = serializer.serialize_map(Some(values.len()))?;
    for (metric, value) in STORED_METRICS.into_iter().zip(values) {
        map.serialize_entry(metric.key(), value)?;
    }
    map.end()
}

fn unit(metric: Metric) -> &'static str {
    match metric {
        Metric::Upload | Metric::Download | Metric::DiskRead | Metric::DiskWrite => {
            "bytes_per_second"
        }
        Metric::Traffic | Metric::AvailableMemory | Metric::UsedMemory | Metric::UsedGpuMemory => {
            "bytes"
        }
        Metric::CpuFrequency => "megahertz",
        Metric::Uptime => "seconds",
        Metric::Cpu
        | Metric::Memory
        | Metric::Disk
        | Metric::Battery
        | Metric::AvailableMemoryPercent
        | Metric::GpuMemoryPercent => "percent",
    }
}

fn write_json(directory: &Path, now: u64, output: &mut impl Write) -> io::Result<u64> {
    let start = now.saturating_sub(RETENTION);
    let units: BTreeMap<_, _> = STORED_METRICS
        .into_iter()
        .map(|metric| (metric.key(), unit(metric)))
        .collect();
    output.write_all(br#"{"schema_version":1,"units":"#)?;
    serde_json::to_writer(&mut *output, &units)?;
    output.write_all(b",\"samples\":[\n")?;

    let mut count = 0;
    for hour in (start / HOUR)..=(now / HOUR) {
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
            let timestamp_ms = u64::from_le_bytes(record[..8].try_into().unwrap());
            if timestamp_ms < start || timestamp_ms > now {
                continue;
            }
            let interval_ms = u64::from_le_bytes(record[8..16].try_into().unwrap());
            let metrics = std::array::from_fn(|index| {
                let offset = 16 + 8 * index;
                let value = f64::from_le_bytes(record[offset..offset + 8].try_into().unwrap());
                (value.is_finite() && value >= 0.0).then_some(value)
            });
            if count != 0 {
                output.write_all(b",\n")?;
            }
            serde_json::to_writer(
                &mut *output,
                &ExportSample {
                    timestamp_ms,
                    interval_ms,
                    metrics,
                },
            )?;
            count += 1;
        }
    }
    output.write_all(b"\n]}\n")?;
    Ok(count)
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
    fn exports_existing_records_with_the_exact_retention_cutoff() {
        let dir = TestDir::new();
        let now = RETENTION + 2 * HOUR + 15;
        let cutoff = now - RETENTION;
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
        let destination = dir.0.join("export.json");
        assert_eq!(export_json(&dir.0, &destination, now).unwrap(), 2);

        let sample = Sample {
            cpu: Some(42.5),
            ..Default::default()
        };
        let mut writer = Writer::new(dir.0.clone());
        writer.record(now, 0, sample, 20).unwrap();
        writer.record(now + 1, 1_000, sample, 22).unwrap();
        drop(writer);
        prune(&dir.0, now).unwrap();
        assert!(!hour_path(&dir.0, 1).exists());
        assert_eq!(export_json(&dir.0, &destination, now).unwrap(), 3);
        let data: serde_json::Value =
            serde_json::from_reader(File::open(destination).unwrap()).unwrap();
        assert_eq!(data["schema_version"], 1);
        assert_eq!(data["units"]["cpu"], "percent");
        assert_eq!(data["units"]["traffic"], "bytes");
        let samples = data["samples"].as_array().unwrap();
        assert_eq!(samples[0]["timestamp_ms"], cutoff);
        assert_eq!(samples[0]["interval_ms"], 1_000);
        assert!(samples[0]["metrics"]["cpu"].is_null());
        assert_eq!(samples[2]["timestamp_ms"], now);
        assert_eq!(samples[2]["interval_ms"], 0);
        assert_eq!(samples[2]["metrics"]["traffic"], 20.0);
        assert_eq!(samples[2]["metrics"]["cpu"], 42.5);
        assert_eq!(samples.len(), 3);
    }

    #[test]
    fn empty_history_exports_an_empty_samples_array() {
        let dir = TestDir::new();
        let mut output = Vec::new();
        assert_eq!(write_json(&dir.0, 5_000, &mut output).unwrap(), 0);
        let data: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(data["samples"], serde_json::json!([]));
    }
}
