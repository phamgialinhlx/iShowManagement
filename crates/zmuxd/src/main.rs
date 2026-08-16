//! `zmuxd` — persistent terminal sessions.
//!
//! ```text
//! zmuxd attach --session api --cwd /srv/api   # create or reattach
//! zmuxd daemon                                 # the host (started on demand)
//! zmuxd version
//! ```
//!
//! `attach` is what zmux runs, locally or through `ssh`. It holds no state, so
//! losing the connection leaves the shell running.

use std::process::ExitCode;

use zmuxd::{Hello, alias, attach, daemon, status};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");

    match command {
        "daemon" => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "zmuxd=info".into()),
                )
                .init();

            match daemon::serve().await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("zmuxd: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        "attach" => {
            let mut hello = match parse_attach(&args[1..]) {
                Ok(hello) => hello,
                Err(e) => {
                    eprintln!("zmuxd: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // **Resolved here, before `Hello`.** That is what keeps aliases out
            // of the wire protocol entirely: the daemon only ever sees session
            // keys, so an older daemon needs no changes to serve a renamed
            // session. A name that is not an alias passes through unchanged.
            hello.session = alias::resolve(&hello.session);
            match attach::attach(hello, true).await {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("zmuxd: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        "kill" => {
            let name = args.get(1).map(|n| alias::resolve(n)).unwrap_or_default();
            if name.is_empty() {
                eprintln!("zmuxd: kill needs a session name");
                return ExitCode::FAILURE;
            }
            match attach::kill(&name).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("zmuxd: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        // Reads `KEY=VALUE` lines from stdin. Never takes them as arguments —
        // see `attach::set_env`.
        "setenv" => {
            use std::io::Read;
            let mut input = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut input) {
                eprintln!("zmuxd: {e}");
                return ExitCode::FAILURE;
            }

            let pairs: std::collections::BTreeMap<String, String> = input
                .lines()
                .filter_map(|line| line.split_once('='))
                .map(|(k, v)| (k.trim().to_owned(), v.to_owned()))
                .filter(|(k, _)| !k.is_empty())
                .collect();

            match attach::set_env(pairs).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("zmuxd: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        // Used to decide whether an uploaded binary needs replacing.
        // What is this daemon running? Printed as one NUL-free line per
        // session so a caller can split on newlines and tabs — the same
        // reasoning as `zmux-fs`, except a session name is ours and cannot
        // contain either.
        "list" => {
            match attach::list().await {
                Ok(sessions) => {
                    // Joined from the file rather than carried on the wire, so
                    // sessions held by an older daemon still show their names.
                    let aliases = alias::load();
                    for s in sessions {
                        println!(
                            "{}\t{}\t{}\t{}\t{}\t{}",
                            s.name,
                            aliases.get(&s.name).cloned().unwrap_or_else(|| "-".into()),
                            s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                            s.age_seconds,
                            if s.attached { "attached" } else { "detached" },
                            s.command.unwrap_or_default(),
                        );
                    }
                    std::process::ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("zmuxd: {e}");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        // Stream Claude session status changes as NDJSON on stdout. Replaces the
        // client's per-pane screen-scrape poll with an event stream — see
        // `status.rs`. An older agent lacks this arm and exits via the usage
        // branch below, which is exactly how the client detects the fallback.
        "watch-status" => match status::watch_status().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("zmuxd: {e}");
                ExitCode::FAILURE
            }
        },

        // Rename a running session. Writes the host-side alias file; no daemon
        // is contacted, which is why this works across agent builds.
        "alias" => {
            let (Some(key), Some(name)) = (args.get(1), args.get(2)) else {
                eprintln!("zmuxd: alias needs a session name and a new name");
                return ExitCode::FAILURE;
            };
            // The live set decides both whether the target exists and whether
            // the new name would shadow something. Listing unions every daemon,
            // so a session held by another build still counts.
            let live: Vec<String> = match attach::list().await {
                Ok(sessions) => sessions.into_iter().map(|s| s.name).collect(),
                Err(e) => {
                    eprintln!("zmuxd: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // Resolved so renaming twice works: the second rename names the
            // session by the alias the first one gave it.
            match alias::set(&alias::resolve(key), name, &live) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("zmuxd: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        "version" => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }

        _ => {
            eprintln!("usage: zmuxd <attach|alias|daemon|kill|list|setenv|watch-status|version> [--session NAME] [--cwd DIR]");
            ExitCode::FAILURE
        }
    }
}

fn parse_attach(args: &[String]) -> anyhow::Result<Hello> {
    let mut hello = Hello {
        session: String::new(),
        cwd: None,
        program: None,
        args: Vec::new(),
        login_command: None,
        env: std::collections::BTreeMap::new(),
        cols: 80,
        rows: 24,
    };

    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let mut value = || -> anyhow::Result<String> {
            i += 1;
            args.get(i).cloned().ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))
        };

        match flag {
            "--session" => hello.session = value()?,
            "--cwd" => hello.cwd = Some(value()?),
            "--program" => hello.program = Some(value()?),
            "--login-command" => hello.login_command = Some(value()?),
            "--cols" => hello.cols = value()?.parse()?,
            "--rows" => hello.rows = value()?.parse()?,
            other => anyhow::bail!("unknown option {other}"),
        }
        i += 1;
    }

    anyhow::ensure!(!hello.session.is_empty(), "--session is required");
    Ok(hello)
}
