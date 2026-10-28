use super::{CloudConfig, InternalSecret, MIN_INTERNAL_SECRET_LEN, SigningSecret};

/// The shortest config that parses — the same two keys `parses_a_minimal_config` uses.
const MINIMAL_TOML: &str =
    "bind = \"127.0.0.1:8443\"\ndatabase_url = \"host=localhost user=pos dbname=poscloud\"\n";

/// A config that validates, so a test about one field is not also a test about the others.
fn valid() -> CloudConfig {
    let mut config = CloudConfig::from_toml(MINIMAL_TOML).expect("the minimal config parses");
    config.internal_shared_secret = Some(InternalSecret::new("a".repeat(MIN_INTERNAL_SECRET_LEN)));
    config
}

/// The rule ADR-0124's retention decision rests on. An archive is a store's whole database,
/// buyer details included, taken *before* the masking cron redacted them — so an archive window
/// at or past the subject window would leave the masking done in the row and undone in the
/// copy, with no symptom until somebody opened an archive. It is a boot refusal for exactly
/// that reason: nothing later would notice.
#[test]
fn archives_kept_as_long_as_subjects_refuse_the_boot() {
    for archive_days in [30_u32, 45] {
        let mut config = valid();
        config.retention_days = Some(30);
        config.archive_retention_days = archive_days;
        let error = config
            .validate()
            .expect_err("an archive window at or past the subject window is refused");
        assert!(
            error.contains("must be below retention_days"),
            "the message names both windows: {error}"
        );
    }
}

#[test]
fn an_archive_window_inside_the_subject_window_is_accepted() {
    let mut config = valid();
    config.retention_days = Some(30);
    config.archive_retention_days = 29;
    assert!(
        config.validate().is_ok(),
        "one day inside the subject window is the boundary, and it holds"
    );
}

/// With no subject window configured the masking cron is off entirely (ADR-0035), and there is
/// nothing for the archive window to be below. It still applies on its own: a bounded archive
/// is worth having whether or not anybody set a legal period.
#[test]
fn an_archive_window_stands_alone_when_no_subject_window_is_set() {
    let mut config = valid();
    config.retention_days = None;
    config.archive_retention_days = 3650;
    assert!(config.validate().is_ok());
}

#[test]
fn a_zero_archive_window_refuses_the_boot() {
    let mut config = valid();
    config.archive_retention_days = 0;
    let error = config.validate().expect_err("zero is refused");
    assert!(
        error.contains("archive_retention_days must be at least 1"),
        "the message says what zero would mean: {error}"
    );
}

#[test]
fn the_archive_window_defaults_to_a_month() {
    let config = CloudConfig::from_toml(MINIMAL_TOML).expect("the minimal config parses");
    assert_eq!(
        config.archive_retention_days, 30,
        "unlike retention_days, this has a default: a guessed archive window only decides how \
         far back a restore reaches, and keeping a month beats keeping nothing"
    );
    assert_eq!(
        config.archive_retention_sweep_interval_secs,
        24 * 60 * 60,
        "the archive sweep runs daily when unset"
    );
}

#[test]
fn an_alert_webhook_url_without_a_secret_refuses_the_boot() {
    // An unsigned alert batch is indistinguishable from one a stranger posted, and "every store
    // is offline" is a convincing thing to be told (ADR-0073 slice 4).
    let mut config = valid();
    config.alert_webhook_url = Some("https://ops.example.com/alerts".to_owned());
    let error = config.validate().expect_err("half a pair is refused");
    assert!(
        error.contains("alert_webhook_secret is required"),
        "the message names the missing field: {error}"
    );
}

#[test]
fn an_alert_secret_without_a_url_refuses_the_boot_rather_than_delivering_nowhere() {
    // The other half of the pair. Silently accepting this leaves an operator believing alerts
    // are being pushed when nothing is configured to receive them.
    let mut config = valid();
    config.alert_webhook_secret = Some(SigningSecret::new("a".repeat(MIN_INTERNAL_SECRET_LEN)));
    let error = config.validate().expect_err("half a pair is refused");
    assert!(
        error.contains("alert_webhook_url is not"),
        "the message names what is missing: {error}"
    );
}

#[test]
fn a_short_alert_secret_is_refused_by_length_and_never_quoted() {
    let mut config = valid();
    config.alert_webhook_url = Some("https://ops.example.com/alerts".to_owned());
    config.alert_webhook_secret = Some(SigningSecret::new("hunter2"));
    let error = config.validate().expect_err("too short");
    assert!(
        error.contains("at least") && error.contains("(got 7)"),
        "the length, not the value: {error}"
    );
    assert!(
        !error.contains("hunter2"),
        "a refusal that quoted the secret would put it in a log: {error}"
    );
}

