//! リリースGUIでも取得できる日次ローテーションログ。

use std::path::{Path, PathBuf};

use known_folders::{get_known_folder_path, KnownFolder};
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::EnvFilter;

pub struct LoggingGuard {
    _worker: Option<WorkerGuard>,
}

fn log_directory_from_local_app_data(local_app_data: &Path) -> PathBuf {
    local_app_data.join("StreamPainter").join("logs")
}

pub fn log_directory() -> Option<PathBuf> {
    get_known_folder_path(KnownFolder::LocalAppData)
        .map(|path| log_directory_from_local_app_data(&path))
}

fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())
}

pub fn init() -> LoggingGuard {
    let file_appender = log_directory().and_then(|directory| {
        std::fs::create_dir_all(&directory).ok()?;
        RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix("stream-painter")
            .filename_suffix("log")
            .max_log_files(7)
            .build(directory)
            .ok()
    });

    if let Some(file_appender) = file_appender {
        let (writer, worker) = tracing_appender::non_blocking(file_appender);
        tracing_subscriber::fmt()
            .with_env_filter(env_filter())
            .with_ansi(false)
            .with_thread_names(true)
            .with_writer(writer)
            .init();
        tracing::info!(
            "file logging initialized: {}",
            log_directory()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "unknown".into())
        );
        LoggingGuard {
            _worker: Some(worker),
        }
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter())
            .with_ansi(false)
            .with_thread_names(true)
            .init();
        tracing::warn!("file logging is unavailable; using process output only");
        LoggingGuard { _worker: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_are_stored_below_local_app_data() {
        assert_eq!(
            log_directory_from_local_app_data(Path::new("LocalAppData")),
            Path::new("LocalAppData").join("StreamPainter").join("logs")
        );
    }
}
