// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A publish's dry run: the checks a version's content meets before it is stored, run on a draft
//! the console holds, without storing anything
//! ([ADR-0190](../../../../../docs/adr/0190-a-screen-is-drawn-from-the-package.md) decision 8).
//!
//! The dry run and the publish share [`first_refusal`], so a draft the dry run accepts is one the
//! publish accepts, but for a version of the package published in between. The dry run answers
//! `200` with what it found, and says where: the field, and for a screen the variant's position in
//! `screens` and the node's position in its tree's pre-order (the root is 0), so the console can
//! show a refusal beside the thing refused.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, service_unavailable};
use pos_cloud_config::packages::{PackageStore, min_edge_version};
use pos_core::flow::{FlowRefusal, check_flows};
use pos_core::package::{
    ContentRefusal, EvolutionRefusal, KeyAt, KeyRefusal, ScreenFault, TapWarning, check_content,
    check_evolution, check_keys, tap_warnings,
};
use pos_core::reaction::{ReactionRefusal, check_reactions};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{PackageId, PackageVersionId, TenantId};
use pos_proto::package::{CONTENT_BYTES_MAX, PackageContent};
use pos_proto::text::ReleaseTag;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::WireEnum as _;
use serde::Serialize;

use super::conditions::PublishConditions;
use super::{PackageState, PackageVersionRequest, package_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::grid_keys::GridKeys;
use crate::http::{FieldRefusal, Json, not_found};

/// The reason a key without an `en` value is refused with, as a receipt template's is.
const UNKNOWN_TRANSLATION_KEY: &str = "UNKNOWN_TRANSLATION_KEY";

/// What a version over [`CONTENT_BYTES_MAX`] is told.
const TOO_LARGE: &str = "a version's content is at most 1 MiB";

/// Why a version's content cannot be published.
pub(super) enum Refusal {
    /// The content itself is refused (ADR-0189, ADR-0190, ADR-0194).
    Content(ContentRefusal),
    /// Its data removes, moves or retypes what an earlier version declared (ADR-0194 decision 6).
    NotAdditive(EvolutionRefusal),
    /// It is over [`CONTENT_BYTES_MAX`] (ADR-0189 decision 3).
    TooLarge,
    /// A flow is refused (ADR-0192 decision 2): `index` is its variant's position in `flows`, or
    /// `None` when the refusal is about the flows together.
    Flow {
        index: Option<usize>,
        refusal: FlowRefusal,
    },
    /// A reaction is refused (ADR-0193 decision 5): `index` is its variant's position in
    /// `reactions`, or `None` when the refusal is about several together.
    Reaction {
        index: Option<usize>,
        refusal: ReactionRefusal,
    },
    /// A translation key it names has no `en` value in the tenant's grid (ADR-0190 decision 6).
    Key(KeyRefusal),
}

impl Refusal {
    /// The `400` a publish answers with.
    pub(super) fn into_response(self) -> Response {
        match self {
            Self::Content(refusal) => api_error_with_details(
                ErrorStatus::InvalidArgument,
                refusal.to_string(),
                &[(refusal.field(), refusal.code())],
            ),
            Self::NotAdditive(refusal) => api_error_with_details(
                ErrorStatus::InvalidArgument,
                refusal.to_string(),
                &[(refusal.field(), "NOT_ADDITIVE")],
            ),
            Self::TooLarge => FieldRefusal::new(TOO_LARGE, "content", "TOO_LARGE").into_response(),
            Self::Flow { refusal, .. } => api_error_with_details(
                ErrorStatus::InvalidArgument,
                format!("flows: {refusal}"),
                &[("flows", "INVALID")],
            ),
            Self::Reaction { refusal, .. } => api_error_with_details(
                ErrorStatus::InvalidArgument,
                format!("reactions: {refusal}"),
                &[("reactions", "INVALID")],
            ),
            Self::Key(refusal) => api_error_with_details(
                ErrorStatus::InvalidArgument,
                refusal.to_string(),
                &[(refusal.field(), UNKNOWN_TRANSLATION_KEY)],
            ),
        }
    }

    /// The refusal as a dry run reports it.
    fn located(&self) -> PackageRefusal {
        let (field, reason, message) = match self {
            Self::Content(refusal) => (refusal.field(), refusal.code(), refusal.to_string()),
            Self::NotAdditive(refusal) => (refusal.field(), "NOT_ADDITIVE", refusal.to_string()),
            Self::TooLarge => ("content", "TOO_LARGE", TOO_LARGE.to_owned()),
            Self::Flow { refusal, .. } => ("flows", flow_reason(refusal), refusal.to_string()),
            Self::Reaction { refusal, .. } => {
                ("reactions", reaction_reason(refusal), refusal.to_string())
            }
            Self::Key(refusal) => (
                refusal.field(),
                UNKNOWN_TRANSLATION_KEY,
                refusal.to_string(),
            ),
        };
        let mut located = PackageRefusal {
            field: field.to_owned(),
            reason: reason.to_owned(),
            message,
            screen_index: None,
            node_index: None,
            name: None,
            flow_index: None,
            step_id: None,
            reaction_index: None,
        };
        match self {
            Self::Flow { index, refusal } => {
                located.flow_index = *index;
                located.step_id = flow_step(refusal).map(str::to_owned);
                located.name = flow_name(refusal).map(str::to_owned);
            }
            Self::Reaction { index, refusal } => {
                located.reaction_index = *index;
                located.name = reaction_name(refusal).map(str::to_owned);
            }
            Self::Key(refusal) => {
                located.name = Some(refusal.key.clone());
                if let KeyAt::Screen { screen, node } = refusal.at {
                    located.screen_index = Some(screen);
                    located.node_index = Some(node);
                }
            }
            _ => {}
        }
        if let Self::Content(ContentRefusal::Screen(screen)) = self {
            let (reason, node, name) = screen_fault(screen.fault);
            located.reason = String::from(reason);
            located.screen_index = Some(screen.screen);
            located.node_index = node;
            located.name = name.map(str::to_owned);
        }
        if let Self::Content(ContentRefusal::Home(home)) = self {
            located.reason = String::from(home.reason());
            located.name = home.index().map(|index| index.to_string());
        }
        located
    }
}

/// A screen fault's reason token, the node at fault, and the property or parameter it names.
const fn screen_fault(fault: ScreenFault) -> (&'static str, Option<usize>, Option<&'static str>) {
    match fault {
        ScreenFault::TooManyScreens => ("TOO_MANY_SCREENS", None, None),
        ScreenFault::ScreenId => ("SCREEN_ID", None, None),
        ScreenFault::Variants(_) => ("VARIANTS", None, None),
        ScreenFault::TooManyNodes => ("TOO_MANY_NODES", None, None),
        ScreenFault::TooDeep => ("TOO_DEEP", None, None),
        ScreenFault::UnknownComponent(node) => ("UNKNOWN_COMPONENT", Some(node), None),
        ScreenFault::DeviceClass(node) => ("DEVICE_CLASS", Some(node), None),
        ScreenFault::Children(node) => ("CHILDREN", Some(node), None),
        ScreenFault::UnknownProperty(node) => ("UNKNOWN_PROPERTY", Some(node), None),
        ScreenFault::MissingProperty(node, name) => ("MISSING_PROPERTY", Some(node), Some(name)),
        ScreenFault::PropertyValue(node, name) => ("PROPERTY_VALUE", Some(node), Some(name)),
        ScreenFault::UnknownAction(node, name) => ("UNKNOWN_ACTION", Some(node), Some(name)),
        ScreenFault::FixedParam(node, name) => ("FIXED_PARAM", Some(node), Some(name)),
        ScreenFault::MissingParam(node, name) => ("MISSING_PARAM", Some(node), Some(name)),
    }
}

/// A flow refusal's stable reason token.
const fn flow_reason(refusal: &FlowRefusal) -> &'static str {
    match refusal {
        FlowRefusal::TooManyFlows(_) => "TOO_MANY_FLOWS",
        FlowRefusal::StepCount { .. } => "STEP_COUNT",
        FlowRefusal::BadId(_) => "BAD_ID",
        FlowRefusal::DuplicateStep { .. } => "DUPLICATE_STEP",
        FlowRefusal::NotOneThing { .. } => "NOT_ONE_THING",
        FlowRefusal::UnknownKind { .. } => "UNKNOWN_KIND",
        FlowRefusal::UnknownAction(_) => "UNKNOWN_ACTION",
        FlowRefusal::UnknownScreen(_) => "UNKNOWN_SCREEN",
        FlowRefusal::UnknownPrerequisite { .. } => "UNKNOWN_PREREQUISITE",
        FlowRefusal::Cycle { .. } => "CYCLE",
        FlowRefusal::Condition { .. } => "CONDITION",
    }
}

