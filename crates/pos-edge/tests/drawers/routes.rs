// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The drawers over the shipped routes.

use super::assigned_drawers::{assigned, employee};
use super::*;

// --- Over the shipped routes ------------------------------------------------------------------

async fn send(
    app: Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Option<String>, Value) {
    let body = body.map_or_else(Body::empty, |value| Body::from(value.to_string()));
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(body)
        .expect("request builds");
    let response = app.oneshot(request).await.expect("router responds");
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
        .expect("body")
        .to_bytes();
    (
        status,
        reason,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A binding record that cannot be read.
struct Unreadable;

impl AgentDispatch for Unreadable {
    fn enqueue<'a>(
        &'a self,
        _agent: DeviceId,
        _printer: DeviceId,
        _job: PrintJob,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>> {
        Box::pin(async { PrintOutcome::AgentUnavailable })
    }

    fn terminal_of<'a>(
        &'a self,
        _device: DeviceId,
    ) -> Pin<Box<dyn Future<Output = Result<Option<DeviceId>, PortError>> + Send + 'a>> {
        Box::pin(async {
            Err(PortError::unavailable(
                PortName::EventStore,
                "the binding cannot be read",
            ))
        })
    }
}

/// What each printer was sent, by address, in order: with no receipt printed on a settle, a
/// drawer's kick and a shift's report are all a printer is sent.
#[derive(Debug, Default)]
struct Written(std::sync::Mutex<Vec<(String, Vec<u8>)>>);

impl Written {
    /// The addresses written to, one per job, in order.
    fn at(&self) -> Vec<String> {
        let sent = self.0.lock().expect("the printers");
        sent.iter().map(|(address, _)| address.clone()).collect()
    }

    /// Whether the last job sent to `address` says `needle`.
    fn last_says(&self, address: &str, needle: &str) -> bool {
        let sent = self.0.lock().expect("the printers");
        sent.iter()
            .rev()
            .find(|(at, _)| at == address)
            .is_some_and(|(_, bytes)| {
                bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes())
            })
    }
}

#[derive(Debug)]
struct WrittenAt(Arc<Written>);

#[derive(Debug)]
struct WriteTransport(Arc<Written>, String);

impl Transport for WriteTransport {
    fn write(&self, bytes: &[u8]) -> Result<(), Unreachable> {
        self.0
            .0
            .lock()
            .map_err(|_| Unreachable)?
            .push((self.1.clone(), bytes.to_vec()));
        Ok(())
    }

    fn probe(&self) -> Result<TransportStatus, Unreachable> {
        Ok(TransportStatus::default())
    }
}

impl TransportFactory for WrittenAt {
    fn open(&self, device: &PublishedDevice) -> Result<Box<dyn Transport>, PortError> {
        Ok(Box::new(WriteTransport(
            Arc::clone(&self.0),
            device.address.clone(),
        )))
    }
}

/// The store over the shipped routes, and the devices signed in on it.
struct Tills {
    app: Router,
    edge: Arc<Edge<FakeStore>>,
    written: Arc<Written>,
    /// A device bound to the bar's till, one bound to the counter's, and one bound to none.
    bar: String,
    counter: String,
    stranger: String,
}

