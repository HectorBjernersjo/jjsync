use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use jjsync::config::{self, Config, RepoConfig};
use jjsync::cycle::{sync_repo, CycleOpts};
use jjsync::exec::{run, Env};
use jjsync::report::{self, notify_new_problems, Report};
use std::path::Path;

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
    Init {
        /// Register in config.local.json — this machine only, never synced
        #[arg(long)]
        local: bool,
    },
    /// Clone configured repos missing on this machine and install the timer
    Bootstrap,
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
        Command::Init { local } => init(local),
        Command::Bootstrap => bootstrap(),
        Command::Sync => sync(vec![]),
        Command::Status => status(),
        Command::Resolve { bookmark } => sync(vec![bookmark]),
        Command::Pause => jjsync::systemd::pause(&Env::default()),
        Command::Resume => jjsync::systemd::resume(&Env::default()),
    }
}

fn sync(resolve: Vec<String>) -> Result<()> {
    let (shared, local) = Config::load_both()?;
    let repos = config::effective_repos(&shared, &local);
    if repos.is_empty() {
        bail!("no repos configured — run `jjsync init` inside a repo first");
    }
    let opts = CycleOpts {
        resolve,
        ..Default::default()
    };
    let report = Report {
        when: report::unix_now(),
        repos: repos
            .iter()
            // Not cloned here yet (bootstrap pending): a non-event, like offline.
            .filter(|repo| repo.expanded_path().is_dir())
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
    let (shared, local) = Config::load_both()?;
    let repos = config::effective_repos(&shared, &local);
    let configured: Vec<String> = repos.iter().map(|r| r.name()).collect();
    let not_cloned: Vec<String> = repos
        .iter()
        .filter(|r| !r.expanded_path().is_dir())
        .map(|r| r.name())
        .collect();
    let env = Env::default();
    let pending: std::collections::BTreeMap<String, Vec<String>> = repos
        .iter()
        .map(|r| {
            let items = jjsync::cycle::pending_items(r, &env).unwrap_or_else(|e| {
                eprintln!("warning: pending check failed for {}: {e:#}", r.name());
                vec![]
            });
            (r.name(), items)
        })
        .collect();
    let mut report = Report::load(&config::state_path()).unwrap_or_default();
    // Stale report entries for repos since removed or ignored don't render.
    report.repos.retain(|r| configured.contains(&r.repo));
    print!(
        "{}",
        report.render(&configured, &not_cloned, &pending, report::unix_now())
    );
    if let Some(line) = jjsync::systemd::timer_status(shared.interval_seconds, &Env::default()) {
        println!("{line}");
    }
    Ok(())
}

fn init(register_local: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = config::find_jj_root(&cwd).context("not inside a jj repo (no .jj found)")?;
    let (mut shared, mut local) = Config::load_both()?;
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

    let registered_in = [
        (&mut shared, config::config_path()),
        (&mut local, config::local_config_path()),
    ]
    .into_iter()
    .find(|(cfg, _)| cfg.repos.iter().any(|r| r.expanded_path() == main));
    if let Some((cfg, path)) = registered_in {
        println!("{} is already registered", main.display());
        let repo = cfg.find_repo_mut(&main).unwrap();
        if repo.url.is_none() {
            repo.url = remote_url(&main, &repo.remote.clone(), &env);
            cfg.save(&path)?;
        }
    } else {
        let repo = RepoConfig {
            path: main.display().to_string(),
            url: remote_url(&main, "origin", &env),
            ..Default::default()
        };
        if repo.url.is_none() {
            eprintln!(
                "warning: no URL for remote 'origin' — `jjsync bootstrap` won't be able \
                 to clone this repo on other machines"
            );
        }
        let (cfg, path, note) = if register_local {
            (
                &mut local,
                config::local_config_path(),
                " (this machine only)",
            )
        } else {
            (&mut shared, config::config_path(), "")
        };
        cfg.repos.push(repo);
        cfg.save(&path)?;
        println!("registered {}{note}", main.display());
    }

    jjsync::systemd::install(shared.interval_seconds, &env)?;
    Ok(())
}

/// Clone every configured repo whose path doesn't exist on this machine —
/// the one command a fresh machine needs once the config is in place.
fn bootstrap() -> Result<()> {
    let (shared, local) = Config::load_both()?;
    let repos = config::effective_repos(&shared, &local);
    if repos.is_empty() {
        bail!("no repos configured — run `jjsync init` inside a repo first");
    }
    let env = Env::default();
    let (mut cloned, mut failed) = (0u32, 0u32);
    for repo in &repos {
        let path = repo.expanded_path();
        if path.exists() {
            continue;
        }
        let Some(url) = &repo.url else {
            eprintln!(
                "skipping {}: no url in config — run `jjsync init` in it on a machine that has it",
                repo.name()
            );
            failed += 1;
            continue;
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        println!("cloning {url} → {}", path.display());
        let dest = path.display().to_string();
        let out = run(
            "jj",
            &[
                "git",
                "clone",
                "--colocate",
                "--remote",
                &repo.remote,
                url,
                &dest,
            ],
            path.parent().unwrap_or(Path::new("/")),
            &env,
        )?;
        if out.ok() {
            cloned += 1;
        } else {
            eprintln!("failed to clone {}: {}", repo.name(), out.stderr.trim());
            failed += 1;
        }
    }
    jjsync::systemd::install(shared.interval_seconds, &env)?;
    println!(
        "{cloned} cloned, {} already present",
        repos.len() as u32 - cloned - failed
    );
    if failed > 0 {
        bail!("{failed} repo(s) could not be cloned");
    }
    Ok(())
}

fn remote_url(dir: &Path, remote: &str, env: &Env) -> Option<String> {
    run("git", &["remote", "get-url", remote], dir, env)
        .ok()
        .filter(|o| o.ok())
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty())
}