/// The step a flow refusal is about, when it is about one. A cycle names the first step caught in
/// it.
fn flow_step(refusal: &FlowRefusal) -> Option<&str> {
    match refusal {
        FlowRefusal::DuplicateStep { step_id, .. }
        | FlowRefusal::NotOneThing { step_id, .. }
        | FlowRefusal::UnknownKind { step_id, .. }
        | FlowRefusal::UnknownPrerequisite { step_id, .. }
        | FlowRefusal::Condition { step_id, .. } => Some(step_id),
        FlowRefusal::Cycle { step_ids, .. } => step_ids.first().map(String::as_str),
        FlowRefusal::TooManyFlows(_)
        | FlowRefusal::StepCount { .. }
        | FlowRefusal::BadId(_)
        | FlowRefusal::UnknownAction(_)
        | FlowRefusal::UnknownScreen(_) => None,
    }
}

/// The id, action, screen or prerequisite a flow refusal names, when it names one.
fn flow_name(refusal: &FlowRefusal) -> Option<&str> {
    match refusal {
        FlowRefusal::BadId(name)
        | FlowRefusal::UnknownAction(name)
        | FlowRefusal::UnknownScreen(name) => Some(name),
        FlowRefusal::UnknownPrerequisite { prerequisite, .. } => Some(prerequisite),
        _ => None,
    }
}

