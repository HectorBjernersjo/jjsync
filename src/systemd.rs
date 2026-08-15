//! systemd user timer + oneshot service. The timer is the debounce; there is
//! no daemon. `pause`/`resume` are thin wrappers around systemctl.

use crate::exec::{run, Env};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

fn unit_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::config::expand_tilde("~/.config"));
    base.join("systemd/user")
}

fn systemd_disabled() -> bool {
    std::env::var_os("JJSYNC_NO_SYSTEMD").is_some()
}

pub fn install(interval_seconds: u64, env: &Env) -> Result<()> {
    if systemd_disabled() {
        return Ok(());
    }
    let exe = std::env::current_exe().context("cannot locate the jjsync binary")?;
    let dir = unit_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("jjsync.service"),
        format!(
            "[Unit]\nDescription=jjsync sync cycle\n\n[Service]\nType=oneshot\n\
             Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin\n\
             ExecStart={} sync\n",
            exe.display()
        ),
    )?;
    std::fs::write(
        dir.join("jjsync.timer"),
        format!(
            "[Unit]\nDescription=jjsync periodic sync\n\n[Timer]\nOnBootSec=60\n\
             OnUnitActiveSec={interval_seconds}s\n\n[Install]\nWantedBy=timers.target\n"
        ),
    )?;
    systemctl(&["daemon-reload"], env)?;
    systemctl(&["enable", "--now", "jjsync.timer"], env)?;
    Ok(())
}

/// One-line timer health for `jjsync status`. None when systemd is disabled
/// or systemctl is unavailable.
pub fn timer_status(interval_seconds: u64, env: &Env) -> Option<String> {
    if systemd_disabled() {
        return None;
    }
    let out = run(
        "systemctl",
        &["--user", "is-active", "jjsync.timer"],
        Path::new("/"),
        env,
    )
    .ok()?;
    let state = out.stdout.trim();
    Some(if state == "active" {
        format!("timer: active (every {interval_seconds}s)")
    } else {
        format!("timer: {state} — run `jjsync resume`")
    })
}

pub fn pause(env: &Env) -> Result<()> {
    systemctl(&["stop", "jjsync.timer"], env)
}

pub fn resume(env: &Env) -> Result<()> {
    systemctl(&["start", "jjsync.timer"], env)
}

fn systemctl(args: &[&str], env: &Env) -> Result<()> {
    if systemd_disabled() {
        return Ok(());
    }
    let mut full = vec!["--user"];
    full.extend_from_slice(args);
    let out = run("systemctl", &full, Path::new("/"), env)?;
    if !out.ok() {
        eprintln!(
            "warning: systemctl --user {} failed: {}",
            args.join(" "),
            out.stderr.trim()
        );
    }
    Ok(())
}