#[test]
fn console_only_alerting_is_a_valid_posture() {
    // Neither field set. The in-console channel is the primary one and always runs; a deployment
    // that watches its console does not have to configure a push (ADR-0073).
    let config = valid();
    assert!(config.alert_webhook_url.is_none());
    config.validate().expect("console-only alerting validates");
}

#[test]
fn the_alert_secret_is_redacted_from_debug_output() {
    let secret = SigningSecret::new("a".repeat(MIN_INTERNAL_SECRET_LEN));
    let rendered = format!("{secret:?}");
    assert!(
        !rendered.contains(&"a".repeat(MIN_INTERNAL_SECRET_LEN)),
        "CloudConfig derives Debug, so a plaintext secret here would reach any log that dumps \
         the config: {rendered}"
    );
}

#[test]
fn a_missing_internal_secret_refuses_the_boot_and_names_the_field() {
    // Fail closed, and loudly. `CloudConfig` is not `deny_unknown_fields`, so a misspelled key
    // deserialises to `None` — if absence meant "authentication off", that typo would be a
    // silently open surface behind a file that reads as armed (ADR-0097).
    let mut config = valid();
    config.internal_shared_secret = None;
    let refusal = config
        .validate()
        .expect_err("an absent secret refuses the boot");
    assert!(
        refusal.contains("internal_shared_secret"),
        "the refusal must name the field to set: {refusal}"
    );
}

#[test]
fn a_too_short_internal_secret_is_refused_without_quoting_it() {
    let mut config = valid();
    config.internal_shared_secret = Some(InternalSecret::new("short"));
    let refusal = config
        .validate()
        .expect_err("a short secret refuses the boot");
    assert!(refusal.contains("at least"), "got {refusal}");
    assert!(
        !refusal.contains("short"),
        "the refusal reports the length, never the value: {refusal}"
    );
}

#[test]
fn the_internal_secret_is_redacted_from_debug() {
    let secret = InternalSecret::new("the-actual-secret-value");
    let rendered = format!("{secret:?}");
    assert!(
        !rendered.contains("the-actual-secret-value"),
        "got {rendered}"
    );
    assert_eq!(secret.expose(), "the-actual-secret-value");
}

#[test]
fn parses_a_minimal_config() {
    let config = CloudConfig::from_toml(
        "bind = \"127.0.0.1:8443\"\ndatabase_url = \"host=localhost user=pos dbname=poscloud\"\n",
    )
    .expect("valid config");
    assert_eq!(config.bind.port(), 8443);
    assert!(config.database_url.contains("dbname=poscloud"));
    assert!(config.nats.is_none(), "no [nats] means the cursor is off");
    assert_eq!(
        config.projector_interval_secs, 30,
        "the projector interval defaults when unset"
    );
    assert_eq!(
        config.admin_session_ttl_secs,
        8 * 60 * 60,
        "the admin session absolute cap defaults to eight hours when unset"
    );
    assert_eq!(
        config.admin_session_idle_ttl_secs,
        30 * 60,
        "the admin session idle timeout defaults to thirty minutes when unset"
    );
    assert_eq!(
        config.admin_login_max_attempts, 10,
        "the admin login rate limit defaults to ten attempts when unset"
    );
    assert_eq!(
        config.admin_login_window_secs,
        5 * 60,
        "the admin login rate-limit window defaults to five minutes when unset"
    );
    assert_eq!(
        config.retention_days, None,
        "with no configured retention period the cron stays off — never a code default"
    );
    assert_eq!(
        config.admin_setup_token, None,
        "with no configured setup token the /admin/setup route stays off"
    );
    assert_eq!(
        config.retention_sweep_interval_secs,
        24 * 60 * 60,
        "the retention sweep interval defaults to daily when unset"
    );
    assert_eq!(
        config.webhook_dispatch_interval_secs, 10,
        "the webhook dispatch interval defaults to ten seconds when unset"
    );
    assert_eq!(
        config.webhook_delivery_timeout_secs, 10,
        "the webhook delivery timeout defaults to ten seconds when unset"
    );
    assert_eq!(
        config.table_token_secret, None,
        "with no configured QR secret the /v1/qr/orders endpoint stays off"
    );
    assert!(
        config.metrics.is_none(),
        "no [metrics] means the monitoring profile is off — sparse-sampling posture below ~50 stores"
    );
    assert_eq!(
        config.trusted_proxy_hops, 1,
        "one trusted proxy — the bundled Caddy — when the posture does not say otherwise"
    );
    // The `/internal` secret is required (ADR-0097) and is not what this test is about, so it
    // is supplied rather than asserted on.
    let mut config = config;
    config.internal_shared_secret = Some(InternalSecret::new("a".repeat(MIN_INTERNAL_SECRET_LEN)));
    config
        .validate()
        .expect("the defaults are a valid configuration");
}

