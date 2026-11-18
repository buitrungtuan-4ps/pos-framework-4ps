// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! One cell's store: its configuration document, its in-process edge, and a till signed in to it.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use pos_business_types::Template;
use pos_core::capability::{Capability, CapabilityContext};
use pos_core::permission::{Permission, PermissionSet};
use pos_country::CountryModule;
use pos_edge::pairing::Minter;
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StaffAuth,
    StaffRoster, StoreIdentity, SystemClock,
};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::ids::{DeviceId, EmployeeId, StoreId};
use pos_proto::money::CurrencyCode;
use pos_proto::ulid::Ulid;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The fake badge code and PIN the cell's one member of staff signs in with.
const STAFF_CODE: &str = "SIM01";
const STAFF_PIN: &str = "9753";

/// A fixed salt, so the fake PIN's hash needs no random source. Never used for a real PIN.
const SALT: &[u8] = b"matrix-fake-salt";

/// A till paired with, and signed in to, one cell's edge.
pub(super) struct Till {
    router: Router,
    token: String,
    /// The currency the store trades in, from its country pack.
    pub(super) currency: CurrencyCode,
    /// Whether the store serves at tables.
    pub(super) tables: bool,
    /// Whether the store sends lines to a kitchen display.
    pub(super) kitchen: bool,
}

impl Till {
    /// Builds the cell's edge from `template` in `pack`'s country, pairs a till and signs it in.
    pub(super) async fn open(
        template: &Template,
        pack: &dyn CountryModule,
    ) -> Result<Self, String> {
        let preset = template
            .capabilities()
            .ok_or_else(|| "the template has no capability preset".to_owned())?;
        let currency = pack.locale_pack().currency_code;
        // The till pairs first, so the store can publish it as one of its tills, as the console does
        // when a device is approved.
        let pairing = Arc::new(Pairing::new());
        let now = SystemClock.now();
        let (code, _) = pairing
            .mint(now, Minter::Boot)
            .map_err(|error| format!("no pairing code: {error:?}"))?;
        let token = pairing
            .redeem(&code, now)
            .await
            .map_err(|error| format!("the pairing code does not redeem: {error:?}"))?
            .token()
            .ok_or_else(|| "a fresh code paired no device".to_owned())?;
        let device_id = pairing
            .device_for(&token)
            .ok_or_else(|| "the paired device has no id".to_owned())?;
        let token = token.as_str().to_owned();
        let document = document(template, pack, preset, device_id)?;
        let session =
            pos_edge::config_client::session_from_config(&EdgeSession::bootstrap(), &document)
                .with_staff(roster()?);
        let edge = Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(0x51_0000))),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .map_err(|error| format!("the edge does not start: {error}"))?;
        let router = pos_edge::http::domain_router(
            Arc::new(edge),
            InMemoryQueueNumbers::new(),
            Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
            pos_edge::print_queue::InMemoryPrintQueue::new(),
            pos_edge::print_wake::SharedPrintWake::new(),
            pairing,
            Arc::new(Sessions::new()),
            &Arc::new(pos_edge::origins::Origins::new()),
        );
        let till = Self {
            router,
            token,
            currency,
            tables: preset.enabled(Capability::Tables),
            kitchen: preset.enabled(Capability::Kds),
        };
        till.send(
            "POST",
            "/api/session/sign-in",
            Some(json!({ "code": STAFF_CODE, "pin": STAFF_PIN })),
        )
        .await?;
        Ok(till)
    }

    /// The badge code and PIN of somebody who may approve a comp: the cell's one member of staff,
    /// who, as the store does not enforce each person's own set, may approve their own.
    pub(super) const fn approver() -> (&'static str, &'static str) {
        (STAFF_CODE, STAFF_PIN)
    }

    /// Sends one request as the signed-in till; the answer's body when it succeeds (any `2xx`).
    pub(super) async fn send(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", self.token))
            .body(body)
            .map_err(|error| format!("{method} {uri} does not build: {error}"))?;
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .map_err(|error| format!("{method} {uri} got no answer: {error}"))?;
        let status = response.status();
        let reason = response
            .headers()
            .get("pos-error-reason")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|error| format!("{method} {uri} answer unreadable: {error}"))?
            .to_bytes();
        let answer = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status.is_success() {
            Ok(answer)
        } else {
            Err(format!(
                "{method} {uri} answered {status} {}: {answer}",
                reason.unwrap_or_default()
            ))
        }
    }
}