/// The store over the shipped routes, keeping a drawer per till: a device bound to the bar's till,
/// one bound to the counter's, and one bound to none, each signed in by the manager. The binding is
/// the print agents' record, as `serve` composes it, or one that cannot be read where `unreadable`.
async fn drawers_app(unreadable: bool) -> Tills {
    let edge = Arc::new(edge_over(
        FakeStore::new(),
        session(DrawerModel::PerTerminal, false),
    ));
    let agents = Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new());
    let queue = Arc::new(pos_edge::print_queue::InMemoryPrintQueue::new());
    let wake = pos_edge::print_wake::SharedPrintWake::new();
    let dispatch: Arc<dyn AgentDispatch> = if unreadable {
        Arc::new(Unreadable)
    } else {
        Arc::new(AgentLane::new(
            Arc::clone(&agents),
            Arc::clone(&queue),
            wake.clone(),
        ))
    };
    let written = Arc::new(Written::default());
    let printers =
        Arc::new(Printers::over(Arc::new(WrittenAt(Arc::clone(&written)))).with_agents(dispatch));
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let mut paired = Vec::new();
    for _ in 0..3 {
        let (code, _) = pairing
            .mint(now, Minter::Boot)
            .expect("mint a pairing code");
        let token = pairing
            .redeem(&code, now)
            .await
            .expect("redeem")
            .token()
            .expect("a fresh code pairs a device");
        let device = pairing
            .device_for(&token)
            .expect("a freshly issued token resolves");
        paired.push((device, token.as_str().to_owned()));
    }
    let (_, stranger) = paired.pop().expect("a device that is no till");
    let (counter_device, counter) = paired.pop().expect("the counter's device");
    let (bar_device, bar) = paired.pop().expect("the bar's device");
    for (terminal, device) in [(BAR, bar_device), (COUNTER, counter_device)] {
        agents
            .claim(till(terminal), device, now.as_milliseconds_since_epoch())
            .await
            .expect("the manager's binding is recorded");
    }
    let app = pos_edge::http::domain_router(
        Arc::clone(&edge),
        InMemoryQueueNumbers::new(),
        agents,
        queue,
        wake,
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    )
    .layer(axum::Extension(printers));
    for token in [&bar, &counter, &stranger] {
        let (status, _, _) = send(
            app.clone(),
            token,
            "POST",
            "/api/session/sign-in",
            Some(json!({ "code": MANAGER_CODE, "pin": MANAGER_PIN })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "the manager signs in");
    }
    Tills {
        app,
        edge,
        written,
        bar,
        counter,
        stranger,
    }
}

/// Settles a fresh bill for a pizza on table `table`, over the route from the device holding
/// `token`, in one tender of `method`.
async fn settle_over(
    tills: &Tills,
    token: &str,
    table: u128,
    method: &str,
) -> (StatusCode, Option<String>, Value) {
    let (bill, due) = a_bill(&tills.edge, person(MANAGER, 0xB1), table).await;
    let body = json!({
        "payments": [{ "method": method, "tendered": due, "applied_to_bill": due }],
    });
    send(
        tills.app.clone(),
        token,
        "POST",
        &format!("/api/bills/{bill}/settle"),
        Some(body),
    )
    .await
}

