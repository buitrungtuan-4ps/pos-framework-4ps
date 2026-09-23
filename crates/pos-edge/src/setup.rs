// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `pos-edge install` — a store PC sets itself up from the one file it was given
//! ([ADR-0140](../../../docs/adr/0140-a-store-pc-installs-itself-from-one-file.md)).
//!
//! Bringing a Windows store online took a technician a binary, a `PowerShell` script, three values
//! typed as parameters, an elevated shell and a runbook. The script is right — CI parses it under
//! both `PowerShell` editions and a shop's only till has run it — so this does not replace it. It
//! **carries** it: the script is embedded in the binary, and `pos-edge install` runs it with the
//! values it can work out for itself.
//!
//! # One file, named for its store
//!
//! The console hands out the edge binary renamed `pos-edge-setup_<cloud host>_<store ULID>.exe`.
//! Double-clicking a file with that name installs rather than serves: the name says which store the
//! machine is and which cloud it dials, which is everything `config.toml` needs, and neither is a
//! secret — the device credential arrives later, through activation at `/setup`, which the installer
//! opens when it is done, and the store's sync key is pasted into the installer's own window
//! ([ADR-0141](../../../docs/adr/0141-the-console-hands-out-the-installer-by-name.md)). The bytes
//! are the release's own, so the minisign signature still verifies; data appended to the file would
//! have broken that.
//!
//! A browser saving a second copy calls it `… (1).exe`; that suffix is tolerated. A cloud on a port
//! other than 443 is written `host@port`. The name always means `https`, `localhost` included: the
//! edge's cloud transport speaks nothing else, so a name that implied plain HTTP would install a box
//! that refuses its own cloud and runs LAN-only.
//!
//! # What runs where
//!
//! The name and argument parsing below is plain Rust and is tested on every platform. Running the
//! script needs Windows and an elevated process: a double-click is not elevated, so the first run
//! asks Windows to start the same file again as an administrator (the UAC prompt), and that second
//! run writes the embedded script to a temporary file and hands it to `PowerShell` in a window that
//! stays open, so the technician reads the result rather than watching it vanish.

use std::ffi::OsString;
use std::path::Path;

use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;

/// The subcommand that installs this binary as the store's Windows service.
pub const INSTALL_COMMAND: &str = "install";

/// The prefix a tagged installer's file name starts with. Compared case-insensitively, because
/// Windows file names are.
pub const TAGGED_PREFIX: &str = "pos-edge-setup_";

/// The port the installer binds when nothing says otherwise — the script's own default.
pub const DEFAULT_PORT: u16 = 8787;

/// Which store this machine becomes, and which cloud it dials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTarget {
    /// The store the machine is.
    pub store_id: StoreId,
    /// The cloud it dials, as `config.toml`'s `cloud_url` spells it.
    pub cloud_url: url::Url,
}

/// Why a name or a command line could not be read as an install.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallInputError {
    /// The file is not a tagged installer, and the command line did not name a store and a cloud.
    #[error(
        "say which store and which cloud: pos-edge install --store <ULID> --cloud <https URL>, or \
         run the installer the console downloads (pos-edge-setup_<cloud>_<store>.exe)"
    )]
    Missing,
    /// The store id is not a ULID.
    #[error("the store id is not a ULID: {0}")]
    StoreId(String),
    /// The cloud is not an absolute `https` URL — the only kind the edge's cloud transport dials.
    #[error("the cloud is not an https URL: {0}")]
    CloudUrl(String),
}

/// Reads a tagged installer's file name: `pos-edge-setup_<host>[@<port>]_<store ULID>[ (n)].exe`.
///
/// `None` for any other name, which is what makes an untagged binary run as a server whatever it
/// is called.
#[must_use]
pub fn from_file_name(name: &str) -> Option<InstallTarget> {
    let lower = name.to_ascii_lowercase();
    if !lower.starts_with(TAGGED_PREFIX) {
        return None;
    }
    let stem = name.get(TAGGED_PREFIX.len()..)?;
    let stem = strip_suffix_ignoring_case(stem, ".exe").unwrap_or(stem);
    // A second download of the same file: "… (1)".
    let stem = match stem.rfind(" (") {
        Some(at) if stem.ends_with(')') => stem.get(..at)?,
        _ => stem,
    };
    let (host, store) = stem.rsplit_once('_')?;
    let store_id = parse_store_id(store).ok()?;
    let cloud_url = cloud_from_host(host).ok()?;
    Some(InstallTarget {
        store_id,
        cloud_url,
    })
}

