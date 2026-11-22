#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Language {
    #[default]
    Chinese,
    English,
}

impl Language {
    pub fn choose<T>(self, chinese: T, english: T) -> T {
        match self {
            Self::Chinese => chinese,
            Self::English => english,
        }
    }
}