/// A reaction refusal's stable reason token.
const fn reaction_reason(refusal: &ReactionRefusal) -> &'static str {
    match refusal {
        ReactionRefusal::BadId(_) => "BAD_ID",
        ReactionRefusal::UnknownTrigger(_) => "UNKNOWN_TRIGGER",
        ReactionRefusal::Qualifier(_) => "QUALIFIER",
        ReactionRefusal::UnknownTarget { .. } => "UNKNOWN_TARGET",
        ReactionRefusal::TooManyForTrigger { .. } => "TOO_MANY_FOR_TRIGGER",
        ReactionRefusal::UnknownAction(_) => "UNKNOWN_ACTION",
        ReactionRefusal::ActNotAllowed { .. } => "ACT_NOT_ALLOWED",
        ReactionRefusal::Param { .. } => "PARAM",
        ReactionRefusal::Condition { .. } => "CONDITION",
    }
}

/// The id, target, action or parameter a reaction refusal names, when it names one.
fn reaction_name(refusal: &ReactionRefusal) -> Option<&str> {
    match refusal {
        ReactionRefusal::BadId(name)
        | ReactionRefusal::UnknownTrigger(name)
        | ReactionRefusal::UnknownAction(name) => Some(name),
        ReactionRefusal::UnknownTarget { target, .. } => Some(target),
        ReactionRefusal::ActNotAllowed { action_id, .. } => Some(action_id),
        ReactionRefusal::Param { param, .. } => Some(param),
        ReactionRefusal::Qualifier(_)
        | ReactionRefusal::TooManyForTrigger { .. }
        | ReactionRefusal::Condition { .. } => None,
    }
}

/// The first refusal of a version's flows, then of its reactions: each variant alone, so a
/// refusal says which, then all of them together for what only the whole can break (how many flows,
/// how many reactions to one trigger). A step's or a reaction's screen is not resolved here, as the
/// edge does not resolve it on apply: one the version does not declare is drawn from the release's
/// default package (ADR-0190).
fn first_part_refusal(content: &PackageContent) -> Option<Refusal> {
    let conditions = PublishConditions::new();
    let any_screen = |_screen: &str| true;
    let flows = &content.flows;
    let flow = flows
        .iter()
        .enumerate()
        .find_map(|(index, variant)| {
            check_flows(core::slice::from_ref(variant), &any_screen, &conditions)
                .err()
                .map(|refusal| (Some(index), refusal))
        })
        .or_else(|| {
            check_flows(flows, &any_screen, &conditions)
                .err()
                .map(|refusal| (None, refusal))
        });
    if let Some((index, refusal)) = flow {
        return Some(Refusal::Flow { index, refusal });
    }
    let reactions = &content.reactions;
    reactions
        .iter()
        .enumerate()
        .find_map(|(index, variant)| {
            check_reactions(
                core::slice::from_ref(variant),
                flows,
                &any_screen,
                &conditions,
            )
            .err()
            .map(|refusal| (Some(index), refusal))
        })
        .or_else(|| {
            check_reactions(reactions, flows, &any_screen, &conditions)
                .err()
                .map(|refusal| (None, refusal))
        })
        .map(|(index, refusal)| Refusal::Reaction { index, refusal })
}

