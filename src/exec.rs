//! Child-process plumbing shared by the git and jj wrappers.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

/// Extra environment variables applied to every child process. Tests use this
/// to point HOME/XDG_* into a tempdir so real user config never leaks in.
#[derive(Clone, Default)]
pub struct Env {
    pub vars: Vec<(String, String)>,
}

pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

pub fn run(program: &str, args: &[&str], cwd: &Path, env: &Env) -> Result<Output> {
    run_with_stdin(program, args, cwd, env, None)
}

pub fn run_with_stdin(
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &Env,
    stdin: Option<&str>,
) -> Result<Output> {
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(cwd);
    for (k, v) in &env.vars {
        cmd.env(k, v);
    }
    let out = if let Some(input) = stdin {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to run {program}"))?;
        use std::io::Write;
        child.stdin.take().unwrap().write_all(input.as_bytes())?;
        child.wait_with_output()?
    } else {
        cmd.output()
            .with_context(|| format!("failed to run {program}"))?
    };
    Ok(Output {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Like `run`, but a non-zero exit is an error.
pub fn run_ok(program: &str, args: &[&str], cwd: &Path, env: &Env) -> Result<Output> {
    let out = run(program, args, cwd, env)?;
    if !out.ok() {
        bail!(
            "`{program} {}` failed: {}",
            args.join(" "),
            out.stderr.trim()
        );
    }
    Ok(out)
}
