// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `pos-cloud` binary boots, serves and stops the way an operator sees it: the exit code and
//! the log lines, in order.
//!
//! These run the real binary, because what they pin is the order of a boot that `main.rs` composes
//! and nothing short of the process shows. The refusals need no database and always run. The full
//! boot needs a PostgreSQL to migrate, so it is gated behind the `integration` feature with the
//! database the `store-postgres` suite uses:
//!
//! ```text
//! DATABASE_URL="host=localhost port=5432 user=pos password=pos dbname=poscloud" \
//!     cargo test -p pos-cloud --features integration --test boot
//! ```

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::time::Instant;

/// A shared secret long enough for the `/internal` routes (ADR-0097).
const SECRET: &str = "0123456789abcdef0123456789abcdef";

/// The `pos-cloud` binary, wherever the test runner put it.
fn pos_cloud_bin() -> std::ffi::OsString {
    std::env::var_os("NEXTEST_BIN_EXE_pos-cloud")
        .or_else(|| std::env::var_os("NEXTEST_BIN_EXE_pos_cloud"))
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_pos-cloud").into())
}

/// A scratch directory that removes itself, so this suite needs no new dependency.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pos-cloud-boot-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Self(path)
    }

    /// A configuration file here for `database_url`, binding `bind`, with `extra` lines.
    fn config(&self, bind: &str, database_url: &str, extra: &str) -> PathBuf {
        let path = self.0.join("cloud.toml");
        std::fs::write(
            &path,
            format!(
                "bind = \"{bind}\"\ndatabase_url = \"{database_url}\"\n\
                 internal_shared_secret = \"{SECRET}\"\n{extra}"
            ),
        )
        .expect("write the config");
        path
    }

    /// Starts the binary, configured by `config` (or by nothing at all), writing plain log lines
    /// to a file here rather than to a pipe nobody drains.
    fn start(&self, config: Option<&Path>) -> Child {
        let log = std::fs::File::create(self.0.join("said.log")).expect("a log file");
        let mut command = Command::new(pos_cloud_bin());
        command
            .env_remove("POS_CLOUD_CONFIG")
            .env("RUST_LOG", "info")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("the log file"))
            .stderr(log);
        if let Some(config) = config {
            command.env("POS_CLOUD_CONFIG", config);
        }
        command.spawn().expect("start pos-cloud")
    }

    /// Everything the process wrote, with any terminal colouring taken out.
    fn said(&self) -> String {
        let text = std::fs::read_to_string(self.0.join("said.log")).unwrap_or_default();
        let mut plain = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for end in chars.by_ref() {
                    if end == 'm' {
                        break;
                    }
                }
            } else {
                plain.push(c);
            }
        }
        plain
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