/// What a publish would store for `request`, under `package_version_id`, with the minimum edge
/// release the server computes.
pub(super) fn version_content(
    package_id: PackageId,
    package_version_id: PackageVersionId,
    request: PackageVersionRequest,
) -> PackageContent {
    let mut content = PackageContent {
        package_id,
        package_version_id,
        version_label: request.version_label.trim().to_owned(),
        min_edge_version: ReleaseTag::new(""),
        time_slots: request.time_slots,
        flows: request.flows,
        screens: request.screens,
        data: request.data,
        reactions: request.reactions,
        home_screens: request.home_screens,
    };
    content.min_edge_version = min_edge_version(&content);
    content
}

/// The first reason `content` cannot be published, in the order the publish checks: the content,
/// then its flows and reactions, then that every key it names has an `en` value in `grid`, then
/// that its data only adds to every earlier version's, then its size.
///
/// # Errors
///
/// The `503` a package store or translation store failure earns.
pub(super) async fn first_refusal<P: PackageStore>(
    packages: &P,
    grid: &dyn GridKeys,
    tenant_id: TenantId,
    content: &PackageContent,
) -> Result<Option<Refusal>, Response> {
    if let Err(refusal) = check_content(content) {
        return Ok(Some(Refusal::Content(refusal)));
    }
    if let Some(refusal) = first_part_refusal(content) {
        return Ok(Some(refusal));
    }
    let keys = grid.fallback_keys(tenant_id).await.map_err(|error| {
        tracing::warn!(%error, "a package version's check could not read the translation grid");
        service_unavailable("translation store")
    })?;
    if let Err(refusal) = check_keys(content, |key| keys.contains(key)) {
        return Ok(Some(Refusal::Key(refusal)));
    }
    let earlier = packages
        .list_versions(tenant_id, content.package_id)
        .await
        .map_err(|error| package_error_response(&error))?;
    if let Some(refusal) = earlier
        .iter()
        .find_map(|version| check_evolution(&version.content.data, &content.data).err())
    {
        return Ok(Some(Refusal::NotAdditive(refusal)));
    }
    if serde_json::to_vec(content).map_or(true, |bytes| bytes.len() > CONTENT_BYTES_MAX) {
        return Ok(Some(Refusal::TooLarge));
    }
    Ok(None)
}

/// The acts `content` offers over their tap budget (ADR-0210 decision 5), once the checks the
/// count follows have passed: the content's, the flows' and reactions', and the keys'
/// (decision 6). A version they refuse has none, as its graph may not hold together.
fn warnings_after(refusal: Option<&Refusal>, content: &PackageContent) -> Vec<TapWarning> {
    match refusal {
        None | Some(Refusal::NotAdditive(_) | Refusal::TooLarge) => tap_warnings(content),
        Some(_) => Vec::new(),
    }
}

/// What a dry run found.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(super) struct PackageVersionDryRun {
    /// Whether the draft would publish as it stands.
    accepted: bool,
    /// The oldest edge release that would run it, as the publish would compute it.
    min_edge_version: String,
    /// Why it would be refused: the first fault the publish's checks meet, or none.
    refusals: Vec<PackageRefusal>,
    /// What would publish but is worth a look: each act a role reaches in more taps than its
    /// budget from its home (ADR-0210 decision 5). Never a reason to refuse. Empty for a draft that
    /// states no homes, or one refused before the count.
    warnings: Vec<PackageWarning>,
}

/// An act over its tap budget for one role on one device class, at the node offering it most
/// cheaply (ADR-0210 decision 5).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(super) struct PackageWarning {
    /// `TAP_BUDGET`.
    reason: &'static str,
    /// The role, by its id; absent for the base: every role without a home of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    role_id: Option<String>,
    /// The device class.
    device_class: &'static str,
    /// The act, by its catalogue id.
    action_id: String,
    /// The screen variant offering it most cheaply, by its position in `screens`.
    screen_index: usize,
    /// The node offering it, by its position in that tree's pre-order, the root being 0.
    node_index: usize,
    /// What it costs from the home, in taps: over two for a common act, over three for a rare one.
    taps: u32,
}

