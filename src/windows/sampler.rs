use std::{
    collections::HashMap,
    mem::{size_of, swap},
    ptr::{addr_of, null_mut},
    slice,
    time::{Duration, Instant},
};

use ::windows::{
    Win32::{
        Foundation::{ERROR_SUCCESS, FILETIME},
        Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1},
        NetworkManagement::{
            IpHelper::{
                FreeMibTable, GetIfTable2, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211,
                IF_TYPE_WWANPP, IF_TYPE_WWANPP2, MIB_IF_ROW2, MIB_IF_TABLE2,
            },
            Ndis::IfOperStatusUp,
        },
        System::{
            Performance::{
                PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT, PDH_FMT_COUNTERVALUE,
                PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT_LARGE, PDH_HCOUNTER,
                PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery,
                PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhGetFormattedCounterValue,
                PdhOpenQueryW,
            },
            Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS},
            Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW},
            SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX},
            Threading::{GetActiveProcessorCount, GetSystemTimes},
        },
    },
    core::{PCWSTR, w},
};

use crate::model::{Bytes, CpuTime, GpuMemory, PhysicalMemory, Sample, rate, traffic_delta};

// The SDK formatting flags are not exposed by windows 0.62.
const PDH_FMT_NOSCALE: u32 = 0x0000_1000;
const PDH_FMT_NOCAP100: u32 = 0x0000_8000;
const PDH_DOUBLE_UNCAPPED: PDH_FMT = PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100);
const PDH_BYTES_PER_SECOND: PDH_FMT =
    PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOSCALE | PDH_FMT_NOCAP100);
const PDH_BYTES: PDH_FMT = PDH_FMT(PDH_FMT_LARGE.0 | PDH_FMT_NOSCALE | PDH_FMT_NOCAP100);

pub struct Sampler {
    previous_network: HashMap<u64, Bytes>,
    network: HashMap<u64, Bytes>,
    network_time: Option<(Instant, Duration)>,
    traffic: u64,
    previous_cpu: Option<CpuTime>,
    performance: Option<PerformanceQuery>,
    many_processors: bool,
    retry_at: Instant,
}

impl Sampler {
    pub fn new() -> Self {
        let many_processors = unsafe { GetActiveProcessorCount(u16::MAX) } > 64;
        Self {
            previous_network: HashMap::new(),
            network: HashMap::new(),
            network_time: None,
            traffic: 0,
            previous_cpu: None,
            performance: PerformanceQuery::new(many_processors),
            many_processors,
            retry_at: Instant::now() + super::SAVE_INTERVAL,
        }
    }

    pub fn poll(&mut self, interval: Duration) -> Sample {
        let now = Instant::now();
        let mut sample = Sample::default();
        if read_network(&mut self.network) {
            if let Some((previous_time, previous_interval)) = self.network_time {
                let delta = traffic_delta(&self.previous_network, &self.network);
                self.traffic = self
                    .traffic
                    .saturating_add(delta.sent.saturating_add(delta.received));
                let elapsed = now - previous_time;
                let cadence = interval.max(previous_interval);
                sample.upload = rate(delta.sent, elapsed, cadence);
                sample.download = rate(delta.received, elapsed, cadence);
            }
            self.network_time = Some((now, interval));
            swap(&mut self.previous_network, &mut self.network);
        }

        sample.traffic = self.traffic;
        sample.memory = memory_status();
        sample.battery = battery_percent();
        sample.uptime = Some(Duration::from_millis(unsafe { GetTickCount64() }));
        if now >= self.retry_at {
            if let Some(query) = &mut self.performance {
                query.add_missing_counters(self.many_processors);
            } else {
                self.performance = PerformanceQuery::new(self.many_processors);
            }
            self.retry_at = now + super::SAVE_INTERVAL;
        }
        if let Some(query) = &mut self.performance
            && query.collect()
        {
            sample.disk = query.disk_activity();
            sample.disk_read = query.disk_read.and_then(disk_rate);
            sample.disk_write = query.disk_write.and_then(disk_rate);
            sample.gpu_memory = query.gpu_memory();
            sample.cpu_mhz = query.cpu_frequency();
            if let Some(cpu) = query.cpu {
                sample.cpu = counter_value(cpu, PDH_FMT_DOUBLE);
            }
        }
        if !self.many_processors
            && let Some(current) = cpu_times()
        {
            sample.cpu = self
                .previous_cpu
                .and_then(|previous| current.utilization(previous));
            self.previous_cpu = Some(current);
        }
        sample
    }
}

