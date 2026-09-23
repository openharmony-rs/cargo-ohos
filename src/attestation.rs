//! Verification of the GitHub build provenance attestations the mirrors publish
//! for their release artifacts, via the `gh` CLI.

use std::path::Path;
use std::process::{Command, Output};

use crate::download;

/// The workflow allowed to have produced an artifact.
pub struct Signer {
    pub repository: &'static str,
    pub workflow: &'static str,
}

/// Whether `gh` is installed and authenticated, which `verify` needs. Says why
/// verification is skipped when it is not.
pub fn available() -> Result<bool, String> {
    available_with(|command| command.output())
}

fn available_with(
    mut run: impl FnMut(&mut Command) -> std::io::Result<Output>,
) -> Result<bool, String> {
    let auth_output = match run(Command::new("gh")
        .arg("auth")
        .arg("status")
        .arg("--active")
        .arg("--hostname")
        .arg("github.com"))
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "note: `gh` is not installed; skipping GitHub artifact attestation verification"
            );
            return Ok(false);
        }
        Err(error) => return Err(format!("could not run `gh auth status`: {error}")),
    };
    if !auth_output.status.success() {
        eprintln!(
            "note: `gh` is not authenticated; skipping GitHub artifact attestation verification"
        );
    }
    Ok(auth_output.status.success())
}

/// Whether `repository` published an attestation covering `digest`. Releases made
/// before a mirror started attesting have none, and cannot be verified.
pub fn is_attested(digest: &str, signer: &Signer) -> Result<bool, String> {
    download::exists(&format!(
        "https://api.github.com/repos/{}/attestations/sha256:{digest}",
        signer.repository
    ))
}

/// Verify that `artifact` was built by `signer`'s workflow at `source_ref`. Needs
/// `gh`, see `available`.
pub fn verify(artifact: &Path, signer: &Signer, source_ref: &str) -> Result<(), String> {
    verify_with(artifact, signer, source_ref, |command| command.output())
}

fn verify_with(
    artifact: &Path,
    signer: &Signer,
    source_ref: &str,
    mut run: impl FnMut(&mut Command) -> std::io::Result<Output>,
) -> Result<(), String> {
    let output = run(Command::new("gh")
        .arg("attestation")
        .arg("verify")
        .arg(artifact)
        .arg("--repo")
        .arg(signer.repository)
        .arg("--signer-workflow")
        .arg(signer.workflow)
        .arg("--source-ref")
        .arg(source_ref)
        .arg("--deny-self-hosted-runners"))
    .map_err(|error| format!("could not run `gh attestation verify`: {error}"))?;
    if !output.status.success() {
        return Err(with_command_output(
            format!(
                "GitHub artifact attestation verification failed for {}",
                artifact.display()
            ),
            &output.stdout,
            &output.stderr,
        ));
    }
    eprintln!("note: GitHub artifact attestation verified");
    Ok(())
}

fn with_command_output(mut message: String, stdout: &[u8], stderr: &[u8]) -> String {
    for (label, bytes) in [("stdout", stdout), ("stderr", stderr)] {
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim();
        if !text.is_empty() {
            message.push_str(&format!("\n{label}:\n{text}"));
        }
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exit_status(code: u8) -> std::process::ExitStatus {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;
        #[cfg(unix)]
        let raw = i32::from(code) << 8;
        #[cfg(windows)]
        let raw = u32::from(code);
        ExitStatusExt::from_raw(raw)
    }

    fn output(code: u8) -> std::io::Result<Output> {
        Ok(Output {
            status: exit_status(code),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }

    #[test]
    fn is_available_only_when_gh_is_installed_and_authenticated() {
        assert_eq!(available_with(|_| output(0)), Ok(true));
        assert_eq!(available_with(|_| output(1)), Ok(false));
        assert_eq!(
            available_with(|_| Err(std::io::ErrorKind::NotFound.into())),
            Ok(false)
        );
        assert!(available_with(|_| Err(std::io::ErrorKind::PermissionDenied.into())).is_err());
    }

    #[test]
    fn verifies_the_artifact_against_the_signer_and_ref() {
        let signer = Signer {
            repository: "openharmony-rs/ohos-sdk",
            workflow: "openharmony-rs/ohos-sdk/.github/workflows/Release.yml",
        };
        let mut commands = Vec::new();
        let result = verify_with(
            Path::new("archive.tar.gz"),
            &signer,
            "refs/tags/v7.0",
            |command| {
                commands.push(
                    command
                        .get_args()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
                output(0)
            },
        );

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(commands.len(), 1);
        assert!(
            commands[0].starts_with("attestation verify"),
            "{}",
            commands[0]
        );
        assert!(
            commands[0].contains("--repo openharmony-rs/ohos-sdk"),
            "{}",
            commands[0]
        );
        assert!(
            commands[0].contains(
                "--signer-workflow openharmony-rs/ohos-sdk/.github/workflows/Release.yml"
            ),
            "{}",
            commands[0]
        );
        assert!(
            commands[0].contains("--source-ref refs/tags/v7.0"),
            "{}",
            commands[0]
        );
    }

    #[test]
    fn includes_captured_command_output_in_errors() {
        let message = with_command_output(
            "verification failed".to_owned(),
            b"attestation details\n",
            b"verification error\n",
        );

        assert_eq!(
            message,
            "verification failed\nstdout:\nattestation details\nstderr:\nverification error"
        );
    }
}
