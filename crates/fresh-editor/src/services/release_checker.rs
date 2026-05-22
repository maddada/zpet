//! Release checker module for checking if a new version is available.
//!
//! This module provides functionality to:
//! - Check for new releases by fetching a GitHub releases API endpoint
//! - Detect the installation method (Homebrew, npm, cargo, etc.) based on executable path
//! - Provide appropriate update commands based on installation method
//! - Six-hour update checking (debounced via stamp file)

use super::time_source::SharedTimeSource;
use chrono::{DateTime, Utc};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// The current version of the editor
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default GitHub releases API URL for Gte.
pub const DEFAULT_RELEASES_URL: &str = "https://api.github.com/repos/maddada/gte/releases/latest";

/// How often Gte may contact the releases endpoint.
pub const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Homebrew command shown as the default update path.
pub const HOMEBREW_UPDATE_COMMAND: &str = "brew upgrade gte";

const UPDATE_STAMP_FILE_NAME: &str = "update_check_stamp";

/// Installation method detection result
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallMethod {
    /// Installed via Homebrew
    Homebrew,
    /// Installed via cargo
    Cargo,
    /// Installed via npm
    Npm,
    /// Installed via a Linux package manager (apt, dnf, etc.)
    PackageManager,
    /// Installed via AUR (Arch User Repository)
    Aur,
    /// Unknown installation method or manually installed
    Unknown,
}

impl InstallMethod {
    /// Get the update command for this installation method
    pub fn update_command(&self) -> Option<&'static str> {
        Some(match self {
            Self::Homebrew => HOMEBREW_UPDATE_COMMAND,
            Self::Cargo => "cargo install --locked gte",
            Self::Npm => "npm update -g gte",
            Self::Aur => "yay -Syu gte  # or use your AUR helper",
            Self::PackageManager => "Update using your system package manager",
            Self::Unknown => return None,
        })
    }
}

/// Result of checking for a new release
#[derive(Debug, Clone)]
pub struct ReleaseCheckResult {
    /// The latest version available
    pub latest_version: String,
    /// Whether an update is available
    pub update_available: bool,
    /// The detected installation method
    pub install_method: InstallMethod,
}

/// Handle to a background update check (one-shot)
///
/// Use `try_get_result` to check if the result is ready without blocking.
pub struct UpdateCheckHandle {
    receiver: Receiver<Result<ReleaseCheckResult, String>>,
    #[allow(dead_code)]
    thread: JoinHandle<()>,
}

impl UpdateCheckHandle {
    /// Try to get the result without blocking.
    /// Returns Some(result) if the check completed, None if still running.
    /// If still running, the background thread is abandoned (will be killed on process exit).
    pub fn try_get_result(self) -> Option<Result<ReleaseCheckResult, String>> {
        match self.receiver.try_recv() {
            Ok(result) => {
                tracing::debug!("Update check completed");
                Some(result)
            }
            Err(TryRecvError::Empty) => {
                // Still running - abandon the thread
                tracing::debug!("Update check still running, abandoning");
                drop(self.thread);
                None
            }
            Err(TryRecvError::Disconnected) => {
                // Thread panicked or exited without sending
                tracing::debug!("Update check thread disconnected");
                None
            }
        }
    }
}

/// Handle to an update checker running in the background.
///
/// Runs a single check at startup (if not already done in the last six hours).
/// Results are available via `poll_result()`.
pub struct UpdateChecker {
    /// Receiver for update check results
    receiver: Receiver<Result<ReleaseCheckResult, String>>,
    /// Background thread handle
    #[allow(dead_code)]
    thread: JoinHandle<()>,
    /// Last successful result (cached)
    last_result: Option<ReleaseCheckResult>,
}

/// Backwards compatibility alias
pub type PeriodicUpdateChecker = UpdateChecker;

fn update_stamp_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join("fresh").join(UPDATE_STAMP_FILE_NAME)
}

