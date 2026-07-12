use serde_json::json;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    panic,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn install(path: PathBuf) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default();
            let _ = writeln!(file, "\n=== MCP PANIC {timestamp} ===");
            let _ = writeln!(file, "{info}");
            let _ = writeln!(
                file,
                "backtrace: {}",
                std::backtrace::Backtrace::force_capture()
            );
        }
        previous(info);
    }));
}

pub fn tail(path: &Path, lines: u32) -> serde_json::Value {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return json!({"log_found": false, "tail": "No MCP crash log found"}),
    };
    let mut text = String::new();
    if file.read_to_string(&mut text).is_err() {
        return json!({"log_found": false, "tail": "MCP crash log could not be read"});
    }
    let tail = text
        .lines()
        .rev()
        .take(lines.clamp(1, 200) as usize)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    json!({"log_found": true, "tail": tail, "truncated": text.lines().count() > lines as usize})
}