/// Reads `--store <ULID> --cloud <URL>` from an `install` command line (the arguments after
/// `install`).
///
/// # Errors
///
/// [`InstallInputError`] naming what is missing or malformed.
pub fn from_arguments(arguments: &[String]) -> Result<InstallTarget, InstallInputError> {
    let value = |flag: &str| {
        arguments
            .iter()
            .position(|argument| argument == flag)
            .and_then(|at| arguments.get(at + 1))
    };
    let (Some(store), Some(cloud)) = (value("--store"), value("--cloud")) else {
        return Err(InstallInputError::Missing);
    };
    let store_id = parse_store_id(store)?;
    let cloud_url = url::Url::parse(cloud)
        .ok()
        .filter(acceptable_cloud)
        .ok_or_else(|| InstallInputError::CloudUrl(cloud.clone()))?;
    Ok(InstallTarget {
        store_id,
        cloud_url,
    })
}

/// The parameters the embedded `PowerShell` installer is run with.
///
/// `-AskSyncKey` makes it ask, in its own window, for the store key the file name deliberately does
/// not carry ([ADR-0141](../../../docs/adr/0141-the-console-hands-out-the-installer-by-name.md)), so a
/// technician pastes it there rather than on a command line. `-OpenSetup` makes it open the
/// activation screen when the service is up, which is the next step and the only one left.
#[must_use]
pub fn installer_arguments(script: &Path, binary: &Path, target: &InstallTarget) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = [
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        // The window stays open when the script ends: the pairing URL and any warning are the
        // point of reading it.
        "-NoExit",
        "-File",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    arguments.push(script.as_os_str().to_owned());
    arguments.push("-Binary".into());
    arguments.push(binary.as_os_str().to_owned());
    arguments.push("-StoreId".into());
    arguments.push(target.store_id.to_string().into());
    arguments.push("-CloudUrl".into());
    // The script writes this into config.toml as given; `Url` adds a trailing slash to a bare
    // origin, which the edge's own URL handling does not want doubled.
    arguments.push(target.cloud_url.as_str().trim_end_matches('/').into());
    arguments.push("-AskSyncKey".into());
    arguments.push("-OpenSetup".into());
    arguments
}

/// Whether this process was started from a tagged installer, by its file name.
#[must_use]
pub fn launched_as_installer() -> bool {
    std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| from_file_name(name).is_some())
}

fn strip_suffix_ignoring_case<'a>(value: &'a str, suffix: &str) -> Option<&'a str> {
    let split = value.len().checked_sub(suffix.len())?;
    let (head, tail) = (value.get(..split)?, value.get(split..)?);
    tail.eq_ignore_ascii_case(suffix).then_some(head)
}

fn parse_store_id(value: &str) -> Result<StoreId, InstallInputError> {
    value
        .parse::<Ulid>()
        .map(StoreId::new)
        .map_err(|_| InstallInputError::StoreId(value.to_owned()))
}

/// `host` or `host@port` from a file name, to the URL it stands for.
fn cloud_from_host(host: &str) -> Result<url::Url, InstallInputError> {
    let (name, port) = match host.rsplit_once('@') {
        Some((name, port)) => (name, Some(port)),
        None => (host, None),
    };
    let valid_name = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    let port = match port {
        Some(port) => Some(
            port.parse::<u16>()
                .map_err(|_| InstallInputError::CloudUrl(host.to_owned()))?,
        ),
        None => None,
    };
    if !valid_name {
        return Err(InstallInputError::CloudUrl(host.to_owned()));
    }
    let text = match port {
        Some(port) => format!("https://{name}:{port}"),
        None => format!("https://{name}"),
    };
    url::Url::parse(&text).map_err(|_| InstallInputError::CloudUrl(host.to_owned()))
}

