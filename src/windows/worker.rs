//! 后台采样线程：定时采样、记录历史、探测任务栏空位，并通过邮箱把结果交给 UI 线程。

use super::*;

#[derive(Default)]
pub(super) struct Mailbox {
    pub(super) sample: Option<Sample>,
    pub(super) dock: Option<(u64, Placement)>,
    pub(super) history_error: Option<String>,
    pub(super) history_export: Option<io::Result<u64>>,
}

pub(super) type Feed = Arc<Mutex<Mailbox>>;

pub(super) enum WorkerCommand {
    Interval(Refresh),
    RecordHistory(bool),
    ResetHistoryTraffic(u64),
    Dock(Option<u64>, TaskbarSide, LayoutSize),
    RecheckDock,
    TaskbarInteraction,
    Stop,
}

pub(super) struct SamplerWorker {
    control: mpsc::Sender<WorkerCommand>,
    thread: Option<JoinHandle<()>>,
}

impl SamplerWorker {
    pub(super) fn start(
        hwnd: HWND,
        feed: Feed,
        settings: Settings,
        (control, commands): (mpsc::Sender<WorkerCommand>, mpsc::Receiver<WorkerCommand>),
    ) -> std::io::Result<Self> {
        let handle = hwnd.0 as usize;
        let thread = thread::Builder::new()
            .name("monitor-sampler".into())
            .spawn(move || {
                let hwnd = HWND(handle as *mut _);
                let mut sampler = Sampler::new();
                let mut last_sample = Instant::now();
                let mut next_sample = last_sample;
                let mut interval = settings.refresh.interval();
                let mut capture = HistoryCapture::new(settings);
                let mut dock = settings.taskbar.then_some((0, next_sample));
                let mut taskbar_side = settings.taskbar_side;
                let mut dock_size = Layout::Taskbar.size(settings.display);
                let mut taskbar = None;
                let mut dock_placement = DockPlacement::default();
                let mut last_placement = None;
                loop {
                    let now = Instant::now();
                    if now >= next_sample {
                        last_sample = now;
                        next_sample = now + interval;
                        let sample = sampler.poll(interval);
                        let pending = feed.lock().unwrap().sample.replace(sample).is_some();
                        if !pending && !notify(hwnd, SAMPLE_READY) {
                            break;
                        }
                        capture.poll(hwnd, &feed, now, interval, sample);
                    }
                    if let Some((generation, deadline)) = dock
                        && now >= deadline
                    {
                        let bar = primary_taskbar();
                        let observed = if bar.is_some_and(taskbar_menu_open) {
                            Some(Observation::Interaction)
                        } else {
                            if taskbar.is_none() {
                                taskbar = Taskbar::new().ok();
                            }
                            bar.and_then(|bar| {
                                taskbar.as_mut()?.observe(
                                    bar,
                                    hwnd,
                                    taskbar_side,
                                    dock_size,
                                    &mut dock_placement,
                                )
                            })
                        };
                        let placement = match observed {
                            Some(Observation::Interaction | Observation::Overlay) => {
                                dock_placement.hold()
                            }
                            // The XAML menu temporarily replaces the taskbar's automation tree.
                            None if bar.is_some_and(taskbar_has_focus) => dock_placement.hold(),
                            Some(Observation::Placement(placement)) => {
                                dock_placement.resolve(Some(placement), Instant::now())
                            }
                            Some(Observation::NoRoom) | None => {
                                dock_placement.resolve(None, Instant::now())
                            }
                        };
                        if let Some(placement) = placement
                            && (last_placement != Some(placement)
                                || matches!(placement, Placement::Docked(rect)
                                    if bar.is_some_and(|bar| covered_by_taskbar(bar, hwnd, rect))))
                        {
                            last_placement = Some(placement);
                            let pending = feed
                                .lock()
                                .unwrap()
                                .dock
                                .replace((generation, placement))
                                .is_some();
                            if !pending && !notify(hwnd, DOCK_READY) {
                                break;
                            }
                        }
                        dock = Some((generation, Instant::now() + DOCK_INTERVAL));
                    }
                    let wake = dock.map_or(next_sample, |(_, deadline)| deadline.min(next_sample));
                    match commands.recv_timeout(wake.saturating_duration_since(Instant::now())) {
                        Ok(WorkerCommand::Interval(refresh)) => {
                            interval = refresh.interval();
                            next_sample = last_sample + interval;
                        }
                        Ok(WorkerCommand::RecordHistory(enabled)) => {
                            capture.set_enabled(hwnd, &feed, enabled);
                        }
                        Ok(WorkerCommand::ResetHistoryTraffic(traffic)) => {
                            capture.starting_traffic = 0;
                            capture.traffic_at_reset = traffic;
                        }
                        Ok(WorkerCommand::Dock(generation, side, size)) => {
                            taskbar_side = side;
                            dock_size = size;
                            dock = generation.map(|id| (id, Instant::now()));
                            if dock.is_none() {
                                taskbar = None;
                                dock_placement = DockPlacement::default();
                            } else {
                                dock_placement.reset_slot();
                            }
                            last_placement = None;
                        }
                        Ok(WorkerCommand::RecheckDock) => {
                            dock = dock.map(|(id, _)| (id, Instant::now()));
                        }
                        Ok(WorkerCommand::TaskbarInteraction) => {
                            // Keep the current slot while Explorer rearranges its popup controls.
                            dock_placement.hold();
                            dock = dock.map(|(id, _)| (id, Instant::now() + DOCK_INTERVAL));
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
            })?;
        Ok(Self {
            control,
            thread: Some(thread),
        })
    }
}

pub(super) fn history_directory() -> Option<PathBuf> {
    Some(state::data_directory()?.join("history-v1"))
}

struct HistoryCapture {
    directory: Option<PathBuf>,
    writer: Option<history::Writer>,
    enabled: bool,
    first_sample: bool,
    prune_at: Instant,
    retry_at: Instant,
    starting_traffic: u64,
    traffic_at_reset: u64,
}

impl HistoryCapture {
    fn new(settings: Settings) -> Self {
        let now = Instant::now();
        Self {
            directory: history_directory(),
            writer: None,
            enabled: settings.record_history,
            first_sample: true,
            prune_at: now,
            retry_at: now,
            starting_traffic: settings.traffic,
            traffic_at_reset: 0,
        }
    }

    fn set_enabled(&mut self, hwnd: HWND, feed: &Feed, enabled: bool) {
        self.enabled = enabled;
        self.first_sample = true;
        self.retry_at = Instant::now();
        if !enabled {
            self.writer = None;
            Self::report(hwnd, feed, None);
        }
    }

    fn poll(&mut self, hwnd: HWND, feed: &Feed, now: Instant, interval: Duration, sample: Sample) {
        if now >= self.prune_at {
            // Previously recorded history expires even when recording is paused.
            let result = self.directory.as_ref().map_or(Ok(()), |directory| {
                history::prune(directory, history::now_ms())
            });
            self.prune_at = now
                + if result.is_ok() {
                    Duration::from_secs(3600)
                } else {
                    SAVE_INTERVAL
                };
        }
        if !self.enabled || now < self.retry_at {
            return;
        }
        let opening = self.writer.is_none();
        let result = self.record(interval, sample);
        if let Err(error) = result {
            self.writer = None;
            self.first_sample = true;
            self.retry_at = now + SAVE_INTERVAL;
            Self::report(hwnd, feed, Some(error.to_string()));
        } else if opening {
            Self::report(hwnd, feed, None);
        }
    }

    fn record(&mut self, interval: Duration, sample: Sample) -> io::Result<()> {
        let directory = self
            .directory
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA"))?;
        let writer = self
            .writer
            .get_or_insert_with(|| history::Writer::new(directory.clone()));
        let cadence = if self.first_sample {
            0
        } else {
            interval.as_millis() as u64
        };
        let traffic = self
            .starting_traffic
            .saturating_add(sample.traffic.saturating_sub(self.traffic_at_reset));
        writer.record(history::now_ms(), cadence, sample, traffic)?;
        self.first_sample = false;
        Ok(())
    }

    fn report(hwnd: HWND, feed: &Feed, error: Option<String>) {
        let mut mailbox = feed.lock().unwrap();
        if mailbox.history_error != error {
            mailbox.history_error = error;
            drop(mailbox);
            notify(hwnd, HISTORY_STATUS);
        }
    }
}

pub(super) fn notify(hwnd: HWND, message: u32) -> bool {
    unsafe { PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0)) }.is_ok()
}

impl Drop for SamplerWorker {
    fn drop(&mut self) {
        let _ = self.control.send(WorkerCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_interval_reschedules_the_running_sampler() {
        let window = Window(
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("Static"),
                    PCWSTR::null(),
                    WINDOW_STYLE::default(),
                    0,
                    0,
                    0,
                    0,
                    Some(HWND_MESSAGE),
                    None,
                    None,
                    None,
                )
            }
            .unwrap(),
        );
        let settings = Settings {
            refresh: Refresh::Three,
            ..Default::default()
        };
        let feed = Feed::default();
        let command = mpsc::channel();
        let sender = command.0.clone();
        let _worker = SamplerWorker::start(window.0, feed.clone(), settings, command).unwrap();
        let wait_for_sample = || {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if feed.lock().unwrap().sample.take().is_some() {
                    return true;
                }
                thread::sleep(Duration::from_millis(20));
            }
            false
        };
        assert!(wait_for_sample());
        sender.send(WorkerCommand::Interval(Refresh::Five)).unwrap();
        thread::sleep(Duration::from_millis(2200));
        assert!(feed.lock().unwrap().sample.is_none());
        sender.send(WorkerCommand::Interval(Refresh::Two)).unwrap();
        assert!(wait_for_sample());
    }
}