struct InterfaceTable(*mut MIB_IF_TABLE2);

impl Drop for InterfaceTable {
    fn drop(&mut self) {
        unsafe { FreeMibTable(self.0.cast()) };
    }
}

fn read_network(current: &mut HashMap<u64, Bytes>) -> bool {
    let mut ptr = null_mut();
    if unsafe { GetIfTable2(&mut ptr) } != ERROR_SUCCESS || ptr.is_null() {
        return false;
    }
    let table = InterfaceTable(ptr);
    // The SDK defines the trailing array with one entry. NumEntries is the real length.
    let rows: &[MIB_IF_ROW2] = unsafe {
        slice::from_raw_parts(
            addr_of!((*table.0).Table).cast(),
            (*table.0).NumEntries as usize,
        )
    };
    current.clear();
    for row in rows {
        // Bit zero of InterfaceAndOperStatusFlags is HardwareInterface.
        // 只统计物理网卡（有线、Wi-Fi、移动宽带），VPN 等虚拟接口会重复计算同一份流量。
        if row.OperStatus == IfOperStatusUp
            && row.InterfaceAndOperStatusFlags._bitfield & 1 != 0
            && matches!(
                row.Type,
                IF_TYPE_ETHERNET_CSMACD | IF_TYPE_IEEE80211 | IF_TYPE_WWANPP | IF_TYPE_WWANPP2
            )
        {
            current.insert(
                unsafe { row.InterfaceLuid.Value },
                Bytes {
                    received: row.InOctets,
                    sent: row.OutOctets,
                },
            );
        }
    }
    true
}

fn cpu_times() -> Option<CpuTime> {
    fn ticks(time: FILETIME) -> u64 {
        (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
    }

    let (mut idle, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    Some(CpuTime {
        idle: ticks(idle),
        kernel: ticks(kernel),
        user: ticks(user),
    })
}

fn memory_status() -> Option<PhysicalMemory> {
    let mut memory = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut memory) }.ok()?;
    Some(PhysicalMemory {
        total: memory.ullTotalPhys,
        available: memory.ullAvailPhys,
    })
}

fn battery_percent() -> Option<f64> {
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
    (status.BatteryFlag & 128 == 0 && status.BatteryLifePercent <= 100)
        .then(|| f64::from(status.BatteryLifePercent))
}

struct PerformanceQuery {
    handle: PDH_HQUERY,
    disk: Option<PDH_HCOUNTER>,
    disk_read: Option<PDH_HCOUNTER>,
    disk_write: Option<PDH_HCOUNTER>,
    gpu_dedicated_usage: Option<PDH_HCOUNTER>,
    gpu_adapters: Option<GpuAdapters>,
    cpu: Option<PDH_HCOUNTER>,
    frequency: Option<PDH_HCOUNTER>,
    processor_performance: Option<PDH_HCOUNTER>,
    nominal_mhz: Option<f64>,
    buffer: Vec<PDH_FMT_COUNTERVALUE_ITEM_W>,
}

