// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The real Linux [`UpdateInstaller`] (roadmap v3 **R4**, shipped with **R5**):
//! [ADR-0055](../../../docs/adr/0055-edge-ota-updater.md) Amendment 1.
//!
//! Until this landed, the only implementor of the install seam in the tree was `RecordingInstaller`
//! in `crates/pos-edge/tests/ota.rs` — the orchestration was complete, tested, and could not run.
//!
//! # Why the binary lives under the state directory
//!
//! `deploy/edge/pos-edge.service` runs the store as an unprivileged `pos` user under
//! `ProtectSystem=strict` and `NoNewPrivileges=true`, which between them make the whole filesystem
//! read-only to the process except its `StateDirectory` — and leave no route to escalate. So the
//! edge **cannot** write `/usr/local/bin/pos-edge`, and loosening the sandbox to let it would give a
//! compromised till the ability to replace system binaries: a strictly worse posture than the one
//! that blocked us.
//!
//! So the running binary moves to where the service already has write access. `ExecStart` is
//! `<state>/bin/current` — a symlink this module owns — and the versions it points at are ordinary
//! files beside it. `/usr/local/bin/pos-edge` stays the operator's copy and what bootstrap
//! installs; it is no longer what runs.
//!
//! # Two slots, and one atomic rename
//!
//! ```text
//! <state>/bin/current      -> slot-a | slot-b   the symlink systemd starts
//! <state>/bin/previous     -> slot-a | slot-b   the last binary that came up healthy
//! <state>/bin/slot-a       a version file, mode 0755
//! <state>/bin/slot-b       the other one
//! <state>/bin/staged       verified bytes that have not been committed
//! <state>/bin/unconfirmed  present while a committed version has never booted healthy
//! <store db>.pre-update    the database as it was before the install
//! ```
//!
//! Two slots rather than a `versions/<v>` tree because [`UpdateInstaller::apply`] receives bytes and
//! no version — the seam's shape, and the right shape: a version string that reached the disk
//! through a *different* path than the bytes is a fourth place for a release's name to drift
//! ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 2).
//! [`SystemdInstaller::commit`] promotes `staged` into whichever slot `current` is *not* using and
//! retargets the symlink with a single [`std::fs::rename`], so there is no instant at which
//! `current` names nothing.
//!
//! # The self-test that gates `commit` is not the self-test that decides a rollback
//!
//! [`SystemdInstaller::self_test`] execs the staged file as `pos-edge --self-test` and asks *can
//! these bytes run on this box*: it catches the wrong architecture, a truncated download, a missing
//! shared library. It is not the verdict [`decide_rollout`](pos_core::ota::decide_rollout) reads —
//! that one compares against the version the box is **running**, so it can only be recorded after
//! the restart. The boot half is [`SystemdInstaller::begin_boot`] and
//! [`SystemdInstaller::clear_boot_marker`], and ADR-0055 Amendment 1 explains why a pre-commit test
//! alone would let a store install the same bad build forever.

use core::time::Duration;
use std::path::{Path, PathBuf};

use crate::ota::{InstallError, UpdateInstaller};
use crate::ota_client::{BootConfirmation, BootStanding};

/// The symlink `ExecStart` runs, relative to the binary directory.
const CURRENT: &str = "current";

/// The symlink naming the last version that booted healthy — where [`UpdateInstaller::rollback`]
/// goes back to.
const PREVIOUS: &str = "previous";

/// The two version files `current` and `previous` alternate between.
const SLOTS: [&str; 2] = ["slot-a", "slot-b"];

/// Verified bytes that have not been committed yet.
const STAGED: &str = "staged";

/// Present while a committed version has never reached a healthy boot; its contents are the number
/// of boots attempted since the commit.
///
/// It does **not** record which version is unconfirmed, deliberately: the only reader is the next
/// boot, and by construction that boot is running the version in question — the commit retargets the
/// symlink and the process then exits for `systemd` to restart it. Storing the version as well would
/// be a second copy of a fact already available, and a second copy is a thing that can disagree.
const UNCONFIRMED: &str = "unconfirmed";

