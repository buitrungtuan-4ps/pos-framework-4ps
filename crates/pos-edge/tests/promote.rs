// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `pos-edge promote` puts the binary it is in place of the one a re-run of the installer finds
//! (ADR-0140 Amendment 1).
//!
//! This runs the real binary, because what the installer script depends on is the whole path: the
//! subcommand's dispatch, the configuration it finds the store by, the bytes it reads as its own,
//! and a staged copy of them passing `--self-test` against that same configuration.
//!
//! Unix only, like the slot tests in `installer.rs`: laying out `bin/current` takes a symlink, and
//! on Windows that needs a privilege a CI runner does not grant.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;

/// A box as a re-run finds it: `bin/current` on slot-a, a store database, and a config naming both.
fn laid_out_box(root: &Path) -> std::path::PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    // The older release. Never run here; only its place in the layout matters.
    std::fs::write(bin.join("slot-a"), "#!/bin/sh\nexit 0\n").expect("slot-a");
    std::os::unix::fs::symlink("slot-a", bin.join("current")).expect("current");
    let store = root.join("store.sqlite");
    std::fs::write(&store, b"the store as it was").expect("store");
    config_for(root, &store)
}

/// A config for `store`, written beside it.
fn config_for(root: &Path, store: &Path) -> std::path::PathBuf {
    let config = root.join("config.toml");
    // A literal string, so a path's backslashes are never read as escapes.
    std::fs::write(
        &config,
        format!(
            "store_id = \"01M2MQ2BH6PKH6W4SEN2YVP9VT\"\n\
             cloud_url = \"https://cloud.invalid\"\n\
             store_path = '{}'\n",
            store.display()
        ),
    )
    .expect("config");
    config
}

/// Runs `pos-edge promote` against `config`, returning whether it succeeded and what it said.
fn promote(root: &Path, config: &Path) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_pos-edge"))
        .arg("promote")
        .env("POS_EDGE_CONFIG", config)
        .env("RUST_LOG", "info")
        .current_dir(root)
        .output()
        .expect("run pos-edge promote");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), said)
}

#[test]
fn promote_puts_this_binary_in_place_of_the_version_a_re_run_finds() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let config = laid_out_box(dir.path());
    let bin = dir.path().join("bin");

    let (succeeded, said) = promote(dir.path(), &config);

    assert!(succeeded, "pos-edge promote failed:\n{said}");
    assert_eq!(
        std::fs::read_link(bin.join("current")).expect("current"),
        Path::new("slot-b"),
    );
    assert!(
        std::fs::read(bin.join("slot-b")).expect("slot-b")
            == std::fs::read(env!("CARGO_BIN_EXE_pos-edge")).expect("the binary"),
        "what went in is the program's own bytes, which is what the installer carried"
    );
    assert_eq!(
        std::fs::read_link(bin.join("previous")).expect("previous"),
        Path::new("slot-a"),
        "the release it replaced is where a revert goes back to"
    );
    assert!(
        bin.join("unconfirmed").exists(),
        "on trial until it comes up, as an over-the-air install is"
    );
    assert_eq!(
        std::fs::read(dir.path().join("store.sqlite.pre-update")).expect("backup"),
        b"the store as it was",
    );
}

#[test]
fn promote_on_a_box_with_nothing_installed_says_so_and_changes_nothing() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let config = config_for(dir.path(), &dir.path().join("store.sqlite"));

    let (succeeded, said) = promote(dir.path(), &config);

    assert!(
        !succeeded,
        "there was nothing to promote beside, yet it succeeded:\n{said}"
    );
    assert!(
        said.contains("no installed version"),
        "the refusal should say why:\n{said}"
    );
    assert!(!dir.path().join("bin").exists());
}
