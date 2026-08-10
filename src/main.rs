mod build_env;
mod sdk;
mod target;
mod toolchain;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};

use build_env::BuildEnv;
use target::Target;

#[derive(Parser)]
#[command(
    name = "cargo-ohos",
    bin_name = "cargo ohos",
    version,
    about,
    disable_help_subcommand = true
)]
struct Cli {
    #[arg(value_parser = ["ohos"], hide = true)]
    _subcommand_name: Option<String>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(external_subcommand)]
    Cargo(Vec<OsString>),
}

struct Options {
    target: String,
    sdk: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let Cmd::Cargo(args) = cli.command;
    let (options, rest) = split_cargo_args(args)?;
    match rest.first().map(|s| s.to_string_lossy()) {
        Some(name) if name == "build" => {}
        _ => return Err("only `cargo ohos build` is supported".to_owned()),
    }

    let target = Target::parse(&options.target).map_err(|e| e.to_string())?;
    let build_env = build_env::derive(target, options.sdk.as_deref()).map_err(|e| e.to_string())?;

    let mut argv: Vec<OsString> = vec!["cargo".into()];
    argv.extend(rest);
    argv.push(format!("--target={}", build_env.target.rust_triple).into());
    spawn(&build_env, &argv)
}

fn split_cargo_args(args: Vec<OsString>) -> Result<(Options, Vec<OsString>), String> {
    let mut rest = Vec::new();
    let mut target = std::env::var("CARGO_BUILD_TARGET").ok();
    let mut sdk = None;

    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let text = arg.to_string_lossy().into_owned();
        let mut take = |name: &str| -> Result<OsString, String> {
            it.next()
                .ok_or_else(|| format!("missing value after `{name}`"))
        };
        match text.as_str() {
            "-t" | "--target" => target = Some(take("--target")?.to_string_lossy().into_owned()),
            "--sdk" => sdk = Some(PathBuf::from(take("--sdk")?)),
            _ => {
                if let Some(v) = text.strip_prefix("--target=") {
                    target = Some(v.to_owned());
                } else {
                    rest.push(arg);
                }
            }
        }
    }

    let target = target.ok_or_else(|| {
        "no target given. Pass `-t aarch64` (or set $CARGO_BUILD_TARGET).".to_owned()
    })?;
    Ok((Options { target, sdk }, rest))
}

fn encoded_rustflags(build_env: &BuildEnv) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Ok(encoded) = std::env::var("CARGO_ENCODED_RUSTFLAGS") {
        parts.extend(
            encoded
                .split('\u{1f}')
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        );
    } else if let Ok(plain) = std::env::var("RUSTFLAGS") {
        parts.extend(plain.split_whitespace().map(str::to_owned));
    }
    parts.extend(build_env.flags.rustflags.iter().cloned());
    parts.join("\u{1f}")
}

fn spawn(build_env: &BuildEnv, argv: &[OsString]) -> Result<ExitCode, String> {
    let (program, args) = argv.split_first().ok_or("no command given")?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.envs(&build_env.env);
    cmd.env("CARGO_ENCODED_RUSTFLAGS", encoded_rustflags(build_env));
    cmd.env_remove("RUSTFLAGS");

    let status = cmd
        .status()
        .map_err(|e| format!("could not run `{}`: {e}", program.to_string_lossy()))?;
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(code.min(255) as u8),
        None => ExitCode::FAILURE,
    })
}