#[test]
fn the_external_tls_posture_configures_two_trusted_hops() {
    // What bootstrap.sh writes for TLS_MODE=external (ADR-0090): a load balancer terminates TLS
    // in front of Caddy, so the chain is client, balancer, caddy and the client is two back.
    let config = CloudConfig::from_toml(
        "bind = \"127.0.0.1:8443\"\n\
         database_url = \"host=localhost dbname=poscloud\"\n\
         trusted_proxy_hops = 2\n",
    )
    .expect("valid config");
    assert_eq!(config.trusted_proxy_hops, 2);
    let mut config = config;
    config.internal_shared_secret = Some(InternalSecret::new("a".repeat(MIN_INTERNAL_SECRET_LEN)));
    config.validate().expect("two hops is valid");
}

#[test]
fn zero_trusted_hops_is_refused_rather_than_treated_as_cautious() {
    // It parses — usize accepts it — and it would silently disable the login rate limit's
    // per-client keying, putting every caller in one bucket. Refusing at load is the only
    // reading of it that is not a denial of service disguised as prudence.
    let config = CloudConfig::from_toml(
        "bind = \"127.0.0.1:8443\"\n\
         database_url = \"host=localhost dbname=poscloud\"\n\
         trusted_proxy_hops = 0\n",
    )
    .expect("it parses; the range check is separate");
    let error = config
        .validate()
        .expect_err("zero trusted hops must not boot");
    assert!(
        error.contains("trusted_proxy_hops"),
        "the message names the field: {error}"
    );
}

#[test]
fn parses_the_metrics_section_with_defaults() {
    let config = CloudConfig::from_toml(
        "bind = \"127.0.0.1:8443\"\n\
         database_url = \"host=localhost dbname=poscloud\"\n\
         [metrics]\n\
         url = \"http://127.0.0.1:8428\"\n",
    )
    .expect("valid config");
    let metrics = config.metrics.expect("a [metrics] section");
    assert_eq!(metrics.url, "http://127.0.0.1:8428");
    assert_eq!(
        metrics.sample_interval_secs, 60,
        "the sample interval defaults to 60s (sparse-sampling cadence)"
    );
}

#[test]
fn parses_the_nats_cursor_section_with_defaults() {
    let config = CloudConfig::from_toml(
        "bind = \"127.0.0.1:8443\"\n\
         database_url = \"host=localhost dbname=poscloud\"\n\
         [nats]\n\
         url = \"127.0.0.1:4222\"\n\
         stream = \"POS_FLEET\"\n\
         durable = \"cloud_ingest\"\n",
    )
    .expect("valid config");
    let nats = config.nats.expect("a [nats] section");
    assert_eq!(nats.stream, "POS_FLEET");
    assert_eq!(nats.durable, "cloud_ingest");
    assert!(
        nats.filter_subject.is_empty(),
        "no filter means all subjects"
    );
    assert_eq!(nats.batch, 256, "the batch default");
    assert_eq!(nats.expires_secs, 5, "the expiry default");
}

// --- pinned before `CloudConfig::validate` is split (ADR-0170 decision 7) -----------------------

/// An archive key that is not a 32-byte key in hex refuses the boot, naming the field and never
/// quoting the value; a well-formed one, or none, is accepted.
#[test]
fn an_archive_key_must_be_a_hex_key_or_absent() {
    let mut config = valid();
    config.archive_key_secret = Some(crate::archive::ArchiveSecret::new("hunter2-secret"));
    let error = config.validate().expect_err("a passphrase is refused");
    assert!(
        error.starts_with("archive_key_secret must be exactly 64 hexadecimal"),
        "{error}"
    );
    assert!(!error.contains("hunter2"), "{error}");

    config.archive_key_secret = Some(crate::archive::ArchiveSecret::new("ab".repeat(32)));
    assert!(config.validate().is_ok());
    config.archive_key_secret = None;
    assert!(config.validate().is_ok());
}

/// A release source names its repository as `owner/name` and reaches its forge over https.
#[test]
fn a_release_source_is_owner_slash_name_over_https() {
    let with_source = |repository: &str, api_base: &str| {
        let mut config = valid();
        config.release_source = Some(super::ReleaseSourceConfig {
            repository: repository.to_owned(),
            token: None,
            api_base: api_base.to_owned(),
        });
        config.validate()
    };
    assert!(with_source("pizza4ps/pos", "https://api.github.com").is_ok());
    for repository in ["pizza4ps", "pizza4ps/", "/pos", "a/b/c", ""] {
        let error = with_source(repository, "https://api.github.com").expect_err(repository);
        assert!(
            error.starts_with(&format!(
                "release_source.repository must be `owner/name` (got `{repository}`)"
            )),
            "{error}"
        );
    }
    let error = with_source("pizza4ps/pos", "http://forge.internal").expect_err("plain http");
    assert!(
        error.starts_with(
            "release_source.api_base must be an https:// URL (got `http://forge.internal`)"
        ),
        "{error}"
    );
}