impl PerformanceQuery {
    fn new(many_processors: bool) -> Option<Self> {
        let mut handle = PDH_HQUERY::default();
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut handle) } != 0 {
            return None;
        }
        let mut query = Self {
            handle,
            disk: None,
            disk_read: None,
            disk_write: None,
            gpu_dedicated_usage: None,
            gpu_adapters: None,
            cpu: None,
            frequency: None,
            processor_performance: None,
            nominal_mhz: nominal_cpu_mhz(),
            buffer: Vec::new(),
        };
        query.add_missing_counters(many_processors);
        Some(query)
    }

    fn add_missing_counters(&mut self, many_processors: bool) {
        if self.disk.is_none() {
            self.disk = add_counter(self.handle, w!(r"\PhysicalDisk(*)\% Idle Time"));
        }
        if self.disk_read.is_none() {
            self.disk_read = add_counter(
                self.handle,
                w!(r"\PhysicalDisk(_Total)\Disk Read Bytes/sec"),
            );
        }
        if self.disk_write.is_none() {
            self.disk_write = add_counter(
                self.handle,
                w!(r"\PhysicalDisk(_Total)\Disk Write Bytes/sec"),
            );
        }
        if self.gpu_dedicated_usage.is_none() {
            self.gpu_dedicated_usage =
                add_counter(self.handle, w!(r"\GPU Adapter Memory(*)\Dedicated Usage"));
        }
        if self.gpu_dedicated_usage.is_some()
            && self
                .gpu_adapters
                .as_ref()
                .is_none_or(|adapters| !unsafe { adapters.factory.IsCurrent() }.as_bool())
        {
            self.gpu_adapters = GpuAdapters::new();
        }
        if many_processors && self.cpu.is_none() {
            self.cpu = add_counter(
                self.handle,
                w!(r"\Processor Information(_Total)\% Processor Time"),
            );
        }
        if self.frequency.is_none() {
            self.frequency = add_counter(
                self.handle,
                w!(r"\Processor Information(_Total)\Processor Frequency"),
            );
        }
        if self.nominal_mhz.is_some() && self.processor_performance.is_none() {
            self.processor_performance = add_counter(
                self.handle,
                w!(r"\Processor Information(_Total)\% Processor Performance"),
            );
        }
    }

    fn cpu_frequency(&self) -> Option<f64> {
        if let (Some(nominal), Some(performance)) = (self.nominal_mhz, self.processor_performance)
            && let Some(percent) = counter_value(performance, PDH_DOUBLE_UNCAPPED)
            && percent > 0.0
        {
            // The performance percentage reflects boost on systems that report a fixed MHz counter.
            return Some(nominal * percent / 100.0);
        }
        counter_value(self.frequency?, PDH_FMT_DOUBLE)
    }

    fn collect(&self) -> bool {
        unsafe { PdhCollectQueryData(self.handle) == 0 }
    }

    fn disk_activity(&mut self) -> Option<f64> {
        let total_name = w!("_Total");
        counter_array(self.disk?, PDH_FMT_DOUBLE, &mut self.buffer)?
            .iter()
            .filter_map(|item| {
                if item.szName.is_null()
                    || unsafe { item.szName.as_wide() } == unsafe { total_name.as_wide() }
                {
                    return None;
                }
                valid_value(item.FmtValue).map(|idle| (100.0 - idle).clamp(0.0, 100.0))
            })
            .reduce(f64::max)
    }

    fn gpu_memory(&mut self) -> Option<GpuMemory> {
        let adapters = self.gpu_adapters.as_mut()?;
        if adapters.memory.is_empty() {
            return None;
        }
        let items = counter_array(self.gpu_dedicated_usage?, PDH_BYTES, &mut self.buffer)?;
        adapters.sample(items)
    }
}

impl Drop for PerformanceQuery {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.handle) };
    }
}

struct GpuAdapters {
    factory: IDXGIFactory1,
    memory: Vec<AdapterMemory>,
}

struct AdapterMemory {
    luid: (u32, u32),
    total: u64,
    used: Option<u64>,
}

impl GpuAdapters {
    fn new() -> Option<Self> {
        let factory = unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }.ok()?;
        let mut memory = Vec::new();
        for index in 0.. {
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                break;
            };
            let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
                continue;
            };
            let total = desc.DedicatedVideoMemory as u64 + desc.DedicatedSystemMemory as u64;
            if total > 0 && desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 {
                memory.push(AdapterMemory {
                    luid: (desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart),
                    total,
                    used: None,
                });
            }
        }
        Some(Self { factory, memory })
    }

    fn sample(&mut self, items: &[PDH_FMT_COUNTERVALUE_ITEM_W]) -> Option<GpuMemory> {
        for adapter in &mut self.memory {
            adapter.used = None;
        }
        for item in items {
            if item.szName.is_null() || !valid_status(item.FmtValue.CStatus) {
                continue;
            }
            let Ok(used) = u64::try_from(unsafe { item.FmtValue.Anonymous.largeValue }) else {
                continue;
            };
            if let Ok(name) = unsafe { item.szName.to_string() }
                && let Some(luid) = adapter_luid(&name)
                && let Some(adapter) = self.memory.iter_mut().find(|adapter| adapter.luid == luid)
            {
                // Linked physical GPUs share an adapter LUID. Include each node's usage.
                adapter.used = Some(adapter.used.unwrap_or(0).saturating_add(used));
            }
        }
        let mut total = 0;
        let mut used = 0;
        for adapter in &self.memory {
            if let Some(adapter_used) = adapter.used {
                total += adapter.total;
                used += adapter_used;
            }
        }
        (total > 0).then_some(GpuMemory { used, total })
    }
}

