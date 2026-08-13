mod build_env;
mod sdk;
mod target;
mod toolchain;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use clap::{Args, Parser, Subcommand, ValueEnum};

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
    /// Print the environment and run nothing.
    Env {
        #[command(flatten)]
        options: CliOptions,
        #[arg(long, value_enum, default_value_t = Format::Json)]
        format: Format,
    },
    #[command(external_subcommand)]
    Cargo(Vec<OsString>),
}

#[derive(Args)]
struct CliOptions {
    /// Rust triple (`aarch64-unknown-linux-ohos`) or short arch (`aarch64`, `armv7`, `x86_64`).
    #[arg(short, long)]
    target: Option<String>,
    /// The `native` directory of the OpenHarmony SDK.
    #[arg(long)]
    sdk: Option<PathBuf>,
}

#[derive(Copy, Clone, ValueEnum)]
enum Format {
    Json,
    Sh,
    Powershell,
}

struct Options {
    target: String,
    sdk: Option<PathBuf>,
}

impl TryFrom<CliOptions> for Options {
    type Error = String;

    fn try_from(cli: CliOptions) -> Result<Self, String> {
        let target = cli
            .target
            .or_else(|| std::env::var("CARGO_BUILD_TARGET").ok())
            .ok_or_else(|| {
                "no target given. Pass `-t aarch64` (or set `CARGO_BUILD_TARGET`).".to_owned()
            })?;
        Ok(Self {
            target,
            sdk: cli.sdk,
        })
    }
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

const CARGO_SUBCOMMANDS: &[&str] = &[
    "b", "bench", "build", "c", "check", "clippy", "d", "doc", "fix", "r", "run", "rustc", "t",
    "test",
];

const TEST_RUNNER: &str = "ohos-test-runner";

fn run(cli: Cli) -> Result<ExitCode, String> {
    let args = match cli.command {
        Cmd::Env { options, format } => {
            let options = Options::try_from(options)?;
            let target = Target::parse(&options.target).map_err(|e| e.to_string())?;
            let build_env =
                build_env::derive(target, options.sdk.as_deref()).map_err(|e| e.to_string())?;
            emit(&build_env, format);
            return Ok(ExitCode::SUCCESS);
        }
        Cmd::Cargo(args) => args,
    };
    let (options, rest) = split_cargo_args(args)?;
    let name = match rest.first().map(|s| s.to_string_lossy()) {
        Some(name) if CARGO_SUBCOMMANDS.contains(&name.as_ref()) => name.into_owned(),
        _ => {
            return Err(format!(
                "unsupported cargo subcommand. Supported: {}",
                CARGO_SUBCOMMANDS.join(", ")
            ))
        }
    };

    let target = Target::parse(&options.target).map_err(|e| e.to_string())?;
    let mut build_env =
        build_env::derive(target, options.sdk.as_deref()).map_err(|e| e.to_string())?;

    let runner_var = format!(
        "CARGO_TARGET_{}_RUNNER",
        build_env.target.rust_triple_upper()
    );
    if runs_target_binaries(&name, &rest) && std::env::var_os(&runner_var).is_none() {
        match find_in_path(TEST_RUNNER) {
            Some(runner) => {
                build_env
                    .env
                    .insert(runner_var, runner.to_string_lossy().into_owned());
            }
            None => {
                return Err(format!(
                    "`cargo ohos {name}` runs binaries on a connected device, which needs \
                     `{TEST_RUNNER}`. Install it with `cargo install --locked {TEST_RUNNER}`."
                ))
            }
        }
    }

    let mut argv: Vec<OsString> = vec!["cargo".into()];
    argv.extend(rest);
    // The commandline may contain a `--` seperator, so we insert `--target` directly
    // after the subcommand.
    argv.insert(
        2,
        format!("--target={}", build_env.target.rust_triple).into(),
    );
    spawn(&build_env, &argv)
}

fn runs_target_binaries(name: &str, rest: &[OsString]) -> bool {
    matches!(name, "r" | "run" | "t" | "test" | "bench")
        && !rest
            .iter()
            .any(|arg| arg == "--no-run" || arg == "--help" || arg == "-h")
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| {
        if dir.as_os_str().is_empty() {
            return None;
        }
        let mut candidate = dir.join(name);
        if cfg!(windows) {
            candidate.set_extension("exe");
        }
        candidate.is_file().then_some(candidate)
    })
}

fn split_cargo_args(args: Vec<OsString>) -> Result<(Options, Vec<OsString>), String> {
    let mut rest = Vec::new();
    let mut target = std::env::var("CARGO_BUILD_TARGET").ok();
    let mut sdk = None;

    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let text = arg.to_string_lossy().into_owned();
        // Everything after `--` belongs to cargo or the target binary.
        if text == "--" {
            rest.push(arg);
            rest.extend(it);
            break;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_stops_at_double_dash() {
        let args: Vec<OsString> = [
            "test", "-t", "aarch64", "--", "--target", "foo", "--sdk", "x",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        let (options, rest) = split_cargo_args(args).unwrap();
        assert_eq!(options.target, "aarch64");
        assert_eq!(
            rest,
            ["test", "--", "--target", "foo", "--sdk", "x"].map(OsString::from)
        );
    }
}

fn plain_rustflags(build_env: &BuildEnv) -> Vec<String> {
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
    parts
}

fn encoded_rustflags(build_env: &BuildEnv) -> String {
    plain_rustflags(build_env).join("\u{1f}")
}

fn emit(build_env: &BuildEnv, format: Format) {
    let mut env: BTreeMap<&str, String> = build_env
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    let mut warnings: Vec<String> = Vec::new();
    match format {
        Format::Json => {
            env.insert("CARGO_ENCODED_RUSTFLAGS", encoded_rustflags(build_env));
        }
        Format::Sh | Format::Powershell => {
            env.retain(|key, _| {
                let mut chars = key.chars();
                chars
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
            let flags = plain_rustflags(build_env);
            if flags.iter().any(|f| f.contains(char::is_whitespace)) {
                warnings.push(
                    "A compiler flag contains whitespace, which `RUSTFLAGS` cannot represent. \
                     Use `--format json` and `CARGO_ENCODED_RUSTFLAGS` instead."
                        .to_owned(),
                );
            }
            env.insert("RUSTFLAGS", flags.join(" "));
        }
    }

    match format {
        Format::Json => {
            let value = serde_json::json!({
                "schema_version": 1,
                "sdk": build_env.sdk,
                "target": build_env.target,
                "toolchain": build_env.toolchain,
                "flags": build_env.flags,
                "env": env,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&value).expect("serializable")
            );
        }
        Format::Sh => {
            for warning in &warnings {
                eprintln!("# warning: {warning}");
            }
            for (key, value) in &env {
                println!("export {key}='{}'", value.replace('\'', r"'\''"));
            }
        }
        Format::Powershell => {
            for warning in &warnings {
                eprintln!("# warning: {warning}");
            }
            for (key, value) in &env {
                println!("$env:{key} = '{}'", value.replace('\'', "''"));
            }
        }
    }
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