/// The sidecar copy [`UpdateInstaller::stage_backup`] makes, appended to the store database's name
/// (`docs/roadmap.md` P9).
const PRE_UPDATE_SUFFIX: &str = ".pre-update";

/// Mode of a version file on a Unix box: readable and executable by all, writable only by the owner.
#[cfg(unix)]
const EXECUTABLE: u32 = 0o755;

/// Links `link` at `target`, a bare file name in the same directory.
///
/// The two operating-system primitives this module needs are here and nowhere else, so everything
/// above is portable and the Windows store installer (roadmap **E4**) is a matter of these two
/// functions plus a service manager that restarts on exit — not a second copy of the swap logic.
#[cfg(unix)]
fn link_at(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// As above, on Windows — where creating a symlink needs a privilege a store service will not have
/// unless an administrator granted it, which is why [`SystemdInstaller::is_ready`] is the gate
/// rather than an assumption.
#[cfg(windows)]
fn link_at(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// Makes `path` executable. A no-op off Unix, where the permission bit does not exist.
#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(EXECUTABLE))
}

/// As above, off Unix: nothing to do.
#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Unix arm can fail; both arms must have one signature"
)]
fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// How many boots a committed version gets to reach [`SystemdInstaller::clear_boot_marker`] before
/// the edge gives up and reverts.
///
/// Three, not one: a store loses power mid-boot, a printer's USB enumeration hangs a driver, an SNTP
/// step delays the first bind. One attempt would revert a good release on any of those. Ten would
/// leave a genuinely broken release crash-looping for minutes while the shop cannot sell.
pub const MAX_UNCONFIRMED_BOOTS: u32 = 3;

/// How long the staged binary gets to answer `--self-test` before the attempt is judged failed.
///
/// The check itself is a config parse and a read-only database open, so this is generous by an order
/// of magnitude; what it really bounds is a binary that starts and then hangs, which would otherwise
/// wedge the OTA loop's worker thread for as long as the box is up.
pub const SELF_TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the wait on the staged binary polls for its exit.
const SELF_TEST_POLL: Duration = Duration::from_millis(100);

/// How many polls make up [`SELF_TEST_TIMEOUT`] — the wait's budget, expressed as a count so it
/// needs no clock. `the_self_test_budget_is_the_timeout` keeps the three constants consistent.
const SELF_TEST_POLLS: u32 = 300;

/// The Linux install seam: a two-slot binary directory under the service's own `StateDirectory`, an
/// atomic symlink swap, and a boot marker that makes a bad release heal itself
/// ([ADR-0055](../../../docs/adr/0055-edge-ota-updater.md) Amendment 1).
#[derive(Debug, Clone)]
pub struct SystemdInstaller {
    bin: PathBuf,
    database: PathBuf,
}

impl SystemdInstaller {
    /// An installer over `bin` (the `<state>/bin` directory `ExecStart`'s symlink lives in) and the
    /// store `database` to back up.
    ///
    /// No configuration path: the staged binary inherits this process's environment, so it reads the
    /// same `POS_EDGE_CONFIG` the running edge does. Passing one would let the smoke test check a
    /// file the real service never opens.
    ///
    /// Creating the directory and the initial `current` symlink is the operator's step, not this
    /// constructor's: a store whose `bin` directory does not exist has not been set up for
    /// over-the-air updates, and inventing the layout here would let a misconfigured box appear to
    /// be updatable while `systemd` still ran a binary from `/usr/local/bin`. [`Self::is_ready`]
    /// reports which of the two a box is.
    #[must_use]
    pub fn new(bin: PathBuf, database: PathBuf) -> Self {
        Self { bin, database }
    }