fn read_update_stamp_file(data_dir: &Path) -> Option<DateTime<Utc>> {
    let content = fs::read_to_string(update_stamp_file_path(data_dir)).ok()?;
    DateTime::parse_from_rfc3339(content.trim())
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn write_update_stamp_file(data_dir: &Path, checked_at: DateTime<Utc>) -> bool {
    let path = update_stamp_file_path(data_dir);
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            tracing::debug!("Failed to create update stamp directory: {}", e);
            return false;
        }
    }

    match fs::File::create(&path).and_then(|mut f| f.write_all(checked_at.to_rfc3339().as_bytes()))
    {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!("Failed to write update stamp file: {}", e);
            false
        }
    }
}

fn should_run_update_check(
    time_source: &dyn super::time_source::TimeSource,
    data_dir: &Path,
    interval: Duration,
) -> bool {
    let now = time_source.now_utc();
    if let Some(last_checked) = read_update_stamp_file(data_dir) {
        let within_interval = now
            .signed_duration_since(last_checked)
            .to_std()
            .map(|elapsed| elapsed < interval)
            .unwrap_or(true);
        if within_interval {
            tracing::debug!("Update check already done within interval, skipping");
            return false;
        }
    }

    let _ = write_update_stamp_file(data_dir, now);
    true
}

fn start_update_checker_with_interval(
    releases_url: &str,
    check_interval: Duration,
    time_source: SharedTimeSource,
    data_dir: PathBuf,
) -> UpdateChecker {
    tracing::debug!("Starting update checker");
    let url = releases_url.to_string();
    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        if let Some(unique_id) =
            super::telemetry::should_run_daily_check(time_source.as_ref(), &data_dir)
        {
            super::telemetry::track_open(&unique_id);
        }

        if should_run_update_check(time_source.as_ref(), &data_dir, check_interval) {
            let result = check_for_update(&url);
            // Receiver may be dropped if checker is dropped before result arrives.
            #[allow(clippy::let_underscore_must_use)]
            let _ = tx.send(result);
        }
    });

    UpdateChecker {
        receiver: rx,
        thread: handle,
        last_result: None,
    }
}

impl UpdateChecker {
    /// Poll for a new update check result without blocking.
    ///
    /// Returns `Some(result)` if a new check completed, `None` if no new result.
    /// Successful results are cached and can be retrieved via `get_cached_result()`.
    pub fn poll_result(&mut self) -> Option<Result<ReleaseCheckResult, String>> {
        match self.receiver.try_recv() {
            Ok(result) => {
                if let Ok(ref release_result) = result {
                    tracing::debug!(
                        "Update check completed: update_available={}",
                        release_result.update_available
                    );
                    self.last_result = Some(release_result.clone());
                }
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => None,
        }
    }

    /// Get the cached result from the last successful check.
    pub fn get_cached_result(&self) -> Option<&ReleaseCheckResult> {
        self.last_result.as_ref()
    }

    /// Check if an update is available (from cached result).
    pub fn is_update_available(&self) -> bool {
        self.last_result
            .as_ref()
            .map(|r| r.update_available)
            .unwrap_or(false)
    }

    /// Get the latest version string if an update is available.
    pub fn latest_version(&self) -> Option<&str> {
        self.last_result.as_ref().and_then(|r| {
            if r.update_available {
                Some(r.latest_version.as_str())
            } else {
                None
            }
        })
    }
}

/// Start an update checker that runs once at startup.
///
/// The check respects six-hour debouncing via the stamp file - if recently
/// checked, no network request is made.
/// Results are available via `poll_result()` on the returned handle.
pub fn start_periodic_update_check(
    releases_url: &str,
    time_source: SharedTimeSource,
    data_dir: PathBuf,
) -> UpdateChecker {
    start_update_checker_with_interval(releases_url, UPDATE_CHECK_INTERVAL, time_source, data_dir)
}

/// Start an update checker (for testing with custom parameters).
#[doc(hidden)]
pub fn start_periodic_update_check_with_interval(
    releases_url: &str,
    check_interval: Duration,
    time_source: SharedTimeSource,
    data_dir: PathBuf,
) -> UpdateChecker {
    start_update_checker_with_interval(releases_url, check_interval, time_source, data_dir)
}

/// Start a background update check
///
/// Returns a handle that can be used to query the result later.
/// The check runs in a background thread and won't block.
/// Respects six-hour debouncing - if recently checked, no result will be sent.
pub fn start_update_check(
    releases_url: &str,
    time_source: SharedTimeSource,
    data_dir: PathBuf,
) -> UpdateCheckHandle {
    tracing::debug!("Starting background update check");
    let url = releases_url.to_string();
    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        if let Some(unique_id) =
            super::telemetry::should_run_daily_check(time_source.as_ref(), &data_dir)
        {
            super::telemetry::track_open(&unique_id);
        }

        if should_run_update_check(time_source.as_ref(), &data_dir, UPDATE_CHECK_INTERVAL) {
            let result = check_for_update(&url);
            // Receiver may be dropped if handle is dropped before result arrives.
            #[allow(clippy::let_underscore_must_use)]
            let _ = tx.send(result);
        }
    });

    UpdateCheckHandle {
        receiver: rx,
        thread: handle,
    }
}

