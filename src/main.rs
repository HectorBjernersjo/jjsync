use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use jjsync::config::{self, Config, RepoConfig};
use jjsync::cycle::{sync_repo, CycleOpts};
use jjsync::exec::Env;
use jjsync::jj::Jj;
use jjsync::report::{notify_new_problems, Report};

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
    /// Per-repo state; one line per problem, ✓ when healthy
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
    let state = config::state_path();
    match Report::load(&state) {
        Some(report) => print!("{}", report.render()),
        None => println!("no sync has run yet"),
    }
    Ok(())
}

fn init() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = config::find_jj_root(&cwd).context("not inside a jj repo (no .jj found)")?;
    let cfg_path = config::config_path();
    let mut cfg = Config::load(&cfg_path)?;
    let env = Env::default();

    if root.join(".git").exists() {
        // Colocated main repo.
        if cfg.find_repo_mut(&root).is_some() {
            println!("{} is already registered", root.display());
        } else {
            cfg.repos.push(RepoConfig {
                path: root.display().to_string(),
                ..Default::default()
            });
            println!("registered {}", root.display());
        }
    } else {
        // A workspace directory: register its path under the main repo entry.
        let main = config::workspace_main_repo(&root)?;
        if !main.join(".git").exists() {
            bail!(
                "{} is not colocated (no .git beside .jj) — jjsync requires colocated repos",
                main.display()
            );
        }
        let jj = Jj::new(&root, &env);
        let wc = jj.wc_commit()?;
        let workspaces = jj.workspaces()?;
        let name = workspaces
            .iter()
            .find(|(_, t)| *t == wc)
            .map(|(n, _)| n.clone())
            .context("could not determine this workspace's name")?;
        let repo = cfg.find_repo_mut(&main).with_context(|| {
            format!(
                "main repo {} is not registered — run `jjsync init` there first",
                main.display()
            )
        })?;
        repo.workspaces
            .insert(name.clone(), root.display().to_string());
        println!("registered workspace '{name}' of {}", main.display());
    }

    cfg.save(&cfg_path)?;
    jjsync::systemd::install(cfg.interval_seconds, &env)?;
    Ok(())
}
