use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::model::{Metric, Sample};

pub const RETENTION: u64 = 7 * 24 * 60 * 60 * 1_000;
const HOUR: u64 = 60 * 60 * 1_000;
// 落盘记录的固定布局，独立于 `Metric::ALL`：新增指标不会改变旧文件的记录长度。
// 只能在末尾追加并同时更换带版本号的历史目录（如 history-v2），不能调整已有顺序。
const STORED_METRICS: [Metric; 16] = [
    Metric::Upload,
    Metric::Download,
    Metric::Cpu,
    Metric::Memory,
    Metric::Disk,
    Metric::Traffic,
    Metric::AvailableMemory,
    Metric::CpuFrequency,
    Metric::Uptime,
    Metric::Battery,
    Metric::AvailableMemoryPercent,
    Metric::UsedMemory,
    Metric::DiskRead,
    Metric::DiskWrite,
    Metric::GpuMemoryPercent,
    Metric::UsedGpuMemory,
];
const RECORD_SIZE: usize = (2 + STORED_METRICS.len()) * 8;
// 记录先写入内存缓冲，最多每隔这么久落盘一次；进程异常退出时最多丢失这段时间的数据。
const FLUSH_INTERVAL: u64 = 10_000;
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
    current: Option<Segment>,
}

struct Segment {
    hour: u64,
    output: BufWriter<File>,
    flushed_at: u64,
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
            .is_none_or(|segment| segment.hour != hour)
        {
            // 切换到新的小时文件前先把旧文件的缓冲写完。
            if let Some(mut previous) = self.current.take() {
                previous.output.flush()?;
            }
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
            self.current = Some(Segment {
                hour,
                output: BufWriter::new(file),
                flushed_at: time,
            });
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
        let segment = self.current.as_mut().unwrap();
        let mut result = segment.output.write_all(&record);
        if result.is_ok() && time.saturating_sub(segment.flushed_at) >= FLUSH_INTERVAL {
            result = segment.output.flush();
            segment.flushed_at = time;
        }
        if result.is_err() {
            self.current = None;
        }
        result
    }
}

fn values(sample: Sample, traffic: u64) -> [Option<f64>; STORED_METRICS.len()] {
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

/// 导出文件格式，由目标文件扩展名决定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// 列式 JSON：列名只出现一次，每条记录是一个数组，体积约为逐条带键名的三分之一。
    Json,
    /// 逗号分隔值，缺失读数留空，便于直接用表格软件打开。
    Csv,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        if extension.eq_ignore_ascii_case("json") {
            Some(Self::Json)
        } else if extension.eq_ignore_ascii_case("csv") {
            Some(Self::Csv)
        } else {
            None
        }
    }
}

pub fn export(directory: &Path, destination: &Path, now: u64) -> io::Result<u64> {
    let format = Format::from_path(destination)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unsupported export format"))?;
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
        let count = match format {
            Format::Json => write_json(directory, now, &mut output)?,
            Format::Csv => write_csv(directory, now, &mut output)?,
        };
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

type StoredValues = [Option<f64>; STORED_METRICS.len()];

/// 按时间顺序遍历保留期内的完整记录，返回记录条数。
fn for_each_record(
    directory: &Path,
    now: u64,
    mut visit: impl FnMut(u64, u64, &StoredValues) -> io::Result<()>,
) -> io::Result<u64> {
    let start = now.saturating_sub(RETENTION);
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
            let values = std::array::from_fn(|index| {
                let offset = 16 + 8 * index;
                let value = f64::from_le_bytes(record[offset..offset + 8].try_into().unwrap());
                (value.is_finite() && value >= 0.0).then_some(value)
            });
            visit(timestamp_ms, interval_ms, &values)?;
            count += 1;
        }
    }
    Ok(count)
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

const TIME_COLUMNS: [&str; 2] = ["timestamp_ms", "interval_ms"];

fn columns() -> impl Iterator<Item = &'static str> {
    TIME_COLUMNS
        .into_iter()
        .chain(STORED_METRICS.into_iter().map(Metric::key))
}

