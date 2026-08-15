use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use jjsync::config::{self, Config, RepoConfig};
use jjsync::cycle::{sync_repo, CycleOpts};
use jjsync::exec::Env;
use jjsync::report::{self, notify_new_problems, Report};

#[derive(Parser)]
#[command(
    name = "jjsync",
    about = "Background synchronization for Jujutsu repositories"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Register the current repo (or workspace) in the config and install the timer
    Init,
    /// Run one sync cycle for all repos (what the timer fires)
    Sync,
    /// Per-repo state on one line each, plus last-sync age and timer health
    Status,
    /// Unfreeze a diverged bookmark: the local position wins
    Resolve { bookmark: String },
    /// Stop the timer
    Pause,
    /// Start the timer
    Resume,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Init => init(),
        Command::Sync => sync(vec![]),
        Command::Status => status(),
        Command::Resolve { bookmark } => sync(vec![bookmark]),
        Command::Pause => jjsync::systemd::pause(&Env::default()),
        Command::Resume => jjsync::systemd::resume(&Env::default()),
    }
}

fn sync(resolve: Vec<String>) -> Result<()> {
    let cfg = Config::load(&config::config_path())?;
    if cfg.repos.is_empty() {
        bail!("no repos configured — run `jjsync init` inside a repo first");
    }
    let opts = CycleOpts {
        resolve,
        ..Default::default()
    };
    let report = Report {
        when: report::unix_now(),
        repos: cfg
            .repos
            .iter()
            .map(|repo| sync_repo(repo, &opts))
            .collect(),
    };
    let state = config::state_path();
    let prev = Report::load(&state);
    notify_new_problems(prev.as_ref(), &report, &Env::default());
    report.save(&state)?;
    // Successful sync is silent; problems live in `jjsync status`.
    Ok(())
}

fn status() -> Result<()> {
    let cfg = Config::load(&config::config_path())?;
    let configured: Vec<String> = cfg.repos.iter().map(|r| r.name()).collect();
    let report = Report::load(&config::state_path()).unwrap_or_default();
    print!("{}", report.render(&configured, report::unix_now()));
    if let Some(line) = jjsync::systemd::timer_status(cfg.interval_seconds, &Env::default()) {
        println!("{line}");
    }
    Ok(())
}

fn init() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = config::find_jj_root(&cwd).context("not inside a jj repo (no .jj found)")?;
    let cfg_path = config::config_path();
    let mut cfg = Config::load(&cfg_path)?;
    let env = Env::default();

    // Running init inside a secondary workspace registers the main repo —
    // workspaces are auto-discovered every cycle via `jj workspace root`.
    let main = if root.join(".git").exists() {
        root
    } else {
        config::workspace_main_repo(&root)?
    };
    if !main.join(".git").exists() {
        bail!(
            "{} is not colocated (no .git beside .jj) — jjsync requires colocated repos",
            main.display()
        );
    }
    if cfg.find_repo_mut(&main).is_some() {
        println!("{} is already registered", main.display());
    } else {
        cfg.repos.push(RepoConfig {
            path: main.display().to_string(),
            ..Default::default()
        });
        println!("registered {}", main.display());
    }

    cfg.save(&cfg_path)?;
    jjsync::systemd::install(cfg.interval_seconds, &env)?;
    Ok(())
}
