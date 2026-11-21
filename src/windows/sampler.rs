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
        NetworkManagement::{
            IpHelper::{
                FreeMibTable, GetIfTable2, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211, MIB_IF_ROW2,
                MIB_IF_TABLE2,
            },
            Ndis::IfOperStatusUp,
        },
        System::{
            Performance::{
                PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT, PDH_FMT_COUNTERVALUE,
                PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
                PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
                PdhGetFormattedCounterArrayW, PdhGetFormattedCounterValue, PdhOpenQueryW,
            },
            Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS},
            Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW},
            SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX},
            Threading::{GetActiveProcessorCount, GetSystemTimes},
        },
    },
    core::{PCWSTR, w},
};

use crate::model::{Bytes, CpuTime, Sample, rate, traffic_delta};

// The SDK flag is not exposed by windows 0.62.
const PDH_DOUBLE_UNCAPPED: PDH_FMT = PDH_FMT(PDH_FMT_DOUBLE.0 | 0x0000_8000);

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
        if let Some(memory) = memory_status() {
            sample.memory = Some(f64::from(memory.dwMemoryLoad));
            sample.available_memory = Some(memory.ullAvailPhys);
        }
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
        if row.OperStatus == IfOperStatusUp
            && row.InterfaceAndOperStatusFlags._bitfield & 1 != 0
            && matches!(row.Type, IF_TYPE_ETHERNET_CSMACD | IF_TYPE_IEEE80211)
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

fn memory_status() -> Option<MEMORYSTATUSEX> {
    let mut memory = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut memory) }.ok()?;
    Some(memory)
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
        let counter = self.disk?;
        let mut bytes = (self.buffer.len() * size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>()) as u32;
        let mut count = 0;
        let buffer = (!self.buffer.is_empty()).then_some(self.buffer.as_mut_ptr());
        let mut status = unsafe {
            PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, buffer)
        };
        if status == PDH_MORE_DATA && bytes > 0 {
            let entries = (bytes as usize).div_ceil(size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>());
            self.buffer
                .resize(entries, PDH_FMT_COUNTERVALUE_ITEM_W::default());
            status = unsafe {
                PdhGetFormattedCounterArrayW(
                    counter,
                    PDH_FMT_DOUBLE,
                    &mut bytes,
                    &mut count,
                    Some(self.buffer.as_mut_ptr()),
                )
            };
        }
        if status != 0 {
            return None;
        }

        let total_name = w!("_Total");
        self.buffer
            .get(..count as usize)?
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
}

impl Drop for PerformanceQuery {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.handle) };
    }
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

fn valid_value(value: PDH_FMT_COUNTERVALUE) -> Option<f64> {
    if !matches!(value.CStatus, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA) {
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
        for value in [sample.cpu, sample.memory, sample.disk] {
            assert!(value.is_some_and(|value| (0.0..=100.0).contains(&value)));
        }
        assert!(sample.upload.is_some_and(|value| value >= 0.0));
        assert!(sample.download.is_some_and(|value| value >= 0.0));
        assert!(sample.available_memory.is_some());
        assert!(sample.cpu_mhz.is_none_or(|mhz| mhz >= 0.0));
        assert!(
            sample
                .uptime
                .is_some_and(|uptime| uptime >= Duration::from_secs(1))
        );
    }
}
