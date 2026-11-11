use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;

/// Starts file logging to `<cache_dir>/senritsu.log`. Keep the guard alive for the process lifetime.
pub fn init(cache_dir: &Path) -> WorkerGuard {
    let appender = tracing_appender::rolling::never(cache_dir, "senritsu.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let _ = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .try_init();
    guard
}