/// Waits for `child` to exit, failing rather than hanging if it never does.
async fn exited(child: &mut Child, within: Duration) -> ExitStatus {
    let deadline = Instant::now() + within;
    loop {
        if let Some(status) = child.try_wait().expect("poll pos-cloud") {
            return status;
        }
        if Instant::now() > deadline {
            let _killed = child.kill();
            panic!("pos-cloud was still running after {within:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Runs the binary on `config` to its exit, and asserts it failed to start, said so, and said
/// `why`.
async fn refused(config: Option<&Path>, scratch: &Scratch, why: &str) {
    let mut child = scratch.start(config);
    let status = exited(&mut child, Duration::from_secs(60)).await;
    let text = scratch.said();
    assert_eq!(status.code(), Some(1), "exit code; it said:\n{text}");
    assert!(
        text.contains("pos_cloud failed to start"),
        "the refusal is logged; it said:\n{text}"
    );
    assert!(
        text.contains(why),
        "the refusal names {why:?}; it said:\n{text}"
    );
    assert!(
        !text.contains("pos_cloud listening"),
        "nothing binds; it said:\n{text}"
    );
}

#[tokio::test]
async fn a_boot_with_no_configuration_named_fails_and_says_why() {
    let scratch = Scratch::new();
    refused(
        None,
        &scratch,
        "POS_CLOUD_CONFIG must name a configuration file",
    )
    .await;
}

#[tokio::test]
async fn a_boot_with_a_configuration_it_cannot_honour_fails_before_any_database() {
    let scratch = Scratch::new();
    let config = scratch.config(
        "127.0.0.1:0",
        "host=127.0.0.1 port=1 user=nobody dbname=nothing",
        "trusted_proxy_hops = 0\n",
    );
    refused(
        Some(&config),
        &scratch,
        "trusted_proxy_hops must be at least 1",
    )
    .await;
}

#[tokio::test]
async fn a_boot_whose_database_cannot_be_reached_fails() {
    let scratch = Scratch::new();
    let config = scratch.config(
        "127.0.0.1:0",
        "host=127.0.0.1 port=1 user=nobody dbname=nothing connect_timeout=5",
        "",
    );
    refused(Some(&config), &scratch, "pos_cloud failed to start").await;
}

#[cfg(all(feature = "integration", unix))]
mod full_boot {
    use std::fmt::Write as _;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    use tokio::time::Instant;

    use super::{Scratch, exited};

    /// The boot's own log lines, in the order a boot with no optional section says them. (The QR
    /// switch's one-off report is said only on the first boot over a database, so it is not here;
    /// nor are the background loops' own stop lines, which race the exit.)
    const BOOT_ORDER: &[&str] = &[
        "no [nats] config; ingest cursor off (reconciliation re-push only), and the fleet \
         stream's ceiling is left as the stores created it",
        "rollup projector started",
        "no retention_days configured; the PII-masking cron is off",
        "webhook dispatcher started",
        "alert evaluator started",
        "scheduled-publish activator started",
        "people republisher started",
        "no [metrics] configured; the monitoring profile is off",
        "pos_cloud listening",
        "no [artifacts] configured; the OTA artifact route is off and stores cannot fetch releases",
        "no [release_source] configured; a release artifact reaches the cloud by upload only",
        "cloud.toml has no integration_secret",
        "store archives are off",
        "no table_token_secret configured; the QR ordering endpoint is off",
    ];

    /// `GET /health`, answered or not.
    fn health(port: u16) -> Option<String> {
        request(port, "GET", "/health", &[], "")
    }

    /// One request, answered or not: the whole answer, status line first.
    fn request(
        port: u16,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Option<String> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        );
        for (name, value) in headers {
            let _written = write!(head, "{name}: {value}\r\n");
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).ok()?;
        stream.write_all(body.as_bytes()).ok()?;
        let mut answer = String::new();
        stream.read_to_string(&mut answer).ok()?;
        Some(answer)
    }

    /// The status code of `answer`.
    fn status(answer: &str) -> &str {
        answer.split(' ').nth(1).unwrap_or_default()
    }

    /// The routes this boot leaves unserved: the OTA artifact and store-archive routes need an
    /// object store, and guest QR ordering a table-token secret, none of which the default boot
    /// configures.
    const UNSERVED_BY_DEFAULT: &[&str] = &[
        "GET /admin/floor/qr",
        "GET /admin/ota/releases/{release}",
        "GET /admin/ota/releases/{release}/installer",
        "POST /admin/ota/releases",
        "POST /admin/ota/releases/fetch",
        "POST /sync/stores/{store_id}/archive",
        "POST /sync/stores/{store_id}/archive-key",
        "POST /sync/stores/{store_id}/artifact",
        "POST /v1/qr/orders",
    ];

    /// Every `.route(` registered in the cloud's router sources, as `METHOD /path`, read from the
    /// tree the way `pos_cloud::openapi_admin` reads it: the first string literal after `.route(`,
    /// then each method constructor in the same call.
    fn registered_routes() -> Vec<String> {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&crates)
            .expect("the crates directory")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name == "pos-cloud" || name.starts_with("pos-cloud-"))
            })
            .map(|path| path.join("src"))
            .collect();
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    files.push(path);
                }
            }
        }
        let mut routes = std::collections::BTreeSet::new();
        for file in files {
            let source = std::fs::read_to_string(&file).unwrap_or_default();
            // Test modules build routers of their own; only the shipped ones count.
            let shipped = source.split("#[cfg(test)]").next().unwrap_or_default();
            for call in shipped.split(".route(").skip(1) {
                let Some(open) = call.find('"') else { continue };
                let rest = call.get(open + 1..).unwrap_or_default();
                let Some(close) = rest.find('"') else {
                    continue;
                };
                let path = rest.get(..close).unwrap_or_default();
                if !path.starts_with('/') {
                    continue;
                }
                // The method constructors up to where this call's parentheses close.
                let mut depth = 1_i32;
                let mut end = call.len();
                for (at, c) in call.char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = at;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let body = call.get(..end).unwrap_or_default();
                for method in ["get", "post", "put", "patch", "delete"] {
                    let needle = format!("{method}(");
                    let mut from = 0;
                    while let Some(at) = body.get(from..).and_then(|rest| rest.find(&needle)) {
                        let start = from + at;
                        let before = body.get(..start).and_then(|b| b.chars().last());
                        if !before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                            routes.insert(format!("{} {path}", method.to_uppercase()));
                            break;
                        }
                        from = start + needle.len();
                    }
                }
            }
        }
        routes.into_iter().collect()
    }

    /// Every registered route is matched by the running cloud: an unauthenticated request is
    /// refused by the route (401, 403, 400, …), never answered "no such route" (404) or "not this
    /// method" (405).
    fn probe_served_routes(port: u16) {
        let mut unserved = Vec::new();
        for route in registered_routes() {
            let (method, path) = route.split_once(' ').expect("METHOD /path");
            let concrete: Vec<&str> = path
                .split('/')
                .map(|segment| {
                    if segment.starts_with('{') {
                        "01J0000000000000000000000A"
                    } else {
                        segment
                    }
                })
                .collect();
            let answer = request(port, method, &concrete.join("/"), &[], "{}").expect("an answer");
            if matches!(status(&answer), "404" | "405") {
                unserved.push(route);
            }
        }
        assert_eq!(unserved, UNSERVED_BY_DEFAULT);
    }

    /// What the running cloud was configured with, seen from outside: the `/internal` secret it
    /// holds, and the sign-in throttle it applies.
    fn probe_configuration(port: u16) {
        let ingest = |key: &str| {
            let answer = request(
                port,
                "POST",
                "/internal/ingest",
                &[("X-Pos-Internal-Key", key)],
                "[]",
            )
            .expect("an answer");
            status(&answer).to_owned()
        };
        assert_ne!(
            ingest(super::SECRET),
            "404",
            "the configured secret opens /internal"
        );
        assert_eq!(
            ingest("not-the-secret"),
            "404",
            "any other key finds no route"
        );

        let login = || {
            let answer = request(
                port,
                "POST",
                "/admin/login",
                &[],
                r#"{"email":"nobody@example.invalid","password":"not-a-password"}"#,
            )
            .expect("an answer");
            status(&answer).to_owned()
        };
        assert_ne!(login(), "429", "the first sign-in attempt is weighed");
        assert_eq!(
            login(),
            "429",
            "admin_login_max_attempts = 1 throttles the second"
        );
    }

    /// The lines that change when retention and guest QR ordering are configured, in order.
    const WITH_OPTIONS_ORDER: &[&str] = &[
        "no [nats] config; ingest cursor off",
        "rollup projector started",
        "retention/PII-masking cron started",
        "webhook dispatcher started",
        "pos_cloud listening",
        "QR ordering enabled (POST /v1/qr/orders)",
    ];

    #[tokio::test]
    async fn a_configured_cloud_migrates_serves_and_stops_cleanly_on_an_interrupt() {
        let text = boot_serve_and_interrupt("admin_login_max_attempts = 1\n", |port| {
            // The configuration first: the route walk signs in once, which would spend the one
            // sign-in attempt the throttle allows.
            probe_configuration(port);
            probe_served_routes(port);
        })
        .await;
        said_in_order(&text, BOOT_ORDER);
    }

    #[tokio::test]
    async fn a_cloud_with_retention_and_guest_ordering_starts_them_in_their_place() {
        let text = boot_serve_and_interrupt(
            "retention_days = 400\ntable_token_secret = \"a-table-token-secret-for-the-boot-test\"\n",
            |_port| {},
        )
        .await;
        said_in_order(&text, WITH_OPTIONS_ORDER);
        assert!(
            !text.contains("no retention_days configured"),
            "it said:\n{text}"
        );
        assert!(
            !text.contains("no table_token_secret configured"),
            "it said:\n{text}"
        );
    }

    /// Each of `lines` said after the one before it.
    fn said_in_order(text: &str, lines: &[&str]) {
        let mut from = 0;
        for line in lines {
            let Some(at) = text.get(from..).and_then(|rest| rest.find(line)) else {
                panic!("{line:?} is not said after the lines before it; it said:\n{text}");
            };
            from += at + line.len();
        }
    }

    /// How long an interrupted cloud may take to stop. Every background loop hears the same
    /// interrupt, and once the server has stopped the boot aborts any loop still running rather
    /// than waiting out its interval, so a clean stop takes seconds, not a loop period.
    const STOPS_WITHIN: Duration = Duration::from_secs(10);

    /// One full boot at a time. Two boots migrating a fresh database at once race on its
    /// `CREATE … IF NOT EXISTS` statements, and the loser refuses to start: true of the binary,
    /// and not what these tests are about.
    static ONE_BOOT_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Boots the binary over `DATABASE_URL` with `extra` configuration, waits for `/health`, runs
    /// `probe` against its port, stops it with an interrupt, and answers what it said once it has
    /// exited cleanly.
    async fn boot_serve_and_interrupt(extra: &str, probe: impl FnOnce(u16)) -> String {
        let _one_at_a_time = ONE_BOOT_AT_A_TIME.lock().await;
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL names the test database");
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("a free port")
            .local_addr()
            .expect("its address")
            .port();
        let scratch = Scratch::new();
        let config = scratch.config(&format!("127.0.0.1:{port}"), &url, extra);
        let mut child = scratch.start(Some(&config));

        let deadline = Instant::now() + Duration::from_secs(120);
        let answer = loop {
            if let Some(answer) = health(port) {
                break answer;
            }
            assert!(
                child.try_wait().expect("poll").is_none(),
                "pos-cloud exited before it served; it said:\n{}",
                scratch.said()
            );
            if Instant::now() > deadline {
                let _killed = child.kill();
                panic!("pos-cloud did not serve /health within 120 seconds");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert!(answer.starts_with("HTTP/1.1 200"), "got {answer}");
        probe(port);

        let interrupted = std::process::Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .expect("send SIGINT");
        assert!(interrupted.success());
        let status = exited(&mut child, STOPS_WITHIN).await;
        let text = scratch.said();
        assert_eq!(status.code(), Some(0), "a clean stop; it said:\n{text}");
        assert!(
            !text.contains("pos_cloud failed to start"),
            "it said:\n{text}"
        );
        text
    }
}