/// `https` with a host, and nothing else: a store's credential crosses this link, and the edge's
/// cloud transport refuses any other scheme — on a loopback address too.
fn acceptable_cloud(url: &url::Url) -> bool {
    url.scheme() == "https" && url.host_str().is_some()
}

/// Runs the install: elevate if needed, then the embedded script.
///
/// # Errors
///
/// [`crate::EdgeError::Install`] with what went wrong, in words a technician can act on.
#[cfg(windows)]
pub fn run(arguments: &[String]) -> Result<(), crate::EdgeError> {
    windows::run(arguments)
}

/// Runs the install: Windows only.
///
/// # Errors
///
/// Always, off Windows: a Linux store installs with `install-pos-edge.sh`, which the console
/// generates beside the Windows script.
#[cfg(not(windows))]
pub fn run(_arguments: &[String]) -> Result<(), crate::EdgeError> {
    Err(crate::EdgeError::Install(
        "pos-edge install sets up the Windows service; on Linux run the install-pos-edge.sh the \
         console generates"
            .to_owned(),
    ))
}

#[cfg(windows)]
mod windows {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{InstallTarget, from_arguments, from_file_name, installer_arguments};
    use crate::EdgeError;

    /// The script the console generates for a Windows store (`deploy/edge/install-pos-edge.ps1`),
    /// byte-order mark included: Windows PowerShell 5.1 reads a script without one in the machine's
    /// code page, and this one carries non-ASCII punctuation.
    const INSTALLER_SCRIPT: &str = include_str!("../../../deploy/edge/install-pos-edge.ps1");

    pub(super) fn run(arguments: &[String]) -> Result<(), EdgeError> {
        let binary = std::env::current_exe().map_err(|error| {
            EdgeError::Install(format!("cannot find this program's path: {error}"))
        })?;
        let target = target(&binary, arguments)?;
        if !elevated() {
            return relaunch_elevated(&binary, arguments);
        }
        let script = write_script()?;
        let status = Command::new("powershell.exe")
            .args(installer_arguments(&script, &binary, &target))
            .status()
            .map_err(|error| EdgeError::Install(format!("could not start PowerShell: {error}")))?;
        if status.success() {
            Ok(())
        } else {
            Err(EdgeError::Install(format!(
                "the installer script stopped with {status}; its window says why"
            )))
        }
    }

    /// The store and cloud: from the command line when given, else from this file's own name.
    fn target(binary: &Path, arguments: &[String]) -> Result<InstallTarget, EdgeError> {
        if arguments.iter().any(|argument| argument == "--store") {
            return from_arguments(arguments)
                .map_err(|error| EdgeError::Install(error.to_string()));
        }
        binary
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(from_file_name)
            .map_or_else(
                || from_arguments(arguments).map_err(|error| EdgeError::Install(error.to_string())),
                Ok,
            )
    }

