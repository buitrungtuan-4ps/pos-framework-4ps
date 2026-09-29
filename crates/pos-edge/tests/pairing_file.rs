// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The pairing file means "a device can connect now" (ADR-0117 decision 3).
//!
//! The Windows installer waits for `POS_EDGE_PAIRING_FILE` to appear before it prints the pairing
//! URL and opens `/setup`, so the file must not exist before the port is bound. A box that cannot
//! bind must not write one at all: it would be a code for a server that is already exiting, printed
//! to a technician in place of the log line that says why.
//!
//! This runs the real binary, because the order that matters is inside `serve_until`, between
//! opening the store and handing the listener to axum, and no seam short of the process shows it.

use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::Duration;

#[tokio::test]
async fn a_box_that_cannot_bind_writes_no_pairing_file() {
    // Hold a port, then point the edge at it.
    let taken = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = taken.local_addr().expect("its address").port();
    let dir = tempfile::tempdir().expect("a scratch directory");
    let config = dir.path().join("config.toml");
    let pairing = dir.path().join("pairing-url.txt");
    // A literal string, so a Windows path's backslashes are not read as escapes.
    std::fs::write(
        &config,
        format!(
            "store_id = \"01M2MQ2BH6PKH6W4SEN2YVP9VT\"\n\
             cloud_url = \"https://cloud.invalid\"\n\
             bind = \"127.0.0.1:{port}\"\n\
             store_path = '{}'\n",
            dir.path().join("store.sqlite").display()
        ),
    )
    .expect("write the config");

    let mut edge = Command::new(env!("CARGO_BIN_EXE_pos-edge"))
        .env("POS_EDGE_CONFIG", &config)
        .env("POS_EDGE_PAIRING_FILE", &pairing)
        .env("RUST_LOG", "warn")
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start pos-edge");

    // It exits on the bind error. Bounded, so a regression that keeps it running fails here rather
    // than holding the test job until its timeout.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while edge.try_wait().expect("poll pos-edge").is_none() {
        if tokio::time::Instant::now() > deadline {
            let _killed = edge.kill();
            panic!("pos-edge was still running 60 seconds after it could not bind port {port}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let output = edge.wait_with_output().expect("collect what it said");
    let said = String::from_utf8_lossy(&output.stderr);

    // It must have stopped for the reason under test. A config it could not read would also exit
    // non-zero with no pairing file, and pass this test while proving nothing.
    assert!(
        !output.status.success() && said.contains("Bind"),
        "pos-edge should have exited on the bind error; it exited with {} and said:\n{said}",
        output.status
    );
    assert!(
        !pairing.exists(),
        "a box that could not bind port {port} wrote a pairing file, which the installer would \
         print as the URL of a server that is exiting"
    );
    drop(taken);
}
