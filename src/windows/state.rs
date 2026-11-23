use std::{env, fs, io, path::PathBuf, time::Duration};

use ::windows::{
    Win32::{
        Foundation::{ERROR_SUCCESS, POINT},
        Globalization::GetUserDefaultUILanguage,
        System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
    },
    core::w,
};

use crate::{language::Language, model::Metrics};

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum LanguagePreference {
    #[default]
    System,
    Chinese,
    English,
}

impl LanguagePreference {
    fn parse(value: &str) -> Self {
        match value {
            "zh-CN" => Self::Chinese,
            "en-US" => Self::English,
            _ => Self::System,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Chinese => "zh-CN",
            Self::English => "en-US",
        }
    }

    pub fn resolve(self) -> Language {
        match self {
            Self::Chinese => Language::Chinese,
            Self::English => Language::English,
            // The low 10 bits are the primary language ID (LANG_CHINESE = 4).
            Self::System if (unsafe { GetUserDefaultUILanguage() } & 0x03ff) == 4 => {
                Language::Chinese
            }
            Self::System => Language::English,
        }
    }
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
    Transparent,
}

impl Theme {
    fn parse(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            "transparent" => Self::Transparent,
            _ => Self::System,
        }
    }

    pub fn is_dark(self, taskbar: bool) -> bool {
        match self {
            Self::System | Self::Transparent => {
                let mut light = 1u32;
                let mut size = size_of::<u32>() as u32;
                let status = unsafe {
                    RegGetValueW(
                        HKEY_CURRENT_USER,
                        w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                        if taskbar || self == Self::Transparent {
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

/// 双击监控面板时执行的操作。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DoubleClick {
    Nothing,
    #[default]
    TaskManager,
    ResourceMonitor,
    Manager,
}

impl DoubleClick {
    fn parse(value: &str) -> Self {
        match value {
            "none" => Self::Nothing,
            "resource_monitor" => Self::ResourceMonitor,
            "manager" => Self::Manager,
            _ => Self::TaskManager,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Nothing => "none",
            Self::TaskManager => "task_manager",
            Self::ResourceMonitor => "resource_monitor",
            Self::Manager => "manager",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Settings {
    pub traffic: u64,
    pub language: LanguagePreference,
    pub theme: Theme,
    pub label_bold: bool,
    pub topmost: bool,
    pub taskbar: bool,
    pub taskbar_side: TaskbarSide,
    pub refresh: Refresh,
    pub record_history: bool,
    pub double_click: DoubleClick,
    pub display: DisplayOptions,
    pub position: Option<POINT>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            traffic: 0,
            language: LanguagePreference::System,
            theme: Theme::System,
            label_bold: true,
            topmost: true,
            taskbar: false,
            taskbar_side: TaskbarSide::default(),
            refresh: Refresh::default(),
            record_history: false,
            double_click: DoubleClick::default(),
            display: DisplayOptions {
                language: LanguagePreference::System.resolve(),
                ..Default::default()
            },
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
                        "language" => settings.language = LanguagePreference::parse(value),
                        "theme" => settings.theme = Theme::parse(value),
                        "label_bold" => settings.label_bold = value.parse().unwrap_or(true),
                        "topmost" => settings.topmost = value.parse().unwrap_or(true),
                        "taskbar" => settings.taskbar = value.parse().unwrap_or(false),
                        "taskbar_side" => settings.taskbar_side = TaskbarSide::parse(value),
                        "refresh" => settings.refresh = Refresh::parse(value),
                        "record_history" => {
                            settings.record_history = value.parse().unwrap_or(false)
                        }
                        "double_click" => settings.double_click = DoubleClick::parse(value),
                        "metrics" => settings.display.metrics = Metrics::parse(value),
                        "density" => settings.display.density = Density::parse(value),
                        "text_size" => {
                            settings.display.text_size = value
                                .parse()
                                .ok()
                                .and_then(TextSize::new)
                                .unwrap_or_default();
                        }
                        "position" => settings.position = parse_point(value),
                        _ => {}
                    }
                }
            }
        }
        settings.display.language = settings.language.resolve();
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
            Theme::Transparent => "transparent",
        };
        let position = self
            .position
            .map(|point| format!("{},{}", point.x, point.y))
            .unwrap_or_default();
        let metrics = self.display.metrics.to_config();
        let text_size = self.display.text_size.pixels();
        let density = self.display.density.as_str();
        let temp = file.with_extension("tmp");
        fs::write(
            &temp,
            format!(
                "traffic={}\ntheme={theme}\nlanguage={}\nlabel_bold={}\ntopmost={}\ntaskbar={}\ntaskbar_side={}\nrefresh={}\nrecord_history={}\ndouble_click={}\nposition={position}\nmetrics={metrics}\ntext_size={text_size}\ndensity={density}\n",
                self.traffic,
                self.language.key(),
                self.label_bold,
                self.topmost,
                self.taskbar,
                self.taskbar_side.as_str(),
                self.refresh.seconds(),
                self.record_history,
                self.double_click.as_str(),
            ),
        )?;
        fs::rename(temp, file)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DisplayOptions {
    pub language: Language,
    pub metrics: Metrics,
    pub text_size: TextSize,
    pub density: Density,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Density {
    #[default]
    Standard,
    Compact,
    Tight,
}

impl Density {
    fn parse(value: &str) -> Self {
        match value {
            "compact" => Self::Compact,
            "tight" => Self::Tight,
            _ => Self::Standard,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Compact => "compact",
            Self::Tight => "tight",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextSize(i32);

impl TextSize {
    pub const MIN: i32 = 10;
    pub const MAX: i32 = 24;
    pub const DEFAULT: i32 = 16;

    pub fn new(pixels: i32) -> Option<Self> {
        (Self::MIN..=Self::MAX)
            .contains(&pixels)
            .then_some(Self(pixels))
    }

    pub fn pixels(self) -> i32 {
        self.0
    }

    pub fn factor(self) -> f32 {
        self.0 as f32 / Self::DEFAULT as f32
    }

    pub fn scale(self, length: i32) -> i32 {
        (length * self.0 + Self::DEFAULT - 1) / Self::DEFAULT
    }
}

impl Default for TextSize {
    fn default() -> Self {
        Self(Self::DEFAULT)
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
    Some(data_directory()?.join("state.ini"))
}

pub fn data_directory() -> Option<PathBuf> {
    Some(PathBuf::from(env::var_os("LOCALAPPDATA")?).join("WindowsMonitor"))
}
