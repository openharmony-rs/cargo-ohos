mod build_env;
mod prebuilt;
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
    disable_help_subcommand = true,
    after_help = cargo_commands_help()
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
    /// Download and cache the newest matching LLVM toolchain release, for example `19`.
    /// Defaults to `$CARGO_OHOS_DOWNLOAD_PREBUILT`.
    #[arg(long, value_name = "VERSION", conflicts_with = "llvm")]
    download_prebuilt: Option<String>,
    /// With --llvm, keep the target flags in `TARGET_CFLAGS`/`TARGET_CXXFLAGS`
    /// instead of folding them into the `CC`/`CXX` values. The default folds
    /// them in so that build systems which re-synthesize compiler command
    /// lines (autoconf probes, SpiderMonkey's moz.configure) still target OHOS.
    #[arg(long)]
    no_inline_flags: bool,
    /// Fail if the SDK's API level (`apiVersion` in its `oh-uni-package.json`)
    /// is below N.
    #[arg(long, value_name = "N")]
    min_api: Option<u32>,
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
    download_prebuilt: Option<String>,
    no_inline_flags: bool,
    min_api: Option<u32>,
}

impl TryFrom<CliOptions> for Options {
    type Error = String;

    fn try_from(cli: CliOptions) -> Result<Self, String> {
        let (llvm, download_prebuilt) = resolve_toolchain_source(
            cli.llvm,
            cli.download_prebuilt,
            std::env::var_os("OHOS_LLVM").map(PathBuf::from),
            std::env::var("CARGO_OHOS_DOWNLOAD_PREBUILT")
                .ok()
                .filter(|value| !value.is_empty()),
        )?;
        if let Some(version) = &download_prebuilt {
            prebuilt::validate_version(version)?;
        }
        Ok(Self {
            target: resolve_target(cli.target),
            sdk: cli.sdk,
            llvm,
            download_prebuilt,
            no_inline_flags: cli.no_inline_flags,
            min_api: cli.min_api,
        })
    }
}

fn resolve_toolchain_source(
    explicit_llvm: Option<PathBuf>,
    explicit_download: Option<String>,
    env_llvm: Option<PathBuf>,
    env_download: Option<String>,
) -> Result<(Option<PathBuf>, Option<String>), String> {
    match (explicit_llvm, explicit_download) {
        (Some(_), Some(_)) => Err(
            "`--llvm` and `--download-prebuilt` select different LLVM toolchains and cannot be used together"
                .to_owned(),
        ),
        (Some(llvm), None) => Ok((Some(llvm), None)),
        (None, Some(download)) => Ok((None, Some(download))),
        (None, None) => match (env_llvm, env_download) {
            (Some(_), Some(_)) => Err(
                "$OHOS_LLVM and $CARGO_OHOS_DOWNLOAD_PREBUILT are both set; unset one of them"
                    .to_owned(),
            ),
            (llvm, download) => Ok((llvm, download)),
        },
    }
}

impl Options {
    fn derive_build_env(&self) -> Result<BuildEnv, String> {
        let target = Target::parse(&self.target).map_err(|e| e.to_string())?;
        let mut config = build_env::Config::new(target);
        config.sdk = self.sdk.clone();
        config.llvm = match &self.download_prebuilt {
            Some(version) => {
                // Validate the SDK before starting a potentially large download.
                let sdk = sdk::Sdk::discover(self.sdk.as_deref()).map_err(|e| e.to_string())?;
                check_min_api(&sdk, self.min_api)?;
                Some(prebuilt::resolve(version)?)
            }
            None => self.llvm.clone(),
        };
        config.no_inline_flags = self.no_inline_flags;
        let build_env = build_env::derive(&config).map_err(|e| e.to_string())?;
        check_min_api(&build_env.sdk, self.min_api)?;
        Ok(build_env)
    }
}