    /// Whether this process holds administrator rights. `net session` answers only to an
    /// administrator, which is the conventional test that needs no Windows API binding.
    fn elevated() -> bool {
        Command::new("net")
            .arg("session")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Starts this same file again as an administrator — the UAC prompt — and returns.
    fn relaunch_elevated(binary: &Path, arguments: &[String]) -> Result<(), EdgeError> {
        let mut forwarded = vec!["install".to_owned()];
        forwarded.extend(arguments.iter().cloned());
        let list = forwarded
            .iter()
            .map(|argument| format!("'{}'", argument.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let command = format!(
            "Start-Process -FilePath '{}' -ArgumentList {list} -Verb RunAs",
            binary.display().to_string().replace('\'', "''")
        );
        let status = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", &command])
            .status()
            .map_err(|error| {
                EdgeError::Install(format!("could not ask for administrator rights: {error}"))
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(EdgeError::Install(
                "administrator rights were not granted; the store was not installed".to_owned(),
            ))
        }
    }

    /// Writes the embedded script where PowerShell can run it.
    fn write_script() -> Result<PathBuf, EdgeError> {
        let path =
            std::env::temp_dir().join(format!("pos-edge-install-{}.ps1", std::process::id()));
        std::fs::write(&path, INSTALLER_SCRIPT).map_err(|error| {
            EdgeError::Install(format!("could not write the installer script: {error}"))
        })?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE: &str = "01J9ZQ3M6V4Q1ZB2Y7H8K5N0PX";

    #[test]
    fn a_tagged_name_carries_the_store_and_the_cloud() {
        let target = from_file_name(&format!("pos-edge-setup_pos.example.vn_{STORE}.exe"))
            .expect("a tagged name");
        assert_eq!(target.store_id.to_string(), STORE);
        assert_eq!(target.cloud_url.as_str(), "https://pos.example.vn/");
    }

    #[test]
    fn a_port_a_second_download_and_the_case_windows_uses_are_all_read() {
        let target = from_file_name(&format!(
            "POS-EDGE-SETUP_cloud.example.com@8443_{STORE} (2).EXE"
        ))
        .expect("a tagged name");
        assert_eq!(target.cloud_url.as_str(), "https://cloud.example.com:8443/");
        assert_eq!(target.store_id.to_string(), STORE);
    }

    #[test]
    fn a_loopback_cloud_is_dialled_over_https_too() {
        // The edge's cloud transport speaks only https, so a name implying plain HTTP would install
        // a box that refuses its own cloud and runs LAN-only.
        let target = from_file_name(&format!("pos-edge-setup_localhost@8080_{STORE}.exe"))
            .expect("a tagged name");
        assert_eq!(target.cloud_url.as_str(), "https://localhost:8080/");
    }

    #[test]
    fn any_other_name_runs_as_a_server() {
        assert_eq!(from_file_name("pos-edge.exe"), None);
        assert_eq!(from_file_name("current"), None);
        assert_eq!(from_file_name("pos-edge-setup_.exe"), None);
        assert_eq!(
            from_file_name("pos-edge-setup_pos.example.vn_not-a-ulid.exe"),
            None
        );
        assert_eq!(
            from_file_name(&format!("pos-edge-setup_bad host!_{STORE}.exe")),
            None,
            "a host a URL cannot carry is not guessed at"
        );
    }

    #[test]
    fn a_command_line_names_the_store_and_an_https_cloud() {
        let args: Vec<String> = ["--store", STORE, "--cloud", "https://pos.example.vn"]
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        let target = from_arguments(&args).expect("reads");
        assert_eq!(target.store_id.to_string(), STORE);

        for plain in ["http://pos.example.vn", "http://localhost:8080"] {
            let args: Vec<String> = ["--store", STORE, "--cloud", plain]
                .iter()
                .map(|value| (*value).to_owned())
                .collect();
            assert!(
                matches!(from_arguments(&args), Err(InstallInputError::CloudUrl(_))),
                "{plain} is refused: the edge dials its cloud over https only"
            );
        }
        assert_eq!(from_arguments(&[]), Err(InstallInputError::Missing));
    }

    #[test]
    fn the_script_is_run_with_the_values_and_opens_the_activation_screen() {
        let target = from_file_name(&format!("pos-edge-setup_pos.example.vn_{STORE}.exe"))
            .expect("a tagged name");
        let arguments: Vec<String> = installer_arguments(
            Path::new("C:\\Temp\\install.ps1"),
            Path::new("C:\\Downloads\\setup.exe"),
            &target,
        )
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
        let after = |flag: &str| {
            arguments
                .iter()
                .position(|argument| argument == flag)
                .and_then(|at| arguments.get(at + 1))
                .cloned()
        };
        assert_eq!(after("-StoreId").as_deref(), Some(STORE));
        assert_eq!(
            after("-CloudUrl").as_deref(),
            Some("https://pos.example.vn")
        );
        assert_eq!(
            after("-Binary").as_deref(),
            Some("C:\\Downloads\\setup.exe")
        );
        assert!(arguments.iter().any(|argument| argument == "-OpenSetup"));
        assert!(arguments.iter().any(|argument| argument == "-AskSyncKey"));
        assert!(arguments.iter().any(|argument| argument == "-NoExit"));
    }
}
