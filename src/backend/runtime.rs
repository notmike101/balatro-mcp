use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
static EMPTY_READY: LazyLock<serde_json::Map<String, Value>> = LazyLock::new(serde_json::Map::new);

use serde_json::Value;

pub const ALLOWED_SEED: &str = "2K9H9HN";
pub const MAX_OBSERVATION_AGE_SECONDS: f64 = 2.0;
pub const EXPECTED_BRIDGE_VERSION: &str = "0.6.0";
pub const MUTATION_LOCK_FILE: &str = "mcp_runtime.lock";

pub struct RuntimeMutationLock {
    path: PathBuf,
}

pub struct RuntimeMutationGuard {
    file: File,
}

impl RuntimeMutationLock {
    pub fn new(runtime_root: &Path) -> Self {
        Self {
            path: runtime_root.join("agent").join(MUTATION_LOCK_FILE),
        }
    }

    pub fn try_acquire(&self) -> Result<RuntimeMutationGuard, String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create runtime lock directory: {error}"))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(|error| format!("cannot open runtime mutation lock: {error}"))?;
        file.try_lock_exclusive().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                "runtime mutation lock is held by another MCP process".into()
            } else {
                format!("cannot acquire runtime mutation lock: {error}")
            }
        })?;
        Ok(RuntimeMutationGuard { file })
    }

    pub fn available(&self) -> bool {
        self.try_acquire().is_ok()
    }
}

impl Drop for RuntimeMutationGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn reset_runtime_databases(runtime_root: &Path, confirmed: bool) -> Result<Value, String> {
    if !confirmed {
        return Err("state reset requires --confirm".into());
    }
    let lock = RuntimeMutationLock::new(runtime_root);
    let _guard = lock.try_acquire()?;
    let agent_root = runtime_root.join("agent");
    fs::create_dir_all(&agent_root)
        .map_err(|error| format!("cannot create runtime agent directory: {error}"))?;
    let archive_root = agent_root.join(format!(
        "archive-state-reset-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock error: {error}"))?
            .as_millis()
    ));
    fs::create_dir_all(&archive_root)
        .map_err(|error| format!("cannot create reset archive: {error}"))?;

    let mut archived = Vec::new();
    for database in ["rust_state.db", "replays.db"] {
        for suffix in ["", "-wal", "-shm"] {
            let source = agent_root.join(format!("{database}{suffix}"));
            if !source.exists() {
                continue;
            }
            let destination = archive_root.join(format!("{database}{suffix}"));
            fs::rename(&source, &destination).map_err(|error| {
                format!(
                    "cannot archive {} to {}: {error}",
                    source.display(),
                    destination.display()
                )
            })?;
            archived.push(destination.display().to_string());
        }
    }
    Ok(serde_json::json!({
        "reset": true,
        "archive_directory": archive_root.display().to_string(),
        "archived": archived,
        "databases": ["rust_state.db", "replays.db"]
    }))
}

pub fn cli(runtime_root: &Path, args: &[String]) -> Result<Option<Value>, String> {
    if args.first().map(String::as_str) != Some("state") {
        return Ok(None);
    }
    if args.get(1).map(String::as_str) != Some("reset") {
        return Err("usage: state reset --confirm".into());
    }
    let confirmed = args.iter().any(|arg| arg == "--confirm");
    Ok(Some(reset_runtime_databases(runtime_root, confirmed)?))
}

#[derive(Debug, Clone)]
pub struct SafetyError(pub String);
impl std::fmt::Display for SafetyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "safety: {}", self.0)
    }
}
impl std::error::Error for SafetyError {}

