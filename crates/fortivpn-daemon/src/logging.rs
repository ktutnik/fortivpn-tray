//! Persistent daemon log.
//!
//! macOS keeps oslog Info/Debug entries in a memory ring buffer and purges them
//! within minutes — a tunnel drop from an hour ago leaves no trace at all, which
//! makes intermittent failures impossible to investigate after the fact. Every
//! line the daemon logs therefore also goes to a rotating file on disk, which
//! survives purging, daemon restarts and reboots.
//!
//! On macOS output still reaches oslog too, so `log stream` keeps working for
//! live watching. Elsewhere it goes to stderr as before.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use log::{LevelFilter, Log, Metadata, Record};

/// Rotate once the active file passes this size.
const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// How many rotated generations to keep alongside the active file.
const GENERATIONS: usize = 3;

/// Path of the active log file.
pub fn log_path() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("fortivpn-tray");
    let _ = fs::create_dir_all(&dir);
    dir.join("daemon.log")
}

/// Local timestamp with milliseconds — logs are read by people, and correlating
/// a drop against "it broke around 2pm" needs local time, not UTC.
fn timestamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S%.3f")
        .to_string()
}

struct RotatingFile {
    path: PathBuf,
    file: Option<File>,
    written: u64,
}

impl RotatingFile {
    fn new(path: PathBuf) -> Self {
        let written = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        Self {
            path,
            file,
            written,
        }
    }

    /// `daemon.log` → `daemon.log.1` → … → `daemon.log.N`, oldest discarded.
    fn rotate(&mut self) {
        self.file = None;

        for gen in (1..GENERATIONS).rev() {
            let from = self.path.with_extension(format!("log.{gen}"));
            let to = self.path.with_extension(format!("log.{}", gen + 1));
            let _ = fs::rename(from, to);
        }
        let _ = fs::rename(&self.path, self.path.with_extension("log.1"));

        self.written = 0;
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok();
    }

    fn write_line(&mut self, line: &str) {
        if self.written >= MAX_BYTES {
            self.rotate();
        }
        if let Some(ref mut f) = self.file {
            // A failed write must never take the daemon down or spam the log.
            if writeln!(f, "{line}").is_ok() {
                self.written += line.len() as u64 + 1;
            }
        }
    }
}

/// Verbosity for a given log target.
///
/// Our own code is deliberately verbose — that is the whole point of the file.
/// Third-party crates are not: rustls alone emits six debug lines per TLS
/// handshake, which on a reconnect loop would bury every diagnostic we came for.
fn level_for(target: &str) -> LevelFilter {
    if target.starts_with("vpn") || target.starts_with("ipc") || target.starts_with("daemon") {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    }
}

/// Writes every record to the rotating file, and mirrors it to the platform's
/// native logger.
struct DaemonLogger {
    file: Mutex<RotatingFile>,
    #[cfg(target_os = "macos")]
    os: oslog::OsLogger,
}

impl Log for DaemonLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= level_for(metadata.target())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let line = format!(
            "{} {:<5} [{}] {}",
            timestamp(),
            record.level(),
            record.target(),
            record.args()
        );

        if let Ok(mut f) = self.file.lock() {
            f.write_line(&line);
        }

        #[cfg(target_os = "macos")]
        self.os.log(record);

        #[cfg(not(target_os = "macos"))]
        eprintln!("{line}");
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            if let Some(ref mut file) = f.file {
                let _ = file.flush();
            }
        }
    }
}

/// Install the daemon logger. Returns the path being written to.
pub fn init() -> PathBuf {
    let path = log_path();

    let logger = DaemonLogger {
        file: Mutex::new(RotatingFile::new(path.clone())),
        #[cfg(target_os = "macos")]
        os: oslog::OsLogger::new("com.fortivpn-tray")
            .level_filter(LevelFilter::Info)
            .category_level_filter("ipc", LevelFilter::Debug)
            .category_level_filter("vpn", LevelFilter::Debug),
    };

    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(LevelFilter::Debug);
    }

    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("fortivpn-logtest-{name}"));
        let _ = fs::remove_file(&p);
        for gen in 1..=GENERATIONS + 1 {
            let _ = fs::remove_file(p.with_extension(format!("log.{gen}")));
        }
        p
    }

    #[test]
    fn test_writes_lines_to_file() {
        let path = temp_path("write");
        let mut f = RotatingFile::new(path.clone());
        f.write_line("hello");
        f.write_line("world");
        drop(f);

        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("hello"));
        assert!(body.contains("world"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_appends_across_reopen() {
        // A daemon restart must not truncate the previous investigation.
        let path = temp_path("append");
        let mut f = RotatingFile::new(path.clone());
        f.write_line("first run");
        drop(f);

        let mut f = RotatingFile::new(path.clone());
        f.write_line("second run");
        drop(f);

        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("first run"));
        assert!(body.contains("second run"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_rotation_moves_old_content_aside() {
        let path = temp_path("rotate");
        let mut f = RotatingFile::new(path.clone());
        f.write_line("old content");
        // Pretend the file is already at the size ceiling.
        f.written = MAX_BYTES;
        f.write_line("new content");
        drop(f);

        let active = fs::read_to_string(&path).unwrap();
        let rotated = fs::read_to_string(path.with_extension("log.1")).unwrap();
        assert!(active.contains("new content"));
        assert!(!active.contains("old content"));
        assert!(rotated.contains("old content"));

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("log.1"));
    }

    #[test]
    fn test_rotation_keeps_bounded_generations() {
        let path = temp_path("bounded");
        let mut f = RotatingFile::new(path.clone());
        for i in 0..GENERATIONS + 3 {
            f.write_line(&format!("generation {i}"));
            f.written = MAX_BYTES;
        }
        drop(f);

        let overflow = path.with_extension(format!("log.{}", GENERATIONS + 1));
        assert!(
            !overflow.exists(),
            "kept more generations than the {GENERATIONS} configured"
        );

        let _ = fs::remove_file(&path);
        for gen in 1..=GENERATIONS {
            let _ = fs::remove_file(path.with_extension(format!("log.{gen}")));
        }
    }

    #[test]
    fn test_our_targets_log_at_debug() {
        assert_eq!(level_for("vpn"), LevelFilter::Debug);
        assert_eq!(level_for("ipc"), LevelFilter::Debug);
        assert_eq!(level_for("daemon"), LevelFilter::Debug);
    }

    #[test]
    fn test_third_party_debug_is_suppressed() {
        // rustls emits several debug lines per handshake; on a reconnect loop
        // that noise would bury the diagnostics the file exists to capture.
        assert_eq!(level_for("rustls::client::hs"), LevelFilter::Info);
        assert_eq!(level_for("rustls::client::tls13"), LevelFilter::Info);
        assert_eq!(level_for("tokio_util::codec"), LevelFilter::Info);
        assert!(log::Level::Debug > LevelFilter::Info);
    }

    #[test]
    fn test_timestamp_has_millisecond_resolution() {
        // Drops are correlated against each other; second resolution is too coarse.
        let ts = timestamp();
        assert_eq!(ts.len(), "2026-08-01 14:30:00.000".len(), "got {ts}");
        assert_eq!(ts.chars().filter(|c| *c == '.').count(), 1);
    }
}
