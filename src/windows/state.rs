use std::{env, fs, io, path::PathBuf, time::Duration};

use ::windows::{
    Win32::{
        Foundation::{ERROR_SUCCESS, POINT},
        System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
    },
    core::w,
};

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    fn parse(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    pub fn is_dark(self, taskbar: bool) -> bool {
        match self {
            Self::System => {
                let mut light = 1u32;
                let mut size = size_of::<u32>() as u32;
                let status = unsafe {
                    RegGetValueW(
                        HKEY_CURRENT_USER,
                        w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                        if taskbar {
                            w!("SystemUsesLightTheme")
                        } else {
                            w!("AppsUseLightTheme")
                        },
                        RRF_RT_REG_DWORD,
                        None,
                        Some((&mut light as *mut u32).cast()),
                        Some(&mut size),
                    )
                };
                status == ERROR_SUCCESS && light == 0
            }
            Self::Light => false,
            Self::Dark => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Refresh {
    #[default]
    One,
    Two,
    Three,
    Five,
}

impl Refresh {
    fn parse(value: &str) -> Self {
        match value {
            "2" => Self::Two,
            "3" => Self::Three,
            "5" => Self::Five,
            _ => Self::One,
        }
    }

    const fn seconds(self) -> u64 {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Three => 3,
            Self::Five => 5,
        }
    }

    pub const fn interval(self) -> Duration {
        Duration::from_secs(self.seconds())
    }
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum TaskbarSide {
    Left,
    #[default]
    Right,
}

impl TaskbarSide {
    fn parse(value: &str) -> Self {
        match value {
            "left" => Self::Left,
            _ => Self::Right,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Settings {
    pub traffic: u64,
    pub theme: Theme,
    pub topmost: bool,
    pub taskbar: bool,
    pub taskbar_side: TaskbarSide,
    pub refresh: Refresh,
    pub position: Option<POINT>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            traffic: 0,
            theme: Theme::System,
            topmost: true,
            taskbar: false,
            taskbar_side: TaskbarSide::default(),
            refresh: Refresh::default(),
            position: None,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let mut settings = Self::default();
        let Some(file) = state_file() else {
            return settings;
        };
        if let Ok(content) = fs::read_to_string(file) {
            for line in content.lines() {
                if let Some((key, value)) = line.split_once('=') {
                    match key {
                        "traffic" => settings.traffic = value.parse().unwrap_or_default(),
                        "theme" => settings.theme = Theme::parse(value),
                        "topmost" => settings.topmost = value.parse().unwrap_or(true),
                        "taskbar" => settings.taskbar = value.parse().unwrap_or(false),
                        "taskbar_side" => settings.taskbar_side = TaskbarSide::parse(value),
                        "refresh" => settings.refresh = Refresh::parse(value),
                        "position" => settings.position = parse_point(value),
                        _ => {}
                    }
                }
            }
        }
        settings
    }

    pub fn save(self) -> io::Result<()> {
        let Some(file) = state_file() else {
            return Ok(());
        };
        if let Some(directory) = file.parent() {
            fs::create_dir_all(directory)?;
        }
        let theme = match self.theme {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        };
        let position = self
            .position
            .map(|point| format!("{},{}", point.x, point.y))
            .unwrap_or_default();
        let temp = file.with_extension("tmp");
        fs::write(
            &temp,
            format!(
                "traffic={}\ntheme={theme}\ntopmost={}\ntaskbar={}\ntaskbar_side={}\nrefresh={}\nposition={position}\n",
                self.traffic,
                self.topmost,
                self.taskbar,
                self.taskbar_side.as_str(),
                self.refresh.seconds()
            ),
        )?;
        fs::rename(temp, file)
    }
}

fn parse_point(value: &str) -> Option<POINT> {
    let (x, y) = value.split_once(',')?;
    Some(POINT {
        x: x.parse().ok()?,
        y: y.parse().ok()?,
    })
}

fn state_file() -> Option<PathBuf> {
    Some(
        PathBuf::from(env::var_os("LOCALAPPDATA")?)
            .join("WindowsMonitor")
            .join("state.ini"),
    )
}