/// Fetches release information from the provided URL.
pub fn fetch_latest_version(url: &str) -> Result<String, String> {
    tracing::debug!("Fetching latest version from {}", url);
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .new_agent();
    let response = agent
        .get(url)
        .header("User-Agent", "gte-update-checker")
        .header("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            format!("HTTP request failed: {}", e)
        })?;

    let body = response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    let version = parse_version_from_json(&body)?;
    tracing::debug!("Latest version: {}", version);
    Ok(version)
}

/// Parse version from GitHub API JSON response
fn parse_version_from_json(json: &str) -> Result<String, String> {
    let tag_name_key = "\"tag_name\"";
    let start = json
        .find(tag_name_key)
        .ok_or_else(|| "tag_name not found in response".to_string())?;

    let after_key = &json[start + tag_name_key.len()..];

    let value_start = after_key
        .find('"')
        .ok_or_else(|| "Invalid JSON: missing quote after tag_name".to_string())?;

    let value_content = &after_key[value_start + 1..];
    let value_end = value_content
        .find('"')
        .ok_or_else(|| "Invalid JSON: unclosed quote".to_string())?;

    let tag = &value_content[..value_end];

    // Strip 'v' prefix if present
    Ok(tag.strip_prefix('v').unwrap_or(tag).to_string())
}

/// Detect the installation method based on the current executable path
pub fn detect_install_method() -> InstallMethod {
    match env::current_exe() {
        Ok(path) => detect_install_method_from_path(&path),
        Err(_) => InstallMethod::Unknown,
    }
}

/// Detect installation method from a given executable path
pub fn detect_install_method_from_path(exe_path: &Path) -> InstallMethod {
    let path_str = exe_path.to_string_lossy();

    // Check for Homebrew paths (macOS and Linux)
    if path_str.contains("/opt/homebrew/")
        || path_str.contains("/usr/local/Cellar/")
        || path_str.contains("/home/linuxbrew/")
        || path_str.contains("/.linuxbrew/")
    {
        return InstallMethod::Homebrew;
    }

    // Check for Cargo installation
    if path_str.contains("/.cargo/bin/") || path_str.contains("\\.cargo\\bin\\") {
        return InstallMethod::Cargo;
    }

    // Check for npm global installation
    if path_str.contains("/node_modules/")
        || path_str.contains("\\node_modules\\")
        || path_str.contains("/npm/")
        || path_str.contains("/lib/node_modules/")
    {
        return InstallMethod::Npm;
    }

    // Check for AUR installation (Arch Linux)
    if path_str.starts_with("/usr/bin/") && is_arch_linux() {
        return InstallMethod::Aur;
    }

    // Check for package manager installation (standard system paths)
    if path_str.starts_with("/usr/bin/")
        || path_str.starts_with("/usr/local/bin/")
        || path_str.starts_with("/bin/")
    {
        return InstallMethod::PackageManager;
    }

    InstallMethod::Unknown
}

/// Check if we're running on Arch Linux
fn is_arch_linux() -> bool {
    std::fs::read_to_string("/etc/os-release")
        .map(|content| content.contains("Arch Linux") || content.contains("ID=arch"))
        .unwrap_or(false)
}