#[tokio::test]
async fn the_drawers_read_lists_each_till_and_a_bound_device_opens_its_own() {
    let tills = drawers_app(false).await;
    let (app, edge, bar, stranger) = (tills.app, tills.edge, tills.bar, tills.stranger);
    let (status, _, read) = send(app.clone(), &bar, "GET", "/api/shifts", None).await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(read["drawer_model"], "DRAWER_MODEL_PER_TERMINAL");
    assert_eq!(
        read["drawers"][0]["terminal_device_id"],
        till(BAR).to_string()
    );
    assert_eq!(read["drawers"][0]["name"], "Bar");
    assert_eq!(read["drawers"][1]["default_float"], json!(vnd(300_000)));
    assert!(read["drawers"][0]["shift"].is_null(), "{read}");

    let (status, _, opened) = send(
        app.clone(),
        &bar,
        "POST",
        "/api/shifts",
        Some(json!({ "opening_float": vnd(500_000) })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert_eq!(opened["terminal_device_id"], till(BAR).to_string());
    let (_, _, current) = send(app.clone(), &bar, "GET", "/api/shifts/current", None).await;
    assert_eq!(current["shift_id"], opened["shift_id"]);

    let open = json!({ "opening_float": vnd(300_000) });
    let (status, reason, _) = send(app.clone(), &stranger, "POST", "/api/shifts", Some(open)).await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("NOT_A_TILL"))
    );
    let named = json!({ "opening_float": vnd(300_000), "terminal_device_id": till(COUNTER) });
    let (status, reason, _) =
        send(app.clone(), &stranger, "POST", "/api/shifts", Some(named)).await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::FORBIDDEN, Some("APPROVAL_REQUIRED")),
        "another till's drawer is PIN-flagged"
    );
    let approved = json!({
        "opening_float": vnd(300_000),
        "terminal_device_id": till(COUNTER),
        "approver_code": MANAGER_CODE,
        "approver_pin": MANAGER_PIN,
    });
    let (status, _, counter) = send(
        app.clone(),
        &stranger,
        "POST",
        "/api/shifts",
        Some(approved),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{counter}");
    assert_eq!(counter["terminal_device_id"], till(COUNTER).to_string());
    let (_, _, none) = send(app.clone(), &stranger, "GET", "/api/shifts/current", None).await;
    assert!(
        none.is_null(),
        "a device that is no till has no drawer: {none}"
    );

    // Counting and closing another till's drawer takes the approver in the request too.
    let count = format!(
        "/api/shifts/{}/count",
        counter["shift_id"].as_str().unwrap_or_default()
    );
    let (status, reason, _) = send(
        app.clone(),
        &bar,
        "POST",
        &count,
        Some(json!({ "counted_minor": 300_000 })),
    )
    .await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::FORBIDDEN, Some("APPROVAL_REQUIRED"))
    );
    let (status, _, _) = send(
        app.clone(),
        &bar,
        "POST",
        &count,
        Some(json!({
            "counted_minor": 300_000,
            "approver_code": MANAGER_CODE,
            "approver_pin": MANAGER_PIN,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, _, read) = send(app.clone(), &bar, "GET", "/api/shifts", None).await;
    assert!(read["drawers"][1]["opened_time"].is_string(), "{read}");
    assert_eq!(read["drawers"][1]["shift"]["shift_id"], counter["shift_id"]);
    assert!(
        read.get("waiting_drawer_model").is_none(),
        "nothing waits: {read}"
    );

    // The store publishes one drawer while two are open: it waits, and the read says so.
    edge.apply_session(session(DrawerModel::PerStore, false));
    let (_, _, read) = send(app.clone(), &bar, "GET", "/api/shifts", None).await;
    assert_eq!(read["drawer_model"], "DRAWER_MODEL_PER_TERMINAL");
    assert_eq!(read["waiting_drawer_model"], "DRAWER_MODEL_PER_STORE");
}

/// The read names the till the device asking is, whose drawer its Shift screen shows first, and
/// tells a device that is no till it is none.
#[tokio::test]
async fn the_drawers_read_names_the_till_the_device_asking_is() {
    let tills = drawers_app(false).await;
    let (app, bar, stranger) = (tills.app, tills.bar, tills.stranger);
    let (_, _, read) = send(app.clone(), &bar, "GET", "/api/shifts", None).await;
    assert_eq!(read["terminal_device_id"], till(BAR).to_string(), "{read}");
    let (_, _, read) = send(app.clone(), &stranger, "GET", "/api/shifts", None).await;
    assert!(
        read.get("terminal_device_id").is_none(),
        "a device that is no till is told so: {read}"
    );
}

/// A drawer whose shift is open says when it opened on the shop's wall clock, which a till's own
/// clock may not keep, so the list of every till's drawer can show it as it is.
#[tokio::test]
async fn an_open_drawer_says_when_it_opened_on_the_shops_wall_clock() {
    let tills = drawers_app(false).await;
    let (app, bar, stranger) = (tills.app, tills.bar, tills.stranger);
    let (_, _, read) = send(app.clone(), &stranger, "GET", "/api/shifts", None).await;
    assert!(
        read["drawers"][0].get("opened_clock").is_none(),
        "a closed drawer opened at no time: {read}"
    );
    let open = json!({ "opening_float": vnd(500_000) });
    let (status, _, _) = send(app.clone(), &bar, "POST", "/api/shifts", Some(open)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, read) = send(app.clone(), &stranger, "GET", "/api/shifts", None).await;
    let clock = read["drawers"][0]["opened_clock"]
        .as_str()
        .unwrap_or_default();
    assert!(
        clock.len() == 5 && clock.chars().nth(2) == Some(':'),
        "the shop's wall clock, HH:MM: {read}"
    );
}

#[tokio::test]
async fn a_binding_that_cannot_be_read_refuses_rather_than_guesses_the_till() {
    let tills = drawers_app(true).await;
    let (status, reason, _) = settle_over(&tills, &tills.bar, 41, "PAYMENT_METHOD_CASH").await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("STORE_UNAVAILABLE")),
        "cash is refused rather than put in a drawer that may be the wrong one"
    );
    let (status, _, card) = settle_over(&tills, &tills.bar, 42, "PAYMENT_METHOD_CARD").await;
    assert_eq!(status, StatusCode::OK, "a card goes into no drawer: {card}");
    let Tills { app, edge, bar, .. } = tills;
    let open = json!({ "opening_float": vnd(500_000) });
    let (status, reason, _) =
        send(app.clone(), &bar, "POST", "/api/shifts", Some(open.clone())).await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("STORE_UNAVAILABLE"))
    );
    let (status, reason, _) = send(app.clone(), &bar, "GET", "/api/shifts", None).await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("STORE_UNAVAILABLE")),
        "which drawer is the device's own decides what the list shows"
    );

    // Where the store keeps one drawer it is every device's, and nothing asks which till this is.
    edge.apply_session(session(DrawerModel::PerStore, false));
    let (status, _, opened) = send(app, &bar, "POST", "/api/shifts", Some(open)).await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert!(opened.get("terminal_device_id").is_none(), "{opened}");
}