    /// Whether this box is laid out for over-the-air updates: `<state>/bin/current` exists and is
    /// the symlink `ExecStart` runs.
    ///
    /// A `false` here is the normal state of every store provisioned before ADR-0055 Amendment 1,
    /// and it is why the OTA loop is not spawned rather than left to fail on every tick.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.bin.join(CURRENT).symlink_metadata().is_ok()
    }

    /// The slot `current` points at, as a bare file name.
    fn live_slot(&self) -> Result<String, InstallError> {
        let link = self.bin.join(CURRENT);
        let target = std::fs::read_link(&link).map_err(|error| {
            InstallError::new(format!(
                "{} is not a readable symlink: {error}",
                link.display()
            ))
        })?;
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                InstallError::new(format!("{} points at an unusable name", link.display()))
            })?;
        if SLOTS.contains(&name) {
            Ok(name.to_owned())
        } else {
            Err(InstallError::new(format!(
                "{} points at {name}, which is not one of the two version slots",
                link.display()
            )))
        }
    }

    /// The slot `current` is *not* using — where the next version goes.
    fn spare_slot(&self) -> Result<&'static str, InstallError> {
        let live = self.live_slot()?;
        SLOTS
            .iter()
            .find(|slot| **slot != live)
            .copied()
            .ok_or_else(|| InstallError::new("both version slots are in use"))
    }

    /// Points `link` at `target` (a bare file name in the same directory) atomically: write a
    /// temporary symlink beside it, then [`std::fs::rename`] over it.
    ///
    /// The rename is what makes this safe. Removing the old link and creating a new one leaves a
    /// window in which `ExecStart` names nothing, and that window is exactly when a store loses
    /// power.
    fn point(&self, link: &str, target: &str) -> Result<(), InstallError> {
        let staging = self.bin.join(format!("{link}.swap"));
        let _ignored = std::fs::remove_file(&staging);
        link_at(target, &staging).map_err(|error| {
            InstallError::new(format!(
                "linking {} at {target} failed: {error}",
                staging.display()
            ))
        })?;
        std::fs::rename(&staging, self.bin.join(link)).map_err(|error| {
            InstallError::new(format!("swapping {link} to {target} failed: {error}"))
        })
    }

    /// Reads the boot marker and decides what this boot is: settled, unconfirmed, or past its
    /// allowance and therefore reverted.
    ///
    /// Call this **before** serving, once, and act on it: [`BootStanding::Reverted`] means the
    /// symlink and the database have already been moved back and the process must exit so the
    /// service manager starts the binary that worked.
    ///
    /// # Errors
    ///
    /// [`InstallError`] if the marker could not be read or written, or if the revert failed. A box
    /// that cannot tell whether it is on trial must not silently assume it is not.
    pub fn begin_boot(&self) -> Result<BootStanding, InstallError> {
        let marker = self.bin.join(UNCONFIRMED);
        let Ok(recorded) = std::fs::read_to_string(&marker) else {
            return Ok(BootStanding::Settled);
        };
        // An unreadable count is treated as the last allowed attempt rather than as zero: the marker
        // exists, so a version is on trial, and the cautious reading is the one that reverts.
        let attempt = recorded
            .trim()
            .parse::<u32>()
            .unwrap_or(MAX_UNCONFIRMED_BOOTS)
            .saturating_add(1);
        if attempt > MAX_UNCONFIRMED_BOOTS {
            self.revert_to_previous()?;
            let _ignored = std::fs::remove_file(&marker);
            return Ok(BootStanding::Reverted);
        }
        std::fs::write(&marker, attempt.to_string()).map_err(|error| {
            InstallError::new(format!("recording boot attempt {attempt} failed: {error}"))
        })?;
        Ok(BootStanding::Unconfirmed { attempt })
    }

    /// Clears the boot marker: this version came up and is trusted from now on.
    ///
    /// Idempotent, and a no-op on a box with no marker — which is every boot that did not follow an
    /// install.
    ///
    /// # Errors
    ///
    /// [`InstallError`] if the marker exists and could not be removed. Leaving it in place would
    /// count a healthy version's ordinary restarts towards a revert.
    pub fn clear_boot_marker(&self) -> Result<(), InstallError> {
        let marker = self.bin.join(UNCONFIRMED);
        match std::fs::remove_file(&marker) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(InstallError::new(format!(
                "clearing {} failed: {error}",
                marker.display()
            ))),
        }
    }

    /// Retargets `current` at `previous` and restores the pre-update database — the shared body of
    /// [`UpdateInstaller::rollback`] and the give-up arm of [`Self::begin_boot`].
    fn revert_to_previous(&self) -> Result<(), InstallError> {
        let previous = self.bin.join(PREVIOUS);
        let target = std::fs::read_link(&previous).map_err(|error| {
            InstallError::new(format!(
                "there is no {} to revert to: {error}",
                previous.display()
            ))
        })?;
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                InstallError::new(format!("{} points at an unusable name", previous.display()))
            })?;
        self.point(CURRENT, name)?;
        self.restore_backup()
    }

    /// Copies the `.pre-update` sidecar back over the live database, if one was staged.
    ///
    /// An absent sidecar is not an error: a rollback triggered by the boot marker on a box whose
    /// install predates the backup, or a revert with nothing to restore, should still put the old
    /// binary back rather than refuse the whole operation.
    fn restore_backup(&self) -> Result<(), InstallError> {
        let backup = self.backup_path();
        if !backup.exists() {
            return Ok(());
        }
        std::fs::copy(&backup, &self.database)
            .map(|_bytes| ())
            .map_err(|error| {
                InstallError::new(format!(
                    "restoring {} over {} failed: {error}",
                    backup.display(),
                    self.database.display()
                ))
            })
    }

    /// Where the pre-update database copy lives: the store database's own path plus a suffix, so it
    /// shares the filesystem and the sidecar cannot land on a volume with no room.
    fn backup_path(&self) -> PathBuf {
        let mut name = self.database.clone().into_os_string();
        name.push(PRE_UPDATE_SUFFIX);
        PathBuf::from(name)
    }

    /// Waits for `child` through at most [`SELF_TEST_POLLS`] polls, killing it and reporting failure
    /// if it overruns.
    ///
    /// A bounded poll loop rather than a blocking `wait`, because a `wait` with no timeout is how a
    /// hung staged binary becomes a hung store. Counting polls rather than reading a clock is
    /// deliberate: the elapsed wall time of a local process wait is not a fact the store's
    /// [`ClockSource`](pos_proto::ClockSource) owns — that port exists so a business date never
    /// comes from the host clock — and a fixed budget of `n` sleeps of a known length is the same
    /// bound with nothing to disagree about.
    #[expect(
        clippy::disallowed_methods,
        reason = "the install seam is synchronous by ADR-0055's explicit decision, so bounding a \
                  child process's wait has no async timer available; the blocking is one worker \
                  thread for at most SELF_TEST_TIMEOUT, once per release per store, on a loop that \
                  runs on its own task"
    )]
    fn wait_bounded(child: &mut std::process::Child) -> Result<bool, InstallError> {
        for _poll in 0..SELF_TEST_POLLS {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status.success()),
                Ok(None) => {}
                Err(error) => {
                    return Err(InstallError::new(format!(
                        "waiting for the staged binary failed: {error}"
                    )));
                }
            }
            std::thread::sleep(SELF_TEST_POLL);
        }
        let _ignored = child.kill();
        let _ignored = child.wait();
        Ok(false)
    }
}

