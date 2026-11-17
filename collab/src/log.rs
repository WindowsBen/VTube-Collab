//! The log file: what happened during a session, for troubleshooting testers' reports.
//!
//! Written to %LOCALAPPDATA%\VTubeCollab\logs\<program>-<date>_<time>.log (one file per
//! session, the 10 newest kept). Contains events, stats and errors — but never the
//! connection codes themselves (they contain network addresses).

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static FILE: OnceLock<Mutex<File>> = OnceLock::new();
static PATH: OnceLock<PathBuf> = OnceLock::new();

const KEEP_FILES: usize = 10;

/// The folder the log files go in.
pub fn folder() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("VTubeCollab").join("logs")
}

/// This session's log file, once `start` has run.
pub fn path() -> Option<&'static PathBuf> {
    PATH.get()
}

/// Start this session's log: create the file, write a header about this PC, tidy
/// up old logs, and also log crashes. `program` = "app" or "cli".
pub fn start(program: &str) {
    let dir = folder();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    remove_old_logs(&dir);

    let (date, time) = now_parts();
    let path = dir.join(format!("{program}-{date}_{}.log", time.replace(':', "-")));
    let Ok(file) = File::create(&path) else { return };
    let _ = FILE.set(Mutex::new(file));
    let _ = PATH.set(path);

    info(&format!("VTube Collab {} ({program}) started", env!("CARGO_PKG_VERSION")));
    info(&format!("Windows: {}", windows_version()));
    for (i, card) in vtc_core::system::graphics_cards().iter().enumerate() {
        info(&format!("Graphics card {i}: {card}{}", if i == 0 { " (default)" } else { "" }));
    }
    info(&format!("Spout2 sources: {:?}", crate::avatar_sources()));

    // Crashes: write what happened to the log before the program ends.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        write("CRASH", &format!("{panic}\n{backtrace}"));
        default_hook(panic);
    }));
}

/// Log a connection event (warnings at warning level, everything else as info).
pub fn event(event: &crate::Event) {
    match event {
        crate::Event::Warning(text) => warning(text),
        other => info(&format!("{other:?}")),
    }
}

pub fn info(message: &str) {
    write("INFO", message);
}

pub fn warning(message: &str) {
    write("WARN", message);
}

pub fn error(message: &str) {
    write("ERROR", message);
}

fn write(level: &str, message: &str) {
    let Some(file) = FILE.get() else { return };
    let (date, time) = now_parts();
    if let Ok(mut file) = file.lock() {
        let _ = writeln!(file, "{date} {time} UTC [{level}] {message}");
        let _ = file.flush();
    }
}

/// Keep only the newest log files.
fn remove_old_logs(dir: &PathBuf) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut logs: Vec<_> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    logs.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
    for (_, old) in logs.into_iter().skip(KEEP_FILES - 1) {
        let _ = std::fs::remove_file(old);
    }
}

/// Windows' own version text, e.g. "Microsoft Windows [Version 10.0.26200.1234]".
fn windows_version() -> String {
    std::process::Command::new("cmd")
        .args(["/C", "ver"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// Current UTC date and time as ("2026-11-17", "21:04:05.123").
fn now_parts() -> (String, String) {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Days since 1970-01-01 → year/month/day (Howard Hinnant's "civil from days").
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        format!("{year:04}-{month:02}-{day:02}"),
        format!("{:02}:{:02}:{:02}.{:03}", rest / 3600, rest % 3600 / 60, rest % 60, since.subsec_millis()),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn date_looks_right() {
        let (date, time) = super::now_parts();
        assert_eq!(date.len(), 10);
        assert!(date.starts_with("20"));
        assert_eq!(time.len(), 12);
    }
}
