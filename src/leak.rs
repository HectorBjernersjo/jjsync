//! The leak gate: every publish must pass a gitleaks scan of the outgoing
//! commits. Fail closed — a missing or broken gitleaks blocks publishing.

use crate::exec::{run, Env};
use anyhow::{bail, Result};
use std::path::Path;

pub enum ScanResult {
    Clean,
    Hit(Vec<String>),
}

/// Scan exactly `shas` (the commits the remote does not have yet).
pub fn scan(repo: &Path, shas: &[String], env: &Env) -> Result<ScanResult> {
    if shas.is_empty() {
        return Ok(ScanResult::Clean);
    }
    let report = std::env::temp_dir().join(format!(
        "jjsync-gitleaks-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let log_opts = format!("--no-walk {}", shas.join(" "));
    let report_str = report.to_string_lossy().into_owned();
    let out = run(
        "gitleaks",
        &[
            "git",
            "--no-banner",
            "--exit-code=3",
            "--report-format=json",
            &format!("--report-path={report_str}"),
            &format!("--log-opts={log_opts}"),
            ".",
        ],
        repo,
        env,
    );
    let result = match out {
        Ok(o) if o.status == 0 => Ok(ScanResult::Clean),
        Ok(o) if o.status == 3 => {
            let mut files: Vec<String> = std::fs::read_to_string(&report)
                .ok()
                .and_then(|text| serde_json::from_str::<Vec<serde_json::Value>>(&text).ok())
                .map(|findings| {
                    findings
                        .iter()
                        .filter_map(|f| f.get("File").and_then(|v| v.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            files.sort();
            files.dedup();
            Ok(ScanResult::Hit(files))
        }
        Ok(o) => bail!("gitleaks failed: {}", o.stderr.trim()),
        Err(e) => bail!("could not run gitleaks (is it installed?): {e}"),
    };
    let _ = std::fs::remove_file(&report);
    result
}