impl UpdateInstaller for SystemdInstaller {
    fn stage_backup(&self) -> Result<(), InstallError> {
        let backup = self.backup_path();
        std::fs::copy(&self.database, &backup)
            .map(|_bytes| ())
            .map_err(|error| {
                InstallError::new(format!(
                    "copying {} to {} failed: {error}",
                    self.database.display(),
                    backup.display()
                ))
            })
    }

    /// Writes the verified bytes as `<state>/bin/staged`, mode 0755.
    ///
    /// Written to a temporary name, flushed to the platter, made executable, and only then renamed
    /// into place — so a `staged` file that exists is a whole file. A half-written one that a power
    /// cut left behind would otherwise be exec'd by the self-test, and "the download was truncated"
    /// and "this release is broken" are not the same diagnosis.
    fn apply(&self, artifact: &[u8]) -> Result<(), InstallError> {
        use std::io::Write as _;

        let staging = self.bin.join(format!("{STAGED}.part"));
        let mut file = std::fs::File::create(&staging).map_err(|error| {
            InstallError::new(format!("creating {} failed: {error}", staging.display()))
        })?;
        file.write_all(artifact)
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                InstallError::new(format!("writing {} failed: {error}", staging.display()))
            })?;
        drop(file);
        make_executable(&staging).map_err(|error| {
            InstallError::new(format!(
                "making {} executable failed: {error}",
                staging.display()
            ))
        })?;
        std::fs::rename(&staging, self.bin.join(STAGED)).map_err(|error| {
            InstallError::new(format!("staging {} failed: {error}", staging.display()))
        })
    }

    /// Runs the staged binary as `pos-edge --self-test` and reports whether it exited zero.
    ///
    /// A binary that cannot be spawned at all — the wrong architecture, a missing loader — is
    /// `Ok(false)` and not an error: that is precisely the release-is-wrong-for-this-box case this
    /// check exists to catch, and it is a routine rollback rather than a fault of the installer.
    fn self_test(&self) -> Result<bool, InstallError> {
        let staged = self.bin.join(STAGED);
        let spawned = std::process::Command::new(&staged)
            .arg("--self-test")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => Self::wait_bounded(&mut child),
            Err(error) => {
                tracing::warn!(
                    staged = %staged.display(),
                    %error,
                    "the staged binary could not be run; treating the self-test as failed"
                );
                Ok(false)
            }
        }
    }

    /// Promotes `staged` into the spare slot, remembers the outgoing version as `previous`, points
    /// `current` at the new one, and marks it unconfirmed.
    ///
    /// The order is the safety argument. `previous` is written *before* `current` moves, so a power
    /// cut between the two leaves a box that still runs the old binary and knows where to go back
    /// to; the reverse order would leave one that runs the new binary with no recorded predecessor.
    /// The marker is written last, because a marker without a committed version would revert a box
    /// that had installed nothing.
    ///
    /// It does not restart. The loop reports to the cloud first and then asks the process to drain
    /// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)): a box that restarted inside
    /// `commit` would install and reboot without the console ever hearing about it.
    fn commit(&self) -> Result<(), InstallError> {
        let outgoing = self.live_slot()?;
        let incoming = self.spare_slot()?;
        std::fs::rename(self.bin.join(STAGED), self.bin.join(incoming)).map_err(|error| {
            InstallError::new(format!("promoting the staged binary failed: {error}"))
        })?;
        self.point(PREVIOUS, &outgoing)?;
        self.point(CURRENT, incoming)?;
        std::fs::write(self.bin.join(UNCONFIRMED), "0").map_err(|error| {
            InstallError::new(format!(
                "marking the new version unconfirmed failed: {error}"
            ))
        })
    }

    /// Points `current` back at `previous` and restores the pre-update database.
    ///
    /// Also clears the boot marker: whatever was on trial is no longer running, so counting further
    /// boots against it would revert the binary that just came back.
    fn rollback(&self) -> Result<(), InstallError> {
        self.revert_to_previous()?;
        let _ignored = std::fs::remove_file(self.bin.join(STAGED));
        self.clear_boot_marker()
    }
}