fn check_min_api(sdk: &sdk::Sdk, min_api: Option<u32>) -> Result<(), String> {
    let Some(min) = min_api else {
        return Ok(());
    };
    match sdk.api_version {
        Some(api) if api >= min => Ok(()),
        Some(api) => Err(format!(
            "The OpenHarmony SDK at {} has API level {api}, but --min-api requires at least {min}",
            sdk.native_root.display()
        )),
        None => Err(format!(
            "--min-api {min} was given, but the SDK at {} does not declare an API level \
             (missing or unreadable oh-uni-package.json)",
            sdk.native_root.display()
        )),
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

fn cargo_commands_help() -> String {
    format!(
        "Cargo commands:\n  {}\n\nThese run the matching cargo command with the OpenHarmony \
         cross-compilation environment set up, e.g. `cargo ohos build --release`. See \
         `cargo ohos build --help` for the extra options they accept.",
        CARGO_SUBCOMMANDS.join(", ")
    )
}

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
    let name = match rest.first().map(|s| s.to_string_lossy()) {
        Some(name) if CARGO_SUBCOMMANDS.contains(&name.as_ref()) => name.into_owned(),
        _ => {
            return Err(format!(
                "unsupported cargo subcommand. Supported: {}",
                CARGO_SUBCOMMANDS.join(", ")
            ))
        }
    };

    if wants_cargo_help(&rest) {
        let mut argv: Vec<OsString> = vec!["cargo".into()];
        argv.extend(rest);
        let code = spawn(None, &argv)?;
        print_ohos_options_help(&name);
        return Ok(code);
    }

    let options = Options::try_from(options)?;
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
    spawn(Some(&build_env), &argv)
}

fn wants_cargo_help(rest: &[OsString]) -> bool {
    rest.iter()
        .take_while(|arg| arg.as_os_str() != "--")
        .any(|arg| arg == "--help" || arg == "-h")
}

fn print_ohos_options_help(name: &str) {
    let mut cmd = CliOptions::augment_args(clap::Command::new("cargo-ohos"))
        .disable_help_flag(true)
        .help_template("{options}");
    println!(
        "\nOptions handled by `cargo ohos {name}` itself and not passed on to cargo:\n{}",
        cmd.render_help()
    );
}

fn runs_target_binaries(name: &str, rest: &[OsString]) -> bool {
    matches!(name, "r" | "run" | "t" | "test" | "bench")
        && !rest
            .iter()
            .take_while(|arg| arg.as_os_str() != "--")
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
            "--download-prebuilt" => {
                options.download_prebuilt =
                    Some(take("--download-prebuilt")?.to_string_lossy().into_owned())
            }
            "--no-inline-flags" => options.no_inline_flags = true,
            "--min-api" => {
                options.min_api = Some(parse_min_api(&take("--min-api")?.to_string_lossy())?)
            }
            _ => {
                if let Some(v) = text.strip_prefix("--target=") {
                    options.target = Some(v.to_owned());
                } else if let Some(v) = text.strip_prefix("--download-prebuilt=") {
                    options.download_prebuilt = Some(v.to_owned());
                } else if let Some(v) = text.strip_prefix("--min-api=") {
                    options.min_api = Some(parse_min_api(v)?);
                } else {
                    rest.push(arg);
                }
            }
        }
    }

    Ok((options, rest))
}

fn parse_min_api(value: &str) -> Result<u32, String> {
    value
        .parse()
        .map_err(|_| format!("invalid API level after `--min-api`: `{value}`"))
}

// Pure: only the tool's own values, independent of the ambient environment,
// so `env` output can be cached or evaluated repeatedly. The cargo
// subcommands fold ambient flags in via `passthrough_env` instead.
fn resolved_env(build_env: &BuildEnv) -> BTreeMap<String, String> {
    let mut env = build_env.env.clone();
    env.insert(
        "CARGO_ENCODED_RUSTFLAGS".to_owned(),
        build_env.flags.rustflags.join("\u{1f}"),
    );
    env
}

fn ambient_rustflags() -> Vec<String> {
    if let Ok(encoded) = std::env::var("CARGO_ENCODED_RUSTFLAGS") {
        encoded
            .split('\u{1f}')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    } else if let Ok(plain) = std::env::var("RUSTFLAGS") {
        plain.split_whitespace().map(str::to_owned).collect()
    } else {
        Vec::new()
    }
}

fn passthrough_env(build_env: &BuildEnv) -> BTreeMap<String, String> {
    let mut env = resolved_env(build_env);

    let ambient = ambient_rustflags();
    let tool_rustflags = &build_env.flags.rustflags;
    let rustflags = if contains_sequence(&ambient, tool_rustflags) {
        ambient
    } else {
        tool_rustflags.iter().cloned().chain(ambient).collect()
    };
    env.insert(
        "CARGO_ENCODED_RUSTFLAGS".to_owned(),
        rustflags.join("\u{1f}"),
    );

    let bindgen_var = format!(
        "BINDGEN_EXTRA_CLANG_ARGS_{}",
        build_env.target.rust_triple_underscored()
    );
    for warning in fold_ambient_flags(&mut env, &bindgen_var, |key| std::env::var(key).ok()) {
        eprintln!("warning: {warning}");
    }
    env
}