impl From<&TapWarning> for PackageWarning {
    fn from(warning: &TapWarning) -> Self {
        Self {
            reason: TapWarning::REASON,
            role_id: warning.role_id.map(|role| role.to_string()),
            device_class: warning.device_class.as_wire(),
            action_id: warning.action_id.clone(),
            screen_index: warning.screen_index,
            node_index: warning.node_index,
            taps: warning.taps,
        }
    }
}

/// One reason a draft would be refused, and where.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(super) struct PackageRefusal {
    /// The request field at fault, as a publish's field error names it, such as `version_label`,
    /// `screens` or `data.object_types`.
    field: String,
    /// A stable token naming the fault: a publish's code (`INVALID`, `NOT_ADDITIVE`,
    /// `TOO_LARGE`, `UNKNOWN_TRANSLATION_KEY` with the key in `name`), for a screen what is
    /// wrong with it, such as `MISSING_PROPERTY`, or for a home `NO_BASE_HOME`,
    /// `SAME_HOME_SELECTOR`, `HOME_SELECTOR` or `UNKNOWN_HOME_SCREEN`.
    reason: String,
    /// The fault in one English sentence, for a log; the console words it from `reason`.
    message: String,
    /// For a screen, the variant's position in `screens`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    screen_index: Option<usize>,
    /// For a screen's node, its position in the tree's pre-order, the root being 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    node_index: Option<usize>,
    /// The property or act parameter at fault, when the fault names one; for a flow or a reaction,
    /// the id, action, screen, prerequisite, target or parameter its fault names; for a key without
    /// an `en` value, the key; for a home, its position in `home_screens`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    name: Option<String>,
    /// For a flow, the variant's position in `flows`. Absent on a flow refusal about the flows
    /// together, such as too many.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    flow_index: Option<usize>,
    /// For a flow's step, its `step_id`; a cycle names the first step caught in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    step_id: Option<String>,
    /// For a reaction, the variant's position in `reactions`. Absent on a refusal about several
    /// together, such as too many for one trigger.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    reaction_index: Option<usize>,
}

#[utoipa::path(
    post,
    path = "/admin/packages/{package_id}/versions/dry-run",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.packages.manage"))),
    params(("package_id" = String, Path, description = "The package's 26-character ULID")),
    request_body(
        content = PackageVersionRequest, description = "A draft, in the shape a publish takes. Nothing is stored",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = PackageVersionDryRun, description = "What the publish's checks found: whether the draft would \
                                      publish, its minimum edge release, and the first fault and where it is"),
        (status = 400, description = "tenant_id or package_id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.packages.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no package with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The package store or the translation store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "packages",
)]
/// A console user checks a draft version the way a publish would, and nothing is stored.
pub(crate) async fn admin_dry_run_package_version<P, A, C>(
    State(state): State<PackageState<P, A, C>>,
    headers: HeaderMap,
    Path(package_id): Path<String>,
    Json(request): Json<PackageVersionRequest>,
) -> Response
where
    P: PackageStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePackages,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, package_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("package_id", &package_id),
    ]) {
        Ok([tenant_id, package_id]) => (TenantId::new(tenant_id), PackageId::new(package_id)),
        Err(refusal) => return refusal,
    };
    match state.packages.get_package(tenant_id, package_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found("package"),
        Err(error) => return package_error_response(&error),
    }
    // No version is stored, so none is minted: the checks never read the id.
    let content = version_content(package_id, PackageVersionId::new(Ulid::NIL), request);
    let found = match first_refusal(&state.packages, state.grid.as_ref(), tenant_id, &content).await
    {
        Ok(found) => found,
        Err(unavailable) => return unavailable,
    };
    let warnings = warnings_after(found.as_ref(), &content);
    let refusals = found.iter().map(Refusal::located).collect::<Vec<_>>();
    let verdict = PackageVersionDryRun {
        accepted: refusals.is_empty(),
        min_edge_version: content.min_edge_version.as_str().to_owned(),
        refusals,
        warnings: warnings.iter().map(PackageWarning::from).collect(),
    };
    (StatusCode::OK, Json(verdict)).into_response()
}