/// Check for Balatro.exe processes using tasklist.
pub fn balatro_processes() -> Result<Vec<Value>, String> {
    let output = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq Balatro.exe", "/FO", "CSV", "/NH"])
        .output()
        .map_err(|e| format!("tasklist failed: {e}"))?;

    if !output.status.success() {
        return Ok(vec![]);
    }

    Ok(parse_process_list(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_process_list(stdout: &str) -> Vec<Value> {
    let mut processes = Vec::new();

    for line in stdout.lines() {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() >= 2 && fields[0].trim().trim_matches('"') == "Balatro.exe" {
            if let Ok(pid) = fields[1].trim().trim_matches('"').parse::<u32>() {
                processes.push(serde_json::json!({"pid": pid, "name": "Balatro.exe"}));
            }
        }
    }

    processes
}

/// Get the age of the observation file in seconds.
pub fn observation_age(observation_path: &PathBuf) -> Option<f64> {
    if !observation_path.exists() {
        return None;
    }
    let metadata = observation_path.metadata().ok()?;
    let mtime = metadata.modified().ok()?;
    let now = SystemTime::now();
    let duration = now.duration_since(mtime).ok()?;
    Some(duration.as_secs_f64())
}

/// Extract the seed from observation data.
pub fn observation_seed(observation: &Value) -> Option<String> {
    let seed = [
        observation.pointer("/round/seed"),
        observation.pointer("/ready/saved_game_seed"),
        observation.pointer("/run/seed"),
        observation.pointer("/run_info/seed"),
        observation.pointer("/game/seed"),
    ]
    .into_iter()
    .flatten()
    .next()?;
    seed.as_str()
        .map(str::to_owned)
        .or_else(|| Some(seed.to_string()))
}

/// Validate that we're in a safe runtime state for mutations.
pub fn validate_runtime(
    observation: &Value,
    processes: &[Value],
    age_seconds: Option<f64>,
) -> Result<(), SafetyError> {
    let found = processes;
    if found.len() != 1 {
        return Err(SafetyError(format!(
            "expected exactly one Balatro.exe process; found {}",
            found.len()
        )));
    }

    let age = age_seconds;
    if age.is_none() || age.unwrap_or(f64::MAX) > MAX_OBSERVATION_AGE_SECONDS {
        return Err(SafetyError(format!(
            "observation stale or missing: age={:?}",
            age
        )));
    }

    let bridge: &serde_json::Map<String, Value> = observation
        .get("bridge")
        .and_then(|b| b.as_object())
        .ok_or_else(|| SafetyError("bridge missing".into()))?;
    if !bridge
        .get("loaded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Err(SafetyError("AgentAutomation bridge not loaded".into()));
    }
    if bridge.get("version").and_then(|v| v.as_str()) != Some(EXPECTED_BRIDGE_VERSION) {
        return Err(SafetyError(format!(
            "bridge version {} loaded; restart Balatro to load {}",
            bridge
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
            EXPECTED_BRIDGE_VERSION
        )));
    }
    if bridge.get("session_id").is_none() {
        return Err(SafetyError("bridge session_id missing".into()));
    }

    Ok(())
}

/// Validate seed safety.
pub fn validate_seed(observation: &Value, requested_seed: Option<&str>) -> Result<(), SafetyError> {
    let live_seed = observation_seed(observation);

    if let Some(rs) = requested_seed {
        if rs != ALLOWED_SEED {
            return Err(SafetyError(format!(
                "seed rejected: {}; only {} is allowed",
                rs, ALLOWED_SEED
            )));
        }
    }

    if let Some(ls) = &live_seed {
        if ls != ALLOWED_SEED {
            return Err(SafetyError(format!(
                "live or saved seed rejected: {}; expected {}",
                ls, ALLOWED_SEED
            )));
        }
    }

    Ok(())
}

/// Guard a command before writing it to the IPC channel.
pub fn guard_command(command: &Value, observation: &Value) -> Result<(), SafetyError> {
    let action = command
        .get("action")
        .or_else(|| command.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let requested_seed = command.get("seed").and_then(|s| s.as_str());
    let ready_map: Option<&serde_json::Map<String, Value>> =
        observation.get("ready").and_then(|r| r.as_object());
    let ready: &serde_json::Map<String, Value> = ready_map.unwrap_or(&EMPTY_READY);
    let live_seed = observation_seed(observation);

    let wrong_seed_recovery = match (&live_seed, action) {
        (Some(ls), _) if ls != ALLOWED_SEED && ls != "" => {
            action == "setup_new_run"
                || (action == "ui_click"
                    && command.get("ui_id").and_then(|v| v.as_str()) == Some("main_menu_play"))
                || (action == "start_run"
                    && requested_seed == Some(ALLOWED_SEED)
                    && ready.get("current_setup").and_then(|v| v.as_str()) == Some("New Run"))
        }
        _ => false,
    };

    if !wrong_seed_recovery {
        validate_seed(observation, requested_seed)?;
    }

    if ready
        .get("saved_game_present")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        if action == "setup_new_run" {
            let saved_seed = ready
                .get("saved_game_seed")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            if saved_seed == ALLOWED_SEED {
                return Err(SafetyError(
                    "saved run exists; resume it instead of starting a new run".into(),
                ));
            }
        }
        if action == "start_run" && requested_seed == Some(ALLOWED_SEED) {
            return Err(SafetyError(
                "saved run exists; resume it without supplying a seed".into(),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_allowed_seed() {
        assert_eq!(ALLOWED_SEED, "2K9H9HN");
    }

    #[test]
    fn runtime_mutation_lock_is_exclusive_until_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = RuntimeMutationLock::new(dir.path());
        let second = RuntimeMutationLock::new(dir.path());
        let guard = first.try_acquire().unwrap();
        assert!(second.try_acquire().is_err());
        drop(guard);
        assert!(second.try_acquire().is_ok());
    }

    #[test]
    fn reset_requires_confirmation_and_archives_both_databases() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("rust_state.db"), b"state").unwrap();
        std::fs::write(agent.join("replays.db"), b"replays").unwrap();
        assert!(reset_runtime_databases(dir.path(), false).is_err());
        let result = reset_runtime_databases(dir.path(), true).unwrap();
        assert_eq!(result["reset"], true);
        assert!(!agent.join("rust_state.db").exists());
        assert!(!agent.join("replays.db").exists());
        assert_eq!(result["archived"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn test_max_observation_age() {
        assert_eq!(MAX_OBSERVATION_AGE_SECONDS, 2.0);
    }
    #[test]
    fn test_bridge_version() {
        assert_eq!(EXPECTED_BRIDGE_VERSION, "0.6.0");
    }
    #[test]
    fn test_observation_seed_with_round_seed() {
        let obs = json!({"round":{"seed":"2K9H9HN"},"ready":{}});
        assert_eq!(observation_seed(&obs), Some("2K9H9HN".to_string()));
    }
    #[test]
    fn test_observation_seed_with_saved_game_seed() {
        let obs = json!({"round":{},"ready":{"saved_game_seed":"2K9H9HN"}});
        assert_eq!(observation_seed(&obs), Some("2K9H9HN".to_string()));
    }
    #[test]
    fn test_observation_seed_from_menu_ready_state_without_round() {
        let obs = json!({"game":{"state":"MENU"},"ready":{"saved_game_seed":"2K9H9HN"}});
        assert_eq!(observation_seed(&obs), Some("2K9H9HN".to_string()));
    }
    #[test]
    fn test_observation_seed_from_menu_run_info() {
        let obs = json!({"game":{"state":"MENU"},"run_info":{"seed":"2K9H9HN"}});
        assert_eq!(observation_seed(&obs), Some("2K9H9HN".to_string()));
    }
    #[test]
    fn test_observation_seed_prefers_round_seed() {
        let obs = json!({"round":{"seed":"ROUND"},"ready":{"saved_game_seed":"SAVED"}});
        assert_eq!(observation_seed(&obs), Some("ROUND".to_string()));
    }
    #[test]
    fn test_observation_seed_missing() {
        let obs = json!({});
        assert_eq!(observation_seed(&obs), None);
    }
    #[test]
    fn test_observation_seed_non_string_is_preserved() {
        let obs = json!({"round":{"seed":123},"ready":{}});
        assert_eq!(observation_seed(&obs), Some("123".to_string()));
    }
    #[test]
    fn test_process_list_parser_filters_invalid_rows() {
        let output =
            "\"Balatro.exe\",1234,\"Console\"\n\"Other.exe\",22\n\"Balatro.exe\",bad\nshort";
        let processes = parse_process_list(output);
        assert_eq!(processes, vec![json!({"pid":1234,"name":"Balatro.exe"})]);
    }
    #[test]
    fn test_observation_age_missing_and_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        assert!(observation_age(&missing).is_none());
        let present = dir.path().join("observation.json");
        std::fs::write(&present, "{}").unwrap();
        assert!(observation_age(&present).unwrap() < 2.0);
    }
    #[test]
    fn test_validate_runtime_single_process() {
        let obs = json!({"bridge":{"loaded":true,"version":"0.6.0","session_id":"abc"}});
        assert!(validate_runtime(&obs, &[json!({"pid":1234})], Some(0.5)).is_ok());
    }
    #[test]
    fn test_validate_runtime_wrong_process_count() {
        let obs = json!({"bridge":{"loaded":true,"version":"0.6.0","session_id":"abc"}});
        let err =
            validate_runtime(&obs, &[json!({"pid":1}), json!({"pid":2})], Some(0.5)).unwrap_err();
        assert!(err.0.contains("expected exactly one"));
    }
    #[test]
    fn test_validate_runtime_stale_observation() {
        let obs = json!({"bridge":{"loaded":true,"version":"0.6.0","session_id":"abc"}});
        let err = validate_runtime(&obs, &[json!({"pid":1})], Some(5.0)).unwrap_err();
        assert!(err.0.contains("stale"));
    }
    #[test]
    fn test_validate_runtime_bridge_missing() {
        let obs = json!({});
        let err = validate_runtime(&obs, &[json!({"pid":1})], Some(0.5)).unwrap_err();
        assert!(err.0.contains("bridge missing"));
    }
    #[test]
    fn test_validate_runtime_bridge_not_loaded() {
        let obs = json!({"bridge":{"loaded":false,"version":"0.6.0","session_id":"abc"}});
        let err = validate_runtime(&obs, &[json!({"pid":1})], Some(0.5)).unwrap_err();
        assert!(err.0.contains("not loaded"));
    }
    #[test]
    fn test_validate_runtime_wrong_version() {
        let obs = json!({"bridge":{"loaded":true,"version":"0.5.0","session_id":"abc"}});
        let err = validate_runtime(&obs, &[json!({"pid":1})], Some(0.5)).unwrap_err();
        assert!(err.0.contains("version"));
    }
    #[test]
    fn test_validate_runtime_missing_session() {
        let obs = json!({"bridge":{"loaded":true,"version":"0.6.0"}});
        let err = validate_runtime(&obs, &[json!({"pid":1})], Some(0.5)).unwrap_err();
        assert!(err.0.contains("session_id missing"));
    }
    #[test]
    fn test_validate_seed_matching() {
        let obs = json!({"round":{"seed":"2K9H9HN"},"ready":{}});
        assert!(validate_seed(&obs, Some("2K9H9HN")).is_ok());
    }
    #[test]
    fn test_validate_seed_wrong_requested() {
        let obs = json!({"round":{"seed":"2K9H9HN"},"ready":{}});
        let err = validate_seed(&obs, Some("WRONG")).unwrap_err();
        assert!(err.0.contains("rejected"));
    }
    #[test]
    fn test_validate_seed_wrong_live() {
        let obs = json!({"round":{"seed":"WRONG"},"ready":{}});
        let err = validate_seed(&obs, Some("2K9H9HN")).unwrap_err();
        assert!(err.0.contains("live or saved seed rejected"));
    }
    #[test]
    fn test_validate_seed_no_requested_seed() {
        let obs = json!({"round":{"seed":"2K9H9HN"},"ready":{}});
        assert!(validate_seed(&obs, None).is_ok());
    }
    #[test]
    fn test_guard_command_allowed_action() {
        let cmd = json!({"action":"ui_click","ui_id":"test"});
        let obs = json!({"round":{"seed":"2K9H9HN"},"ready":{}});
        assert!(guard_command(&cmd, &obs).is_ok());
    }
    #[test]
    fn test_guard_command_wrong_seed() {
        let cmd = json!({"action":"ui_click","seed":"WRONG"});
        let obs = json!({"round":{"seed":"2K9H9HN"},"ready":{}});
        let err = guard_command(&cmd, &obs).unwrap_err();
        assert!(err.0.contains("rejected"));
    }
    #[test]
    fn test_guard_command_type_field() {
        let cmd = json!({"type":"ui_click"});
        let obs = json!({"round":{"seed":"2K9H9HN"},"ready":{}});
        assert!(guard_command(&cmd, &obs).is_ok());
    }
    #[test]
    fn test_guard_command_new_run_with_saved() {
        let cmd = json!({"action":"setup_new_run"});
        let obs = json!({"round":{"seed":"2K9H9HN"},"ready":{"saved_game_present":true,"saved_game_seed":"2K9H9HN"}});
        let err = guard_command(&cmd, &obs).unwrap_err();
        assert!(err.0.contains("saved run exists"));
    }
    #[test]
    fn test_guard_command_start_run_with_saved() {
        let cmd = json!({"action":"start_run","seed":"2K9H9HN"});
        let obs = json!({"round":{"seed":"2K9H9HN"},"ready":{"saved_game_present":true,"saved_game_seed":"2K9H9HN"}});
        let err = guard_command(&cmd, &obs).unwrap_err();
        assert!(err.0.contains("saved run exists"));
    }
    #[test]
    fn test_safety_error_display() {
        let err = SafetyError("test error".to_string());
        assert_eq!(format!("{}", err), "safety: test error");
    }
    #[test]
    fn test_safety_error_is_error() {
        let err = SafetyError("test".to_string());
        let _: &dyn std::error::Error = &err;
    }
}