impl BootConfirmation for SystemdInstaller {
    fn confirm_boot(&self) -> Result<(), InstallError> {
        self.clear_boot_marker()
    }
}

/// The default binary directory beside a store database: `<parent of the database>/bin`.
///
/// The service unit sets `WorkingDirectory` and `StateDirectory` to the same place and the database
/// path is relative to it, so deriving the binary directory from the database keeps one configured
/// path instead of two that can point at different volumes.
#[must_use]
pub fn binary_directory(database: &Path) -> PathBuf {
    database
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("bin")
}

#[cfg(all(test, unix))]
mod tests {
    use super::{
        BootStanding, MAX_UNCONFIRMED_BOOTS, SELF_TEST_POLL, SELF_TEST_POLLS, SELF_TEST_TIMEOUT,
        SystemdInstaller, UpdateInstaller, binary_directory,
    };
    use std::path::{Path, PathBuf};

    /// A laid-out box: a `bin` directory, both slots present, `current` on slot-a, and a database.
    ///
    /// `slot-a` is a shell script that exits zero, so [`UpdateInstaller::self_test`] has something
    /// real to exec — the seam's one OS call that a temporary directory can genuinely exercise.
    fn box_at(root: &Path) -> SystemdInstaller {
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        write_program(&bin.join("slot-a"), 0);
        std::os::unix::fs::symlink("slot-a", bin.join("current")).expect("current");
        let database = root.join("store.sqlite");
        std::fs::write(&database, b"the store as it was").expect("database");
        SystemdInstaller::new(bin, database)
    }