#[tokio::test]
async fn cash_springs_only_the_drawer_of_the_till_it_is_taken_at() {
    let tills = drawers_app(false).await;
    let mut shifts = Vec::new();
    for (token, own) in [(&tills.bar, BAR), (&tills.counter, COUNTER)] {
        let open = json!({ "opening_float": vnd(500_000) });
        let (status, _, opened) =
            send(tills.app.clone(), token, "POST", "/api/shifts", Some(open)).await;
        assert_eq!(status, StatusCode::OK, "{opened}");
        assert_eq!(opened["terminal_device_id"], till(own).to_string());
        shifts.push(opened["shift_id"].as_str().unwrap_or_default().to_owned());
    }

    let (status, _, settled) = settle_over(&tills, &tills.bar, 41, "PAYMENT_METHOD_CASH").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["drawer_open"], "OPENED");
    assert_eq!(
        tills.written.at(),
        [BAR_DRAWER],
        "the bar's drawer and no other"
    );

    // The counter's till names a printer with no drawer: none springs, the store's included.
    let (status, _, settled) = settle_over(&tills, &tills.counter, 42, "PAYMENT_METHOD_CASH").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert_eq!(settled["drawer_open"], "NO_DRAWER");
    assert_eq!(tills.written.at(), [BAR_DRAWER]);

    // A device that is no till takes a card, and no cash.
    let (status, reason, _) = settle_over(&tills, &tills.stranger, 43, "PAYMENT_METHOD_CASH").await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("NOT_A_TILL"))
    );
    let (status, _, settled) =
        settle_over(&tills, &tills.stranger, 44, "PAYMENT_METHOD_CARD").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    assert!(settled.get("drawer_open").is_none(), "{settled}");

    // A paid in springs the drawer it goes into, and is made at that drawer's own till.
    let movement = json!({ "amount_minor": 100_000, "reason_code_id": making_change() });
    let paid_in = format!("/api/shifts/{}/paid-in", shifts[0]);
    let (status, _, moved) = send(
        tills.app.clone(),
        &tills.bar,
        "POST",
        &paid_in,
        Some(movement.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["drawer_open"], "OPENED");
    for (token, refusal) in [
        (&tills.counter, "SHIFT_NOT_OPEN"),
        (&tills.stranger, "NOT_A_TILL"),
    ] {
        let (status, reason, _) = send(
            tills.app.clone(),
            token,
            "POST",
            &paid_in,
            Some(movement.clone()),
        )
        .await;
        assert_eq!(
            (status, reason.as_deref()),
            (StatusCode::CONFLICT, Some(refusal))
        );
    }

    // A no-sale opening opens the drawer of the till it is made at.
    let no_sale = json!({
        "reason_code_id": making_change(),
        "approver_code": MANAGER_CODE,
        "approver_pin": MANAGER_PIN,
    });
    let (status, _, opened) = send(
        tills.app.clone(),
        &tills.counter,
        "POST",
        "/api/drawer/open",
        Some(no_sale.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert_eq!(opened["drawer_open"], "NO_DRAWER");
    let (status, reason, _) = send(
        tills.app.clone(),
        &tills.stranger,
        "POST",
        "/api/drawer/open",
        Some(no_sale),
    )
    .await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("NOT_A_TILL"))
    );
    assert_eq!(
        tills.written.at(),
        [BAR_DRAWER, BAR_DRAWER],
        "the store's drawer never sprang for a till's cash"
    );
}

/// Opens a drawer over the route from the device holding `token`, its own till's, answering its
/// shift.
async fn open_over(tills: &Tills, token: &str) -> String {
    let open = json!({ "opening_float": vnd(500_000) });
    let (status, _, opened) =
        send(tills.app.clone(), token, "POST", "/api/shifts", Some(open)).await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    opened["shift_id"].as_str().unwrap_or_default().to_owned()
}

