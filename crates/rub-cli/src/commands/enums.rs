use clap::ValueEnum;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StateFormatArg {
    Snapshot,
    A11y,
    Compact,
}

impl StateFormatArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::A11y => "a11y",
            Self::Compact => "compact",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StorageAreaArg {
    Local,
    Session,
}

impl StorageAreaArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Session => "session",
        }
    }
}

/// CLI-facing interference mode selector.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum InterferenceModeArg {
    Normal,
    #[value(name = "public_web_stable")]
    PublicWebStable,
    Strict,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DownloadWaitStateArg {
    Started,
    #[value(name = "in_progress")]
    InProgress,
    Completed,
    Failed,
    Canceled,
    Terminal,
}

impl DownloadWaitStateArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Terminal => "terminal",
        }
    }
}
