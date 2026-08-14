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

#[derive(Args, Default)]
struct CliOptions {
    /// Rust triple (`aarch64-unknown-linux-ohos`) or short arch (`aarch64`, `armv7`, `x86_64`).
    /// Defaults to `$CARGO_BUILD_TARGET`, or `aarch64-unknown-linux-ohos`.
    #[arg(short, long)]
    target: Option<String>,
    /// The `native` directory of the OpenHarmony SDK.
    #[arg(long)]
    sdk: Option<PathBuf>,
    /// An OpenHarmony LLVM toolchain to use instead of the SDK's: the `llvm`
    /// directory of an unpacked prebuilt (see openharmony-rs/ohos-llvm-toolchains).
    /// The sysroot still comes from the SDK. Defaults to `$OHOS_LLVM`.
    #[arg(long)]
    llvm: Option<PathBuf>,
    /// With --llvm, keep the target flags in `TARGET_CFLAGS`/`TARGET_CXXFLAGS`
    /// instead of folding them into the `CC`/`CXX` values. The default folds
    /// them in so that build systems which re-synthesize compiler command
    /// lines (autoconf probes, SpiderMonkey's moz.configure) still target OHOS.
    #[arg(long)]
    no_inline_flags: bool,
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
    llvm: Option<PathBuf>,
    no_inline_flags: bool,
}

impl TryFrom<CliOptions> for Options {
    type Error = String;

    fn try_from(cli: CliOptions) -> Result<Self, String> {
        Ok(Self {
            target: resolve_target(cli.target),
            sdk: cli.sdk,
            llvm: cli
                .llvm
                .or_else(|| std::env::var_os("OHOS_LLVM").map(PathBuf::from)),
            no_inline_flags: cli.no_inline_flags,
        })
    }
}

impl Options {
    fn derive_build_env(&self) -> Result<BuildEnv, String> {
        let target = Target::parse(&self.target).map_err(|e| e.to_string())?;
        let mut config = build_env::Config::new(target);
        config.sdk = self.sdk.clone();
        config.llvm = self.llvm.clone();
        config.no_inline_flags = self.no_inline_flags;
        build_env::derive(&config).map_err(|e| e.to_string())
    }
}

const DEFAULT_TARGET: &str = "aarch64-unknown-linux-ohos";

fn resolve_target(explicit: Option<String>) -> String {
    explicit
        .or_else(|| std::env::var("CARGO_BUILD_TARGET").ok())
        .unwrap_or_else(|| {
            // stderr: the stdout of `env --format sh` is meant to be eval'd.
            eprintln!(
                "note: no target given, defaulting to {DEFAULT_TARGET} \
                 (override with --target or $CARGO_BUILD_TARGET)"
            );
            DEFAULT_TARGET.to_owned()
        })
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
            let build_env = options.derive_build_env()?;
            emit(&build_env, format);
            return Ok(ExitCode::SUCCESS);
        }
        Cmd::Cargo(args) => args,
    };
    let (options, rest) = split_cargo_args(args)?;
    let options = Options::try_from(options)?;
    let name = match rest.first().map(|s| s.to_string_lossy()) {
        Some(name) if CARGO_SUBCOMMANDS.contains(&name.as_ref()) => name.into_owned(),
        _ => {
            return Err(format!(
                "unsupported cargo subcommand. Supported: {}",
                CARGO_SUBCOMMANDS.join(", ")
            ))
        }
    };

    let mut build_env = options.derive_build_env()?;

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

fn split_cargo_args(args: Vec<OsString>) -> Result<(CliOptions, Vec<OsString>), String> {
    let mut rest = Vec::new();
    let mut options = CliOptions::default();

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
            "-t" | "--target" => {
                options.target = Some(take("--target")?.to_string_lossy().into_owned())
            }
            "--sdk" => options.sdk = Some(PathBuf::from(take("--sdk")?)),
            "--llvm" => options.llvm = Some(PathBuf::from(take("--llvm")?)),
            "--no-inline-flags" => options.no_inline_flags = true,
            _ => {
                if let Some(v) = text.strip_prefix("--target=") {
                    options.target = Some(v.to_owned());
                } else {
                    rest.push(arg);
                }
            }
        }
    }

    Ok((options, rest))
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

fn resolved_env(build_env: &BuildEnv) -> BTreeMap<String, String> {
    let mut env = build_env.env.clone();
    env.insert(
        "CARGO_ENCODED_RUSTFLAGS".to_owned(),
        encoded_rustflags(build_env),
    );
    env
}

fn emit(build_env: &BuildEnv, format: Format) {
    let mut env = resolved_env(build_env);
    let mut warnings: Vec<String> = Vec::new();
    match format {
        Format::Json => {}
        Format::Sh | Format::Powershell => {
            env.remove("CARGO_ENCODED_RUSTFLAGS");
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
            env.insert("RUSTFLAGS".to_owned(), flags.join(" "));
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
    cmd.envs(resolved_env(build_env));
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
        assert_eq!(options.target.as_deref(), Some("aarch64"));
        assert_eq!(
            rest,
            ["test", "--", "--target", "foo", "--sdk", "x"].map(OsString::from)
        );
    }

    #[test]
    fn split_takes_llvm_and_inline_options() {
        let args: Vec<OsString> = ["build", "--llvm", "/toolchains/llvm", "--no-inline-flags"]
            .iter()
            .map(OsString::from)
            .collect();
        let (options, rest) = split_cargo_args(args).unwrap();
        assert_eq!(
            options.llvm.as_deref(),
            Some(std::path::Path::new("/toolchains/llvm"))
        );
        assert!(options.no_inline_flags);
        assert_eq!(rest, [OsString::from("build")]);
    }

    #[test]
    fn default_target_is_a_valid_target() {
        assert!(Target::parse(DEFAULT_TARGET).is_ok());
    }

    #[test]
    fn explicit_target_wins() {
        assert_eq!(resolve_target(Some("armv7".to_owned())), "armv7");
    }
}