fn adapter_luid(name: &str) -> Option<(u32, u32)> {
    // PDH instances are named luid_0x<high>_0x<low>_phys_<node>.
    let (luid, _) = name.strip_prefix("luid_")?.split_once("_phys_")?;
    let (high, low) = luid.split_once('_')?;
    Some((
        u32::from_str_radix(high.strip_prefix("0x")?, 16).ok()?,
        u32::from_str_radix(low.strip_prefix("0x")?, 16).ok()?,
    ))
}

fn counter_array(
    counter: PDH_HCOUNTER,
    format: PDH_FMT,
    buffer: &mut Vec<PDH_FMT_COUNTERVALUE_ITEM_W>,
) -> Option<&[PDH_FMT_COUNTERVALUE_ITEM_W]> {
    let item_size = size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    let mut bytes = (buffer.len() * item_size) as u32;
    let mut count = 0;
    let data = (!buffer.is_empty()).then_some(buffer.as_mut_ptr());
    let mut status =
        unsafe { PdhGetFormattedCounterArrayW(counter, format, &mut bytes, &mut count, data) };
    if status == PDH_MORE_DATA && bytes > 0 {
        buffer.resize(
            (bytes as usize).div_ceil(item_size),
            PDH_FMT_COUNTERVALUE_ITEM_W::default(),
        );
        status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                format,
                &mut bytes,
                &mut count,
                Some(buffer.as_mut_ptr()),
            )
        };
    }
    if status != 0 {
        return None;
    }
    buffer.get(..count as usize)
}

fn disk_rate(counter: PDH_HCOUNTER) -> Option<f64> {
    counter_value(counter, PDH_BYTES_PER_SECOND).filter(|value| *value >= 0.0)
}

fn add_counter(query: PDH_HQUERY, path: PCWSTR) -> Option<PDH_HCOUNTER> {
    let mut counter = PDH_HCOUNTER::default();
    (unsafe { PdhAddEnglishCounterW(query, path, 0, &mut counter) } == 0).then_some(counter)
}

fn nominal_cpu_mhz() -> Option<f64> {
    let mut mhz = 0u32;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"),
            w!("~MHz"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut mhz as *mut u32).cast()),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS && mhz > 0).then(|| f64::from(mhz))
}

fn counter_value(counter: PDH_HCOUNTER, format: PDH_FMT) -> Option<f64> {
    let mut value = PDH_FMT_COUNTERVALUE::default();
    if unsafe { PdhGetFormattedCounterValue(counter, format, None, &mut value) } != 0 {
        return None;
    }
    valid_value(value)
}

fn valid_status(status: u32) -> bool {
    matches!(status, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA)
}

fn valid_value(value: PDH_FMT_COUNTERVALUE) -> Option<f64> {
    if !valid_status(value.CStatus) {
        return None;
    }
    let formatted = unsafe { value.Anonymous.doubleValue };
    formatted.is_finite().then_some(formatted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a Windows desktop with physical drive performance counters"]
    fn samples_real_windows_counters() {
        let mut sampler = Sampler::new();
        sampler.poll(Duration::from_secs(1));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let sample = sampler.poll(Duration::from_secs(1));
        eprintln!(
            "{sample:?}, {} active physical adapters",
            sampler.previous_network.len()
        );
        for value in [sample.cpu, sample.disk] {
            assert!(value.is_some_and(|value| (0.0..=100.0).contains(&value)));
        }
        assert!(sample.upload.is_some_and(|value| value >= 0.0));
        assert!(sample.download.is_some_and(|value| value >= 0.0));
        assert!(sample.disk_read.is_some_and(|value| value >= 0.0));
        assert!(sample.disk_write.is_some_and(|value| value >= 0.0));
        assert!(
            sample
                .memory
                .is_some_and(|memory| memory.available <= memory.total && memory.total > 0)
        );
        assert!(sample.cpu_mhz.is_none_or(|mhz| mhz >= 0.0));
        assert!(
            sample
                .uptime
                .is_some_and(|uptime| uptime >= Duration::from_secs(1))
        );
    }
}