/// The configuration document the cloud would publish to a store made from `template` in `pack`'s
/// country, with `till` approved as its one till: the pack's locale and tax table, the template's
/// preset, recommended settings, and demo menu, floor and kitchen stations.
fn document(
    template: &Template,
    pack: &dyn CountryModule,
    preset: CapabilityContext,
    till: DeviceId,
) -> Result<Value, String> {
    let locale = pack.locale_pack();
    let menu = template
        .demo
        .menu(template.business_type, locale.currency_code)
        .ok_or_else(|| {
            format!(
                "the demo menu has no {} price",
                locale.currency_code.as_str()
            )
        })?;
    let mut document = json!({
        "locale": {
            "country_code": locale.country_code.as_str(),
            "currency_code": locale.currency_code.as_str(),
            "currency_exponent": locale.currency_exponent,
            // The business date is not what the matrix tests, so every cell keeps one clock.
            "timezone": "UTC",
            "cutoff_hour": 4,
            "prices_include_tax": locale.prices_include_tax,
            "cash_rounding_increment": locale.cash_rounding_increment,
            "cash_denominations": locale.cash_denominations,
            "cash_count_denominations": locale.cash_count_denominations,
            "default_retention_days": locale.default_retention_days,
            "number_format": locale.number_format,
        },
        "tax": locale.tax_rate_table,
        "menu": menu,
        "devices": { "devices": [{
            "device_id": till,
            "kind": "DEVICE_KIND_TERMINAL",
            "connection": "DEVICE_CONNECTION_UNSPECIFIED",
            "address": "",
            "name": "Demo till",
        }] },
    });
    let object = document
        .as_object_mut()
        .ok_or_else(|| "the document is not an object".to_owned())?;
    for capability in Capability::ALL {
        object.insert(
            capability.meta().key.to_owned(),
            Value::Bool(preset.enabled(*capability)),
        );
    }
    if let Some(floor) = template.demo.floor_plan(template.business_type) {
        object.insert(
            "floor".to_owned(),
            serde_json::to_value(floor).map_err(|error| error.to_string())?,
        );
    }
    if let Some(stations) = template.demo.station_plan(template.business_type) {
        object.insert(
            "stations".to_owned(),
            serde_json::to_value(stations).map_err(|error| error.to_string())?,
        );
    }
    for recommended in &template.recommended_settings {
        let (node, field) = recommended
            .setting_key
            .split_once('.')
            .ok_or_else(|| format!("{} is not node.field", recommended.setting_key))?;
        let node = object.entry(node.to_owned()).or_insert_with(|| json!({}));
        if let Some(fields) = node.as_object_mut() {
            fields.insert(field.to_owned(), recommended.value.clone());
        }
    }
    Ok(document)
}

/// The cell's one member of staff, with a fake badge and PIN.
fn roster() -> Result<StaffRoster, String> {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    let pin_phc = Argon2::default()
        .hash_password_with_salt(STAFF_PIN.as_bytes(), SALT)
        .map_err(|error| format!("the fake PIN does not hash: {error}"))?
        .to_string();
    let mut roster = StaffRoster::new();
    roster.insert(
        STAFF_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(0x51_0001))),
            // What they may approve: a comp (ADR-0219). The store does not enforce each person's own
            // set, so what they may do themselves is the framework's, whatever this says.
            permissions: PermissionSet::EMPTY.with(Permission::ApplyComp),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(pin_phc),
        },
    );
    Ok(roster)
}
