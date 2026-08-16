//! `rmux-agent` — persistent terminal sessions.
//!
//! ```text
//! rmux-agent attach --session api --cwd /srv/api   # create or reattach
//! rmux-agent daemon                                 # the host (started on demand)
//! rmux-agent version
//! ```
//!
//! `attach` is what rmux runs, locally or through `ssh`. It holds no state, so
//! losing the connection leaves the shell running.

use std::process::ExitCode;

use rmux_agent::{Hello, alias, attach, daemon, status};

#[tokio::main]
async fn main() -> ExitCode {
    // **Say which subcommand died.** The agent has crashed on its main thread
    // more than once with nothing but a stripped macOS crash report to show for
    // it, and `daemon`, `attach`, `watch-status` and `list` are completely
    // different code paths that produce an identical report. stderr on the far
    // side of `ssh -tt` is interleaved into a pty nobody is reading, so this
    // prints somewhere the operator can actually reach.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");
    install_panic_reporter(command);

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
            let mut hello = match parse_attach(&args[1..]) {
                Ok(hello) => hello,
                Err(e) => {
                    eprintln!("rmux-agent: {e}");
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
                    eprintln!("rmux-agent: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        "kill" => {
            let name = args.get(1).map(|n| alias::resolve(n)).unwrap_or_default();
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

        // The Redstone bridge: an outbound WebSocket that lets Redstone drive
        // this host's Claude sessions. Enrolled by rmux, which writes
        // `~/.rmux/redstone.json`; refuses with a reason when it is not there.
        //
        // Logs like the daemon rather than printing a protocol on stdout, because
        // there is nobody reading its stdout — it is started detached and lives
        // for weeks.
        "bridge" => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "rmux_agent=info".into()),
                )
                .init();

            match rmux_agent::bridge::run().await {
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
                    eprintln!("rmux-agent: {e}");
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
                eprintln!("rmux-agent: {e}");
                ExitCode::FAILURE
            }
        },

        // Rename a running session. Writes the host-side alias file; no daemon
        // is contacted, which is why this works across agent builds.
        "alias" => {
            let (Some(key), Some(name)) = (args.get(1), args.get(2)) else {
                eprintln!("rmux-agent: alias needs a session name and a new name");
                return ExitCode::FAILURE;
            };
            // The live set decides both whether the target exists and whether
            // the new name would shadow something. Listing unions every daemon,
            // so a session held by another build still counts.
            let live: Vec<String> = match attach::list().await {
                Ok(sessions) => sessions.into_iter().map(|s| s.name).collect(),
                Err(e) => {
                    eprintln!("rmux-agent: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // Resolved so renaming twice works: the second rename names the
            // session by the alias the first one gave it.
            match alias::set(&alias::resolve(key), name, &live) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("rmux-agent: {e}");
                    ExitCode::FAILURE
                }
            }
        }

        "version" => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }

        _ => {
            eprintln!("usage: rmux-agent <attach|alias|daemon|kill|list|setenv|watch-status|version> [--session NAME] [--cwd DIR]");
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


/// Name the panic on the way down.
///
/// The agent aborts on panic (`panic = "abort"` is inherited from the release
/// profile), so a panic is a SIGABRT and macOS writes a crash report against a
/// stripped binary — which symbolicates to nothing. Two such reports arrived
/// with byte-identical stacks and neither could be attributed to a line.
///
/// This costs one line of stderr and turns that into an answer.
fn install_panic_reporter(command: &str) {
    let command = command.to_owned();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic payload>");
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_owned());
        let thread = std::thread::current();
        let who = thread.name().unwrap_or("<unnamed>").to_owned();

        // `eprintln!` and not `println!`: stdout is the protocol on some of
        // these subcommands, and injecting a panic line into a frame stream
        // would corrupt the very thing being debugged.
        eprintln!(
            "rmux-agent panic: {message}\n  subcommand: {command}\n  at: {location}\n  thread: {who}"
        );
        previous(info);
    }));
}