fn write_json(directory: &Path, now: u64, output: &mut impl Write) -> io::Result<u64> {
    let units: BTreeMap<_, _> = STORED_METRICS
        .into_iter()
        .map(|metric| (metric.key(), unit(metric)))
        .collect();
    output.write_all(br#"{"schema_version":2,"columns":"#)?;
    serde_json::to_writer(&mut *output, &columns().collect::<Vec<_>>())?;
    output.write_all(br#","units":"#)?;
    serde_json::to_writer(&mut *output, &units)?;
    output.write_all(
        b",\"samples\":[
",
    )?;
    let mut first = true;
    let count = for_each_record(directory, now, |timestamp_ms, interval_ms, values| {
        if !std::mem::take(&mut first) {
            output.write_all(
                b",
",
            )?;
        }
        // 每条记录是与 columns 一一对应的扁平数组。
        write!(output, "[{timestamp_ms},{interval_ms}")?;
        for value in values {
            output.write_all(b",")?;
            serde_json::to_writer(&mut *output, value)?;
        }
        output.write_all(b"]")
    })?;
    output.write_all(
        b"
]}
",
    )?;
    Ok(count)
}

fn write_csv(directory: &Path, now: u64, output: &mut impl Write) -> io::Result<u64> {
    // 列名全部是 ASCII 标识符，读数是数字，无需引号转义。
    writeln!(output, "{}", columns().collect::<Vec<_>>().join(","))?;
    for_each_record(directory, now, |timestamp_ms, interval_ms, values| {
        write!(output, "{timestamp_ms},{interval_ms}")?;
        for value in values {
            match value {
                Some(value) => write!(output, ",{value}")?,
                None => output.write_all(b",")?,
            }
        }
        output.write_all(
            b"
",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            // 并行测试可能在同一时钟刻度内创建目录，序号保证名字唯一。
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "windows-monitor-history-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }

    #[test]
    fn stored_layout_lists_each_metric_once() {
        for (index, metric) in STORED_METRICS.iter().enumerate() {
            assert!(!STORED_METRICS[..index].contains(metric), "{metric:?}");
        }
        assert_eq!(
            RECORD_SIZE, 144,
            "the history-v1 record layout must not change"
        );
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
        assert_eq!(export(&dir.0, &destination, now).unwrap(), 2);

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
        assert_eq!(export(&dir.0, &destination, now).unwrap(), 3);
        let data: serde_json::Value =
            serde_json::from_reader(File::open(&destination).unwrap()).unwrap();
        assert_eq!(data["schema_version"], 2);
        assert_eq!(data["units"]["cpu"], "percent");
        assert_eq!(data["units"]["traffic"], "bytes");
        let columns: Vec<_> = data["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|column| column.as_str().unwrap())
            .collect();
        assert_eq!(columns[..3], ["timestamp_ms", "interval_ms", "upload"]);
        let column = |name| columns.iter().position(|&column| column == name).unwrap();
        let samples = data["samples"].as_array().unwrap();
        assert_eq!(samples.len(), 3);
        assert!(
            samples
                .iter()
                .all(|row| row.as_array().unwrap().len() == columns.len())
        );
        assert_eq!(samples[0][0], cutoff);
        assert_eq!(samples[0][1], 1_000);
        assert!(samples[0][column("cpu")].is_null());
        assert_eq!(samples[2][0], now);
        assert_eq!(samples[2][1], 0);
        assert_eq!(samples[2][column("traffic")], 20.0);
        assert_eq!(samples[2][column("cpu")], 42.5);

        let csv = dir.0.join("export.CSV");
        assert_eq!(export(&dir.0, &csv, now).unwrap(), 3);
        let text = fs::read_to_string(&csv).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], columns.join(","));
        let last: Vec<_> = lines[3].split(',').collect();
        assert_eq!(last.len(), columns.len());
        assert_eq!(last[0], now.to_string());
        assert_eq!(last[column("cpu")], "42.5");
        assert_eq!(last[column("traffic")], "20");
        assert_eq!(last[column("upload")], "");
        assert!(export(&dir.0, &dir.0.join("export.txt"), now).is_err());
    }

    #[test]
    fn buffers_records_until_the_flush_interval_or_drop() {
        let dir = TestDir::new();
        let start = 5 * HOUR;
        let path = hour_path(&dir.0, start / HOUR);
        let len = || fs::metadata(&path).unwrap().len();
        let mut writer = Writer::new(dir.0.clone());
        writer.record(start, 0, Sample::default(), 0).unwrap();
        writer
            .record(start + 1_000, 1_000, Sample::default(), 0)
            .unwrap();
        assert_eq!(len(), 0);
        writer
            .record(start + FLUSH_INTERVAL, 1_000, Sample::default(), 0)
            .unwrap();
        assert_eq!(len(), 3 * RECORD_SIZE as u64);
        writer
            .record(start + FLUSH_INTERVAL + 1_000, 1_000, Sample::default(), 0)
            .unwrap();
        drop(writer);
        assert_eq!(len(), 4 * RECORD_SIZE as u64);
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