/// The checks run in a fixed order, so the operator reading a refused boot is told the first
/// thing to fix: the proxy hops, the internal secret, the archive key, the archive window, the
/// alert pair, then the release source.
#[test]
fn the_first_failing_check_is_the_one_reported() {
    let mut config = valid();
    config.trusted_proxy_hops = 0;
    config.internal_shared_secret = None;
    config.archive_key_secret = Some(crate::archive::ArchiveSecret::new("short"));
    config.archive_retention_days = 0;
    config.release_source = Some(super::ReleaseSourceConfig {
        repository: "bad".to_owned(),
        token: None,
        api_base: "http://x".to_owned(),
    });
    let mut seen = Vec::new();
    for _ in 0..6 {
        let Err(error) = config.validate() else {
            break;
        };
        let field: String = error.chars().take_while(|c| *c != ' ').collect();
        seen.push(field.clone());
        match field.as_str() {
            "trusted_proxy_hops" => config.trusted_proxy_hops = 1,
            "internal_shared_secret" => {
                config.internal_shared_secret =
                    Some(InternalSecret::new("a".repeat(MIN_INTERNAL_SECRET_LEN)));
            }
            "archive_key_secret" => config.archive_key_secret = None,
            "archive_retention_days" => config.archive_retention_days = 1,
            "release_source.repository" => {
                if let Some(source) = config.release_source.as_mut() {
                    source.repository = "a/b".to_owned();
                }
            }
            "release_source.api_base" => config.release_source = None,
            other => panic!("an unexpected refusal: {other}: {error}"),
        }
    }
    assert_eq!(
        seen,
        [
            "trusted_proxy_hops",
            "internal_shared_secret",
            "archive_key_secret",
            "archive_retention_days",
            "release_source.repository",
            "release_source.api_base",
        ]
    );
    assert!(config.validate().is_ok());
}

/// No `[cell]` section is level 0 of ADR-0179: the boot carries on as the one default cell.
#[test]
fn no_cell_section_is_the_default_cell() {
    let config = valid();
    assert!(config.validate().is_ok());
    assert_eq!(
        config.cell().expect("absent resolves"),
        crate::cell::Cell::single()
    );
}

/// A `[cell]` section is checked at boot: a malformed one stops the process by name.
#[test]
fn a_cell_section_is_read_and_checked_at_boot() {
    let text = format!(
        "{MINIMAL_TOML}internal_shared_secret = \"{}\"\n[cell]\ncell_id = \"vn1\"\n\
         home_country = \"VN\"\nbase_url = \"https://vn1.example.com\"\nruns_in = \"VN\"\n",
        "a".repeat(MIN_INTERNAL_SECRET_LEN)
    );
    let config = CloudConfig::from_toml(&text).expect("the section parses");
    assert!(config.validate().is_ok());
    assert_eq!(config.cell().expect("resolves").cell_id, "vn1");

    let broken = text.replace("\"vn1\"", "\"VN\"");
    let error = CloudConfig::from_toml(&broken)
        .expect("parses")
        .validate()
        .expect_err("a country is not a cell id");
    assert!(error.contains("cell_id"), "{error}");
}

/// A `[federation]` section is checked at boot: a source that cannot be read stops the process by
/// name, and no section is no federation (ADR-0179 C10).
#[test]
fn a_federation_section_is_checked_at_boot() {
    assert!(valid().federation.is_none());
    let text = format!(
        "{MINIMAL_TOML}internal_shared_secret = \"{}\"\n[[federation.sources]]\ncell_id = \"jp1\"\n\
         api_key = \"pos_k1_secret\"\nstores = [\"01M2219QK4T3W6Z0Y8FBQ2X5MV\"]\n",
        "a".repeat(MIN_INTERNAL_SECRET_LEN)
    );
    let config = CloudConfig::from_toml(&text).expect("the section parses");
    assert!(config.validate().is_ok());
    assert!(
        !format!("{config:?}").contains("pos_k1_secret"),
        "the key never reaches a Debug"
    );
    let broken = text.replace("01M2219QK4T3W6Z0Y8FBQ2X5MV", "store-1");
    let error = CloudConfig::from_toml(&broken)
        .expect("parses")
        .validate()
        .expect_err("a store that is not a ULID");
    assert!(error.contains("[federation]"), "{error}");
}