/// Counts `shift` at 500,000 over the route from the device holding `token`.
async fn count_over(tills: &Tills, token: &str, shift: &str) {
    let count = json!({ "counted_minor": 500_000 });
    let uri = format!("/api/shifts/{shift}/count");
    let (status, _, counted) = send(tills.app.clone(), token, "POST", &uri, Some(count)).await;
    assert_eq!(status, StatusCode::OK, "{counted}");
}

/// The manager's code and PIN, which approve their own act where the store does not enforce.
fn approver() -> Value {
    json!({ "approver_code": MANAGER_CODE, "approver_pin": MANAGER_PIN })
}

#[tokio::test]
async fn a_drawers_report_prints_at_the_till_it_is_closed_at_or_not_at_all() {
    let tills = drawers_app(false).await;
    let bar = open_over(&tills, &tills.bar).await;
    let counter = open_over(&tills, &tills.counter).await;
    count_over(&tills, &tills.bar, &bar).await;
    count_over(&tills, &tills.counter, &counter).await;

    let (status, _, closed) = send(
        tills.app.clone(),
        &tills.bar,
        "POST",
        &format!("/api/shifts/{bar}/close"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(closed["shift_report_print"], "PRINTED");
    assert_eq!(tills.written.at(), [BAR_DRAWER], "at the bar's own printer");
    assert!(tills.written.last_says(BAR_DRAWER, "SHIFT REPORT"));

    // Closed from a device that is no till, the report prints at the store's receipt printer.
    let (status, _, closed) = send(
        tills.app.clone(),
        &tills.stranger,
        "POST",
        &format!("/api/shifts/{counter}/close"),
        Some(approver()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(tills.written.at(), [BAR_DRAWER, STORE_DRAWER]);

    // A store that prints no report at a close prints none.
    tills.edge.apply_session(reporting(
        DrawerModel::PerTerminal,
        false,
        CloseReport::None,
    ));
    let bar = open_over(&tills, &tills.bar).await;
    count_over(&tills, &tills.bar, &bar).await;
    let (status, _, closed) = send(
        tills.app.clone(),
        &tills.bar,
        "POST",
        &format!("/api/shifts/{bar}/close"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert!(closed.get("shift_report_print").is_none(), "{closed}");
    assert_eq!(
        closed["variance"],
        json!(vnd(0)),
        "the figures still answer"
    );
    assert_eq!(tills.written.at(), [BAR_DRAWER, STORE_DRAWER]);
}

#[tokio::test]
async fn drawers_closed_together_print_one_slip_where_they_are_closed() {
    let tills = drawers_app(false).await;
    tills.edge.apply_session(reporting(
        DrawerModel::PerTerminal,
        false,
        CloseReport::Combined,
    ));
    let bar = open_over(&tills, &tills.bar).await;
    let counter = open_over(&tills, &tills.counter).await;
    count_over(&tills, &tills.bar, &bar).await;

    let mut batch = approver();
    batch["shift_ids"] = json!([counter, bar]);
    let (status, reason, _) = send(
        tills.app.clone(),
        &tills.counter,
        "POST",
        "/api/shifts:batch_close",
        Some(batch.clone()),
    )
    .await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("TRANSITION_REFUSED")),
        "the counter's drawer is not counted, so neither closes"
    );
    count_over(&tills, &tills.counter, &counter).await;

    let (status, _, closed) = send(
        tills.app.clone(),
        &tills.counter,
        "POST",
        "/api/shifts:batch_close",
        Some(batch),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(closed["shift_report_print"], "PRINTED");
    assert_eq!(closed["shifts"][0]["shift_id"], counter.as_str());
    assert_eq!(closed["shifts"][1]["state"], "SHIFT_STATE_CLOSED");
    assert!(closed["shifts"][1].get("shift_report_print").is_none());
    assert_eq!(
        tills.written.at(),
        [COUNTER_PAPER],
        "one slip, at the counter's own printer"
    );
    for words in ["STORE TOTALS", "Bar", "Counter"] {
        assert!(tills.written.last_says(COUNTER_PAPER, words), "{words}");
    }
}

#[tokio::test]
async fn the_shift_reads_say_a_drawers_day_has_ended_and_its_cash_is_refused_by_name() {
    let tills = drawers_app(false).await;
    tills
        .edge
        .apply_session(zoned(BEHIND, DrawerDayEnd::RequireClose, false));
    let bar = open_over(&tills, &tills.bar).await;
    let (_, _, current) = send(
        tills.app.clone(),
        &tills.bar,
        "GET",
        "/api/shifts/current",
        None,
    )
    .await;
    assert!(current.get("day_ended").is_none(), "{current}");

    tills
        .edge
        .apply_session(zoned(AHEAD, DrawerDayEnd::RequireClose, false));
    let (_, _, current) = send(
        tills.app.clone(),
        &tills.bar,
        "GET",
        "/api/shifts/current",
        None,
    )
    .await;
    assert_eq!(current["shift_id"], bar.as_str());
    assert_eq!(current["day_ended"], true);
    let (_, _, listed) = send(tills.app.clone(), &tills.bar, "GET", "/api/shifts", None).await;
    assert_eq!(listed["drawers"][0]["shift"]["day_ended"], true, "{listed}");
    let (status, reason, _) = settle_over(&tills, &tills.bar, 41, "PAYMENT_METHOD_CASH").await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("DRAWER_DAY_ENDED"))
    );
}

/// `body` posted to `uri` from the device holding `token`.
async fn post(
    tills: &Tills,
    token: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Option<String>, Value) {
    send(tills.app.clone(), token, "POST", uri, Some(body)).await
}

/// What `uri` reads from the device holding `token`.
async fn get(tills: &Tills, token: &str, uri: &str) -> Value {
    send(tills.app.clone(), token, "GET", uri, None).await.2
}

#[tokio::test]
async fn an_assigned_drawer_says_whose_it_is_by_id_and_refuses_anybody_else_by_reason() {
    let tills = drawers_app(false).await;
    tills
        .edge
        .apply_session(assigned(DrawerModel::PerTerminal, false));
    let cashier = employee(CASHIER).to_string();
    let refused = |reason| (StatusCode::CONFLICT, Some(reason));

    // The manager, at a device that is no till, starts the bar's drawer for the cashier.
    let mut start = approver();
    start["opening_float"] = json!(vnd(500_000));
    start["terminal_device_id"] = json!(till(BAR));
    let (status, reason, _) = post(&tills, &tills.stranger, "/api/shifts", start.clone()).await;
    assert_eq!((status, reason.as_deref()), refused("ASSIGNEE_REQUIRED"));
    start["assigned_employee_id"] = json!(cashier);
    let (status, _, opened) = post(&tills, &tills.stranger, "/api/shifts", start).await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert_eq!(opened["assigned_employee_id"], cashier.as_str());
    let current = get(&tills, &tills.bar, "/api/shifts/current").await;
    assert_eq!(
        current["assigned_employee_id"],
        cashier.as_str(),
        "{current}"
    );
    let listed = get(&tills, &tills.bar, "/api/shifts").await;
    let drawer = &listed["drawers"][0]["shift"];
    assert_eq!(drawer["assigned_employee_id"], cashier.as_str(), "{listed}");

    // The manager signed in at the bar is not the cashier.
    let shift = opened["shift_id"].as_str().unwrap_or_default();
    let paid_in = format!("/api/shifts/{shift}/paid-in");
    let mut movement = json!({ "amount_minor": 10_000, "reason_code_id": making_change() });
    let (status, reason, _) = post(&tills, &tills.bar, &paid_in, movement.clone()).await;
    assert_eq!(
        (status, reason.as_deref()),
        refused("DRAWER_ASSIGNED_TO_ANOTHER")
    );
    movement["approver_code"] = json!(MANAGER_CODE);
    movement["approver_pin"] = json!(MANAGER_PIN);
    let (status, _, moved) = post(&tills, &tills.bar, &paid_in, movement).await;
    assert_eq!(status, StatusCode::OK, "{moved}");

    let (status, reason, _) = settle_over(&tills, &tills.bar, 41, "PAYMENT_METHOD_CASH").await;
    assert_eq!(
        (status, reason.as_deref()),
        refused("DRAWER_ASSIGNED_TO_ANOTHER")
    );
    let (status, _, settled) = settle_over(&tills, &tills.bar, 42, "PAYMENT_METHOD_CARD").await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    let (bill, due) = a_bill(&tills.edge, person(MANAGER, 0xB1), 43).await;
    let mut cash = approver();
    cash["payments"] =
        json!([{ "method": "PAYMENT_METHOD_CASH", "tendered": due, "applied_to_bill": due }]);
    let uri = format!("/api/bills/{bill}/settle");
    let (status, _, settled) = post(&tills, &tills.bar, &uri, cash).await;
    assert_eq!(status, StatusCode::OK, "{settled}");

    // One approver covers a no-sale opening and the override beside it.
    let mut no_sale = approver();
    no_sale["reason_code_id"] = json!(making_change());
    let (status, _, sprang) = post(&tills, &tills.bar, "/api/drawer/open", no_sale).await;
    assert_eq!(status, StatusCode::OK, "{sprang}");
}

/// Counts `shift` at `counted` over the route from the device holding `token`.
async fn count_at(tills: &Tills, token: &str, shift: &str, counted: i64) {
    let uri = format!("/api/shifts/{shift}/count");
    let (status, _, answered) = post(tills, token, &uri, json!({ "counted_minor": counted })).await;
    assert_eq!(status, StatusCode::OK, "{answered}");
}

#[tokio::test]
async fn a_close_owed_a_reason_answers_the_figures_to_show_and_closes_when_given_one() {
    let tills = drawers_app(false).await;
    tills.edge.apply_session(varying(false));
    let owed = (StatusCode::CONFLICT, Some("VARIANCE_REASON_REQUIRED"));

    // The bar's drawer, counted 50k short of the 500k float it opened on.
    let bar = open_over(&tills, &tills.bar).await;
    count_at(&tills, &tills.bar, &bar, 450_000).await;
    let close = format!("/api/shifts/{bar}/close");
    let (status, reason, asked) = post(&tills, &tills.bar, &close, json!({})).await;
    assert_eq!((status, reason.as_deref()), owed);
    assert!(asked["message"].is_string(), "{asked}");
    let owing = &asked["shifts"][0];
    assert_eq!(owing["shift_id"], bar.as_str());
    assert_eq!(owing["terminal_device_id"], till(BAR).to_string());
    assert_eq!(owing["state"], "SHIFT_STATE_COUNTED");
    assert_eq!(owing["expected_amount"], json!(vnd(500_000)));
    assert_eq!(owing["counted_amount"], json!(vnd(450_000)));
    assert_eq!(owing["variance"], json!(vnd(-50_000)));
    let wrong = json!({ "reason_code_id": making_change() });
    let (status, reason, _) = post(&tills, &tills.bar, &close, wrong).await;
    assert_eq!(
        (status, reason.as_deref()),
        (StatusCode::CONFLICT, Some("CASH_REASON_NOT_VALID"))
    );
    let given = json!({ "reason_code_id": miscounted() });
    let (status, _, closed) = post(&tills, &tills.bar, &close, given).await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(closed["variance"], json!(vnd(-50_000)));

    // Both drawers 30k short, closed together from the counter: the refusal names each, and one
    // reason apiece closes them.
    let bar = open_over(&tills, &tills.bar).await;
    let counter = open_over(&tills, &tills.counter).await;
    count_at(&tills, &tills.bar, &bar, 470_000).await;
    count_at(&tills, &tills.counter, &counter, 470_000).await;
    let mut batch = approver();
    batch["shift_ids"] = json!([bar, counter]);
    let uri = "/api/shifts:batch_close";
    let (status, reason, asked) = post(&tills, &tills.counter, uri, batch.clone()).await;
    assert_eq!((status, reason.as_deref()), owed);
    let named: Vec<&Value> = asked["shifts"]
        .as_array()
        .map(|shifts| shifts.iter().map(|shift| &shift["shift_id"]).collect())
        .unwrap_or_default();
    assert_eq!(named, [&json!(bar), &json!(counter)], "{asked}");
    batch["variance_reasons"] = json!([
        { "shift_id": bar, "reason_code_id": miscounted() },
        { "shift_id": counter, "reason_code_id": miscounted() },
    ]);
    let (status, _, closed) = post(&tills, &tills.counter, uri, batch).await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(closed["shifts"][1]["state"], "SHIFT_STATE_CLOSED");
}
