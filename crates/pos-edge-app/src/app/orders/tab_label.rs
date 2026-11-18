// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A guest's label on a bar tab
//! ([ADR-0221](../../../../../docs/adr/0221-a-bar-tab-is-an-order-without-a-table-that-stays-open.md)
//! decision 2).
//!
//! A label is what staff type to find a tab again, usually the guest's name: personal data. It is
//! written only where the store's `tabs.guest_label` is on, which the cloud sets only with a
//! recorded confirmation of its notice (ADR-0169). It lives in the edge's subject store alone, under
//! a subject whose id is the tab's order id, so no event, log, ticket or cloud document ever names
//! it, nor even the subject that holds it. With no clock of its own, the store's retention sweep
//! masks it `retention_days` (the locale's `default_retention_days`) after it was first typed.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use pos_core::capability::Capability;
use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::TxContext;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::{SubjectRecord, SubjectStore};
use pos_proto::ids::{OrderId, SubjectId};
use pos_proto::tabs::{GuestLabel, LABEL_MAX_CHARS};

use crate::app::{AppError, Edge};

/// The subject record's field that holds the label.
const LABEL_FIELD: &str = "tab_label";

/// A label staff typed: trimmed, 1 to [`LABEL_MAX_CHARS`] characters, no control character.
///
/// Its `Debug` never shows the text, so a label cannot reach a log through a formatted value.
#[derive(Clone, PartialEq, Eq)]
pub struct TabLabel(String);

impl TabLabel {
    /// Reads what was typed: `Ok(None)` for nothing but spaces, which clears a label.
    ///
    /// # Errors
    ///
    /// The sentence a refusal says, for a label too long or with a control character.
    pub fn parse(text: &str) -> Result<Option<Self>, &'static str> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        if text.chars().count() > LABEL_MAX_CHARS {
            return Err("a tab's label has at most 32 characters");
        }
        if text.chars().any(char::is_control) {
            return Err("a tab's label has no control character");
        }
        Ok(Some(Self(text.to_owned())))
    }

    /// The text, for the subject store and the till only.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TabLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TabLabel(..)")
    }
}

/// The subject a tab's label is kept under: the order's own id.
fn label_subject(order_id: OrderId) -> SubjectId {
    SubjectId::new(order_id.as_ulid())
}

impl<S: EventStore + SubjectStore> Edge<S> {
    /// Writes, or with `None` clears, the label on an open tab, in the subject store only.
    ///
    /// # Errors
    ///
    /// [`AppError::Superseded`] on a replaced box, [`AppError::Domain`] without `sales.line.add` or
    /// on a store without tabs, [`AppError::GuestLabelOff`] where the store has not turned labels
    /// on, [`AppError::UnknownOrder`] for an order that is no open tab, or [`AppError::Port`] if the
    /// store cannot be written.
    pub async fn label_tab(
        &self,
        actor: Actor,
        order_id: OrderId,
        label: Option<TabLabel>,
    ) -> Result<(), AppError> {
        self.refuse_if_superseded()?;
        let ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::AddLine)?;
        ctx.require_capability(Capability::Tabs)?;
        if self.session().tabs.guest_label() != GuestLabel::On {
            return Err(AppError::GuestLabelOff);
        }
        if !self.lock_projection().is_open_tab(order_id) {
            return Err(AppError::UnknownOrder);
        }
        let store_id = self.identity.store_id;
        let subject_id = label_subject(order_id);
        // An edit keeps the label's clock, so a renamed tab is kept no longer than the first name.
        let mut record = self
            .store
            .fetch(store_id, subject_id)
            .await?
            .filter(|record| !record.is_masked())
            .unwrap_or_else(|| SubjectRecord::new(ctx.now, BTreeMap::new()));
        match label {
            Some(label) => {
                record
                    .fields
                    .insert(LABEL_FIELD.to_owned(), label.as_str().to_owned());
            }
            None => {
                record.fields.remove(LABEL_FIELD);
            }
        }
        let mut tx = self.store.begin().await?;
        self.store
            .record(&mut tx, store_id, subject_id, &record)
            .await?;
        tx.commit().await?;
        tracing::info!(
            target: "audit",
            action = "sales.tab.label",
            employee_id = %actor.employee_id,
            device_id = %actor.device_id,
            order_id = %order_id,
            "a tab's label was written"
        );
        Ok(())
    }

    /// The label on each of `order_ids` that has one still unmasked, where the store's labels are
    /// on; nothing where they are off, so turning the setting off hides every label at once.
    ///
    /// # Errors
    ///
    /// [`AppError::Port`] if the store cannot be read.
    pub async fn tab_labels(
        &self,
        order_ids: &[OrderId],
    ) -> Result<HashMap<OrderId, String>, AppError> {
        let mut labels = HashMap::new();
        if self.session().tabs.guest_label() != GuestLabel::On {
            return Ok(labels);
        }
        let store_id = self.identity.store_id;
        for order_id in order_ids {
            let record = self.store.fetch(store_id, label_subject(*order_id)).await?;
            if let Some(label) = record
                .filter(|record| !record.is_masked())
                .and_then(|mut record| record.fields.remove(LABEL_FIELD))
            {
                labels.insert(*order_id, label);
            }
        }
        Ok(labels)
    }
}
