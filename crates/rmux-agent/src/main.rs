//! `rmux-agent` — persistent terminal sessions.
//!
//! ```text
//! rmux-agent attach --session api --cwd /srv/api   # create or reattach
//! rmux-agent daemon                                 # the host (started on demand)
//! rmux-agent kill api                               # end a session for good
//! rmux-agent alias api webapp                       # map a display alias to a session
//! rmux-agent list                                   # what is running
//! rmux-agent version
//! ```
//!
//! `attach` is what rmux runs, locally or through `ssh`. It holds no state, so
//! losing the connection leaves the shell running.

use std::process::ExitCode;

use rmux_agent::{Hello, attach, daemon};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");

    match command {
        "daemon" => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "rmux_agent=info".into()),
                )
                .init();

            match daemon::serve().await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("rmux-agent: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        "attach" => {
            let hello = match parse_attach(&args[1..]) {
                Ok(hello) => hello,
                Err(e) => {
                    eprintln!("rmux-agent: {e}");
                    return ExitCode::FAILURE;
                }
            };
            match attach::attach(hello, true).await {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("rmux-agent: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        "kill" => {
            let name = args.get(1).cloned().unwrap_or_default();
            if name.is_empty() {
                eprintln!("rmux-agent: kill needs a session name");
                return ExitCode::FAILURE;
            }
            match attach::kill(&name).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("rmux-agent: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        "alias" => {
            let key = args.get(1).cloned().unwrap_or_default();
            let alias = args.get(2).cloned().unwrap_or_default();
            if key.is_empty() || alias.is_empty() {
                eprintln!("rmux-agent: alias needs a session name and an alias");
                return ExitCode::FAILURE;
            }
            match attach::alias(&key, &alias).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("rmux-agent: {e}");
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
                eprintln!("rmux-agent: {e}");
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
                    eprintln!("rmux-agent: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        // Used to decide whether an uploaded binary needs replacing.
        // What is this daemon running? Printed as one NUL-free line per
        // session so a caller can split on newlines and tabs — the same
        // reasoning as `rmux-fs`, except a session name is ours and cannot
        // contain either. The alias column is a display name mapped to the key
        // by `alias`; a dash means none.
        "list" => {
            match attach::list().await {
                Ok(sessions) => {
                    for s in sessions {
                        println!(
                            "{}\t{}\t{}\t{}\t{}\t{}",
                            s.name,
                            s.alias.as_deref().unwrap_or("-"),
                            s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                            s.age_seconds,
                            if s.attached { "attached" } else { "detached" },
                            s.command.unwrap_or_default(),
                        );
                    }
                    std::process::ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("rmux-agent: {e}");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        "version" => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }

        _ => {
            eprintln!("usage: rmux-agent <attach|daemon|kill|alias|list|setenv|version> [--session NAME] [--cwd DIR]");
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