/// Compare two semantic versions
/// Returns true if `latest` is newer than `current`
pub fn is_newer_version(current: &str, latest: &str) -> bool {
    let parse_version = |v: &str| -> Option<(u32, u32, u32)> {
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() >= 3 {
            Some((
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].split('-').next()?.parse().ok()?,
            ))
        } else if parts.len() == 2 {
            Some((parts[0].parse().ok()?, parts[1].parse().ok()?, 0))
        } else {
            None
        }
    };

    match (parse_version(current), parse_version(latest)) {
        (Some((c_major, c_minor, c_patch)), Some((l_major, l_minor, l_patch))) => {
            (l_major, l_minor, l_patch) > (c_major, c_minor, c_patch)
        }
        _ => false,
    }
}

/// Check for a new release (blocking)
pub fn check_for_update(releases_url: &str) -> Result<ReleaseCheckResult, String> {
    let latest_version = fetch_latest_version(releases_url)?;
    let install_method = detect_install_method();
    let update_available = is_newer_version(CURRENT_VERSION, &latest_version);

    tracing::debug!(
        current = CURRENT_VERSION,
        latest = %latest_version,
        update_available,
        install_method = ?install_method,
        "Release check complete"
    );

    Ok(ReleaseCheckResult {
        latest_version,
        update_available,
        install_method,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_is_newer_version() {
        // (current, latest, expected_newer)
        let cases = [
            ("0.1.26", "1.0.0", true),        // major bump
            ("0.1.26", "0.2.0", true),        // minor bump
            ("0.1.26", "0.1.27", true),       // patch bump
            ("0.1.26", "0.1.26", false),      // same
            ("0.1.26", "0.1.25", false),      // older patch
            ("0.2.0", "0.1.26", false),       // older minor
            ("1.0.0", "0.1.26", false),       // older major
            ("0.1.26-alpha", "0.1.27", true), // prerelease current
            ("0.1.26", "0.1.27-beta", true),  // prerelease latest
        ];
        for (current, latest, expected) in cases {
            assert_eq!(
                is_newer_version(current, latest),
                expected,
                "is_newer_version({:?}, {:?})",
                current,
                latest
            );
        }
    }

    #[test]
    fn test_detect_install_method() {
        let cases = [
            (
                "/opt/homebrew/Cellar/gte/0.3.6/bin/gte",
                InstallMethod::Homebrew,
            ),
            (
                "/opt/homebrew/Cellar/fresh/0.1.26/bin/fresh",
                InstallMethod::Homebrew,
            ),
            (
                "/home/linuxbrew/.linuxbrew/bin/fresh",
                InstallMethod::Homebrew,
            ),
            ("/home/user/.cargo/bin/gte", InstallMethod::Cargo),
            (
                "C:\\Users\\user\\.cargo\\bin\\gte.exe",
                InstallMethod::Cargo,
            ),
            (
                "/usr/local/lib/node_modules/gte/bin/gte",
                InstallMethod::Npm,
            ),
            ("/usr/local/bin/gte", InstallMethod::PackageManager),
            ("/home/user/downloads/gte", InstallMethod::Unknown),
        ];
        for (path, expected) in cases {
            assert_eq!(
                detect_install_method_from_path(&PathBuf::from(path)),
                expected,
                "detect_install_method({:?})",
                path
            );
        }
    }

    #[test]
    fn test_update_commands_use_gte_packages() {
        assert_eq!(
            InstallMethod::Homebrew.update_command(),
            Some("brew upgrade gte")
        );
        assert_eq!(
            InstallMethod::Cargo.update_command(),
            Some("cargo install --locked gte")
        );
        assert_eq!(
            InstallMethod::Npm.update_command(),
            Some("npm update -g gte")
        );
    }

    #[test]
    fn test_should_run_update_check_debounces_by_interval() {
        let time_source = super::super::time_source::TestTimeSource::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let interval = Duration::from_secs(6 * 60 * 60);

        assert!(should_run_update_check(
            &time_source,
            temp_dir.path(),
            interval
        ));
        assert!(!should_run_update_check(
            &time_source,
            temp_dir.path(),
            interval
        ));

        time_source.advance(interval - Duration::from_secs(1));
        assert!(!should_run_update_check(
            &time_source,
            temp_dir.path(),
            interval
        ));

        time_source.advance(Duration::from_secs(1));
        assert!(should_run_update_check(
            &time_source,
            temp_dir.path(),
            interval
        ));
    }

    #[test]
    fn test_parse_version_from_json() {
        // Various JSON formats should all parse correctly
        let cases = [
            (r#"{"tag_name": "v0.1.27"}"#, "0.1.27"),
            (r#"{"tag_name": "0.1.27"}"#, "0.1.27"),
            (
                r#"{"tag_name": "v0.2.0", "name": "v0.2.0", "draft": false}"#,
                "0.2.0",
            ),
        ];
        for (json, expected) in cases {
            assert_eq!(parse_version_from_json(json).unwrap(), expected);
        }

        // Verify mock version is detected as newer than current
        let version = parse_version_from_json(r#"{"tag_name": "v99.0.0"}"#).unwrap();
        assert!(is_newer_version(CURRENT_VERSION, &version));
    }

    #[test]
    fn test_current_version_is_valid() {
        let parts: Vec<&str> = CURRENT_VERSION.split('.').collect();
        assert!(parts.len() >= 2, "Version should have at least major.minor");
        assert!(parts[0].parse::<u32>().is_ok());
        assert!(parts[1].parse::<u32>().is_ok());
    }

    use std::sync::mpsc as std_mpsc;

    /// Test helper: start a local HTTP server that returns a mock release JSON
    /// Returns (stop_sender, url) - send to stop_sender to shut down the server
    fn start_mock_release_server(version: &str) -> (std_mpsc::Sender<()>, String) {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("Failed to start test server");
        let port = server.server_addr().to_ip().unwrap().port();
        let url = format!("http://127.0.0.1:{}/releases/latest", port);

        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();

        // Spawn a thread to handle requests
        let version = version.to_string();
        thread::spawn(move || {
            loop {
                // Check for stop signal
                if stop_rx.try_recv().is_ok() {
                    break;
                }

                // Non-blocking receive with timeout
                match server.recv_timeout(Duration::from_millis(100)) {
                    Ok(Some(request)) => {
                        let response_body = format!(r#"{{"tag_name": "v{}"}}"#, version);
                        let response = tiny_http::Response::from_string(response_body).with_header(
                            tiny_http::Header::from_bytes(
                                &b"Content-Type"[..],
                                &b"application/json"[..],
                            )
                            .unwrap(),
                        );
                        drop(request.respond(response));
                    }
                    Ok(None) => {
                        // Timeout, continue loop
                    }
                    Err(_) => {
                        // Server error, exit
                        break;
                    }
                }
            }
        });

        (stop_tx, url)
    }

    #[test]
    fn test_update_checker_detects_new_version() {
        let (stop_tx, url) = start_mock_release_server("99.0.0");
        let time_source = super::super::time_source::TestTimeSource::shared();
        let temp_dir = tempfile::tempdir().unwrap();

        let mut checker =
            start_periodic_update_check(&url, time_source, temp_dir.path().to_path_buf());

        // Wait for result
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            if checker.poll_result().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert!(checker.is_update_available());
        assert_eq!(checker.latest_version(), Some("99.0.0"));

        stop_tx.send(()).ok();
    }

    #[test]
    fn test_update_checker_no_update_when_current() {
        let (stop_tx, url) = start_mock_release_server(CURRENT_VERSION);
        let time_source = super::super::time_source::TestTimeSource::shared();
        let temp_dir = tempfile::tempdir().unwrap();

        let mut checker =
            start_periodic_update_check(&url, time_source, temp_dir.path().to_path_buf());

        // Wait for result
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            if checker.poll_result().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert!(!checker.is_update_available());
        assert!(checker.latest_version().is_none());
        assert!(checker.get_cached_result().is_some());

        stop_tx.send(()).ok();
    }

    #[test]
    fn test_update_checker_api_before_result() {
        let (stop_tx, url) = start_mock_release_server("99.0.0");
        let time_source = super::super::time_source::TestTimeSource::shared();
        let temp_dir = tempfile::tempdir().unwrap();

        let checker = start_periodic_update_check(&url, time_source, temp_dir.path().to_path_buf());

        // Immediately check (before result arrives)
        assert!(!checker.is_update_available());
        assert!(checker.latest_version().is_none());
        assert!(checker.get_cached_result().is_none());

        stop_tx.send(()).ok();
    }
}