// User-supplied flags override defaults where order matters.
fn fold_ambient_flags(
    env: &mut BTreeMap<String, String>,
    bindgen_var: &str,
    ambient: impl Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    for (key, plain) in [
        ("TARGET_CFLAGS", "CFLAGS"),
        ("TARGET_CXXFLAGS", "CXXFLAGS"),
        ("TARGET_CPPFLAGS", "CPPFLAGS"),
        (bindgen_var, "BINDGEN_EXTRA_CLANG_ARGS"),
    ] {
        let Some(tool) = env.get(key).cloned() else {
            continue;
        };
        match ambient(key).filter(|value| !value.is_empty()) {
            Some(value) => {
                let folded = if value.contains(&tool) {
                    value
                } else {
                    format!("{tool} {value}")
                };
                env.insert(key.to_owned(), folded);
            }
            None => {
                if ambient(plain).is_some_and(|value| !value.is_empty()) {
                    warnings.push(format!(
                        "${plain} is set, but `cargo ohos` sets ${key} and build scripts use \
                         the most specific variable only; put the flags in ${key} to compose"
                    ));
                }
            }
        }
    }
    warnings
}

fn contains_sequence(haystack: &[String], needle: &[String]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
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
            let flags = &build_env.flags.rustflags;
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
                "cargo_ohos_version": env!("CARGO_PKG_VERSION"),
                "sdk": build_env.sdk,
                "target": build_env.target,
                "toolchain": build_env.toolchain,
                "flags": build_env.flags,
                "runtime_libraries": build_env.runtime_libraries,
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

fn spawn(build_env: Option<&BuildEnv>, argv: &[OsString]) -> Result<ExitCode, String> {
    let (program, args) = argv.split_first().ok_or("no command given")?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(build_env) = build_env {
        cmd.envs(passthrough_env(build_env));
        cmd.env_remove("RUSTFLAGS");
    }

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
    fn split_takes_min_api() {
        let args: Vec<OsString> = ["build", "--min-api", "14"].map(OsString::from).to_vec();
        let (options, rest) = split_cargo_args(args).unwrap();
        assert_eq!(options.min_api, Some(14));
        assert_eq!(rest, [OsString::from("build")]);

        let args: Vec<OsString> = ["build", "--min-api=21"].map(OsString::from).to_vec();
        let (options, _) = split_cargo_args(args).unwrap();
        assert_eq!(options.min_api, Some(21));

        let args: Vec<OsString> = ["build", "--min-api", "five"].map(OsString::from).to_vec();
        assert!(split_cargo_args(args).is_err());
    }

    #[test]
    fn min_api_gate() {
        let fake_sdk = |api_version: Option<u32>| sdk::Sdk {
            native_root: PathBuf::from("/sdk/native"),
            sysroot: PathBuf::from("/sdk/native/sysroot"),
            llvm_root: PathBuf::from("/sdk/native/llvm"),
            llvm_bin: PathBuf::from("/sdk/native/llvm/bin"),
            cmake: None,
            cmake_toolchain_file: None,
            api_version,
            version: None,
        };
        assert!(check_min_api(&fake_sdk(Some(21)), None).is_ok());
        assert!(check_min_api(&fake_sdk(Some(21)), Some(14)).is_ok());
        assert!(check_min_api(&fake_sdk(Some(14)), Some(14)).is_ok());
        assert!(check_min_api(&fake_sdk(Some(12)), Some(14)).is_err());
        assert!(check_min_api(&fake_sdk(None), Some(14)).is_err());
        assert!(check_min_api(&fake_sdk(None), None).is_ok());
    }

    #[test]
    fn split_takes_download_prebuilt() {
        let args: Vec<OsString> = ["build", "--download-prebuilt=19"]
            .iter()
            .map(OsString::from)
            .collect();
        let (options, rest) = split_cargo_args(args).unwrap();

        assert_eq!(options.download_prebuilt.as_deref(), Some("19"));
        assert_eq!(rest, [OsString::from("build")]);
    }

    #[test]
    fn explicit_toolchain_source_overrides_environment_default() {
        let (llvm, download) = resolve_toolchain_source(
            Some(PathBuf::from("explicit")),
            None,
            None,
            Some("19".to_owned()),
        )
        .unwrap();
        assert_eq!(llvm, Some(PathBuf::from("explicit")));
        assert_eq!(download, None);

        let (llvm, download) = resolve_toolchain_source(
            None,
            Some("19".to_owned()),
            Some(PathBuf::from("environment")),
            None,
        )
        .unwrap();
        assert_eq!(llvm, None);
        assert_eq!(download.as_deref(), Some("19"));
    }

    #[test]
    fn conflicting_environment_toolchain_sources_are_rejected() {
        assert!(resolve_toolchain_source(
            None,
            None,
            Some(PathBuf::from("environment")),
            Some("19".to_owned()),
        )
        .is_err());
    }

    #[test]
    fn fold_appends_ambient_after_tool_flags() {
        let mut env = BTreeMap::from([
            ("TARGET_CFLAGS".to_owned(), "--target=t".to_owned()),
            ("BINDGEN_EXTRA_CLANG_ARGS_x".to_owned(), "-Ii".to_owned()),
        ]);
        let warnings = fold_ambient_flags(&mut env, "BINDGEN_EXTRA_CLANG_ARGS_x", |key| {
            (key == "TARGET_CFLAGS").then(|| "-fsanitize=address".to_owned())
        });
        assert_eq!(env["TARGET_CFLAGS"], "--target=t -fsanitize=address");
        assert_eq!(env["BINDGEN_EXTRA_CLANG_ARGS_x"], "-Ii");
        assert!(warnings.is_empty());
    }

    #[test]
    fn fold_keeps_already_composed_ambient_values() {
        let mut env = BTreeMap::from([("TARGET_CFLAGS".to_owned(), "--target=t".to_owned())]);
        fold_ambient_flags(&mut env, "B", |key| {
            (key == "TARGET_CFLAGS").then(|| "--target=t -g".to_owned())
        });
        assert_eq!(env["TARGET_CFLAGS"], "--target=t -g");
    }

    #[test]
    fn fold_ignores_vars_the_tool_does_not_set() {
        let mut env = BTreeMap::new();
        let warnings = fold_ambient_flags(&mut env, "B", |key| {
            (key == "TARGET_CFLAGS").then(|| "-g".to_owned())
        });
        assert!(env.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn fold_warns_about_masked_plain_vars() {
        let mut env = BTreeMap::from([
            ("TARGET_CFLAGS".to_owned(), "--target=t".to_owned()),
            ("BINDGEN_EXTRA_CLANG_ARGS_x".to_owned(), "-Ii".to_owned()),
        ]);
        let warnings = fold_ambient_flags(&mut env, "BINDGEN_EXTRA_CLANG_ARGS_x", |key| {
            matches!(key, "CFLAGS" | "BINDGEN_EXTRA_CLANG_ARGS").then(|| "-g".to_owned())
        });
        assert_eq!(env["TARGET_CFLAGS"], "--target=t");
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("$CFLAGS"));
        assert!(warnings[0].contains("$TARGET_CFLAGS"));
        assert!(warnings[1].contains("$BINDGEN_EXTRA_CLANG_ARGS "));
    }

    #[test]
    fn fold_does_not_warn_when_the_specific_var_is_also_ambient() {
        let mut env = BTreeMap::from([("TARGET_CFLAGS".to_owned(), "--target=t".to_owned())]);
        let warnings = fold_ambient_flags(&mut env, "B", |key| {
            matches!(key, "TARGET_CFLAGS" | "CFLAGS").then(|| "-g".to_owned())
        });
        assert!(warnings.is_empty());
    }

    #[test]
    fn sequence_containment() {
        let flags = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert!(contains_sequence(&flags("-a -b -c"), &flags("-b -c")));
        assert!(!contains_sequence(&flags("-a -b"), &flags("-b -c")));
        assert!(!contains_sequence(&flags("-b"), &flags("-b -c")));
        assert!(contains_sequence(&flags("-a"), &[]));
    }

    #[test]
    fn default_target_is_a_valid_target() {
        assert!(Target::parse(DEFAULT_TARGET).is_ok());
    }

    #[test]
    fn explicit_target_wins() {
        assert_eq!(resolve_target(Some("armv7".to_owned())), "armv7");
    }

    #[test]
    fn root_help_mentions_cargo_subcommands() {
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("cargo ohos build"));
        for name in CARGO_SUBCOMMANDS {
            assert!(help.contains(name), "help does not mention `{name}`");
        }
    }

    #[test]
    fn help_before_double_dash_is_a_help_request() {
        assert!(wants_cargo_help(&["build", "--help"].map(OsString::from)));
        assert!(wants_cargo_help(&["build", "-h"].map(OsString::from)));
        assert!(!wants_cargo_help(
            &["run", "--", "--help"].map(OsString::from)
        ));
        assert!(!wants_cargo_help(&["build"].map(OsString::from)));
    }

    #[test]
    fn target_help_still_needs_a_runner() {
        let args = ["run", "--", "--help"].map(OsString::from);
        assert!(runs_target_binaries("run", &args));
    }

    #[test]
    fn cargo_help_does_not_need_a_runner() {
        let args = ["run", "--help"].map(OsString::from);
        assert!(!runs_target_binaries("run", &args));
    }
}