    /// Writes an executable shell script at `path` that exits with `code`.
    fn write_program(path: &Path, code: u8) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(path, format!("#!/bin/sh\nexit {code}\n")).expect("program");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("mode");
    }

    /// A fresh directory under the system temporary directory, named for the calling test.
    ///
    /// Hand-rolled rather than `tempfile` in the library's own unit tests so the module compiles
    /// with no dev-dependency of its own; the integration suite uses `tempfile`.
    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("pos-installer-{name}-{}", std::process::id()));
        let _ignored = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch");
        root
    }

    #[test]
    fn a_box_with_no_bin_directory_is_not_ready_for_over_the_air_updates() {
        let root = scratch("not-ready");
        let installer = SystemdInstaller::new(root.join("bin"), root.join("store.sqlite"));
        assert!(
            !installer.is_ready(),
            "a store provisioned before the layout existed must be detected, not failed on"
        );
    }

    #[test]
    fn an_install_lands_in_the_spare_slot_and_leaves_the_old_one_reachable() {
        let root = scratch("commit");
        let installer = box_at(&root);
        let bin = root.join("bin");

        installer.stage_backup().expect("backup");
        installer.apply(b"#!/bin/sh\nexit 0\n").expect("apply");
        assert!(
            bin.join("staged").exists(),
            "the verified bytes are staged before anything is swapped"
        );
        installer.commit().expect("commit");

        assert_eq!(
            std::fs::read_link(bin.join("current")).expect("current"),
            Path::new("slot-b"),
            "the new version goes in the slot the old one was not using"
        );
        assert_eq!(
            std::fs::read_link(bin.join("previous")).expect("previous"),
            Path::new("slot-a"),
            "and the old one stays reachable by name, which is what rollback needs"
        );
        assert!(
            !bin.join("staged").exists(),
            "the staged file is moved, not copied — two live copies could disagree"
        );
        assert_eq!(
            std::fs::read_to_string(bin.join("unconfirmed")).expect("marker"),
            "0",
            "a freshly committed version has made zero boot attempts"
        );
    }

    #[test]
    fn a_rollback_puts_back_both_the_binary_and_the_database() {
        let root = scratch("rollback");
        let installer = box_at(&root);
        let bin = root.join("bin");
        let database = root.join("store.sqlite");

        installer.stage_backup().expect("backup");
        installer.apply(b"#!/bin/sh\nexit 0\n").expect("apply");
        installer.commit().expect("commit");
        // The new version runs and migrates the database, as a real one would.
        std::fs::write(&database, b"migrated by the new version").expect("migrate");

        installer.rollback().expect("rollback");

        assert_eq!(
            std::fs::read_link(bin.join("current")).expect("current"),
            Path::new("slot-a"),
            "the binary that worked is what runs again"
        );
        assert_eq!(
            std::fs::read(&database).expect("database"),
            b"the store as it was",
            "and the database is the one that binary understands — a schema the old code cannot \
             read is the same outage as a broken binary"
        );
        assert!(
            !bin.join("unconfirmed").exists(),
            "nothing is on trial any more, so ordinary restarts must not count towards a revert"
        );
    }

    #[test]
    fn a_version_that_never_boots_healthy_reverts_itself_on_the_fourth_attempt() {
        // The failure this exists for: a release that starts, crash-loops, and can therefore never
        // record its own verdict. Without the marker the box sits there restarting until somebody
        // drives to the shop.
        let root = scratch("watchdog");
        let installer = box_at(&root);
        let bin = root.join("bin");

        installer.stage_backup().expect("backup");
        installer.apply(b"#!/bin/sh\nexit 0\n").expect("apply");
        installer.commit().expect("commit");

        for attempt in 1..=MAX_UNCONFIRMED_BOOTS {
            assert_eq!(
                installer.begin_boot().expect("boot"),
                BootStanding::Unconfirmed { attempt },
                "attempt {attempt} is still within the allowance"
            );
            assert_eq!(
                std::fs::read_link(bin.join("current")).expect("current"),
                Path::new("slot-b"),
                "and the new version is still what runs"
            );
        }

        assert_eq!(
            installer.begin_boot().expect("boot"),
            BootStanding::Reverted,
            "past the allowance the box puts itself back"
        );
        assert_eq!(
            std::fs::read_link(bin.join("current")).expect("current"),
            Path::new("slot-a"),
        );
        assert!(!bin.join("unconfirmed").exists());
        assert_eq!(
            installer.begin_boot().expect("boot"),
            BootStanding::Settled,
            "and the boot that follows the revert is an ordinary one"
        );
    }

    #[test]
    fn a_healthy_boot_clears_the_marker_so_restarts_never_accumulate() {
        let root = scratch("confirm");
        let installer = box_at(&root);

        installer.apply(b"#!/bin/sh\nexit 0\n").expect("apply");
        installer.commit().expect("commit");
        assert_eq!(
            installer.begin_boot().expect("boot"),
            BootStanding::Unconfirmed { attempt: 1 }
        );
        installer.clear_boot_marker().expect("confirm");

        // The regression this guards: an operator power-cycling a perfectly good store four times
        // must not trip the watchdog.
        for _restart in 0..10 {
            assert_eq!(installer.begin_boot().expect("boot"), BootStanding::Settled);
        }
        installer
            .clear_boot_marker()
            .expect("confirming twice is a no-op");
    }

    #[test]
    fn the_self_test_budget_is_the_timeout() {
        // Three constants describing one bound. Multiplied out here so a change to any of them that
        // makes them disagree fails the build rather than quietly lengthening or shortening the wait.
        assert_eq!(SELF_TEST_POLL * SELF_TEST_POLLS, SELF_TEST_TIMEOUT);
    }

    #[test]
    fn a_staged_binary_that_exits_nonzero_fails_its_self_test() {
        let root = scratch("self-test");
        let installer = box_at(&root);

        installer.apply(b"#!/bin/sh\nexit 0\n").expect("apply");
        assert!(
            installer.self_test().expect("run"),
            "a staged binary that answers --self-test with zero passes"
        );

        installer.apply(b"#!/bin/sh\nexit 9\n").expect("apply");
        assert!(
            !installer.self_test().expect("run"),
            "a nonzero exit is a routine rollback, not an error"
        );
    }

    #[test]
    fn bytes_that_cannot_be_executed_at_all_are_a_failed_self_test_not_a_fault() {
        // What a wrong-architecture download actually looks like: bytes that exec refuses. This must
        // never reach `commit`, and must not surface as an installer fault either — the installer
        // did its job.
        let root = scratch("unexecutable");
        let installer = box_at(&root);
        installer.apply(b"\x7fELF not really").expect("apply");
        assert!(!installer.self_test().expect("run"));
    }

    #[test]
    fn the_binary_directory_sits_beside_the_store_database() {
        assert_eq!(
            binary_directory(Path::new("/var/lib/pos-edge/store.sqlite")),
            PathBuf::from("/var/lib/pos-edge/bin"),
        );
        // A bare relative database name — the `EdgeConfig` default — has an empty parent, so the
        // binary directory is a bare `bin` that resolves against the same working directory the
        // unit sets for the database. Relative in, relative out; never an absolute guess.
        assert_eq!(
            binary_directory(Path::new("store.sqlite")),
            PathBuf::from("bin"),
        );
    }

    #[test]
    fn a_current_symlink_pointing_somewhere_unexpected_is_refused() {
        // Rather than guessing a slot: a `current` an operator repointed at
        // /usr/local/bin/pos-edge by hand is a box whose next commit would silently orphan whatever
        // is running.
        let root = scratch("stray-link");
        let installer = box_at(&root);
        let bin = root.join("bin");
        std::fs::remove_file(bin.join("current")).expect("unlink");
        std::os::unix::fs::symlink("/usr/local/bin/pos-edge", bin.join("current")).expect("link");
        installer.apply(b"#!/bin/sh\nexit 0\n").expect("apply");
        assert!(installer.commit().is_err());
    }
}
