// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The parts a settled bill nests in its payload: its fee lines, its tax lines and their tax
//! components (ADR-0159, ADR-0168).

use serde::{Deserialize, Serialize};

use crate::fees::FeeCode;
use crate::ids::{FeeId, TaxClassId};
use crate::locale::TaxComponentName;
use crate::money::Money;
use crate::text::DisplayName;

/// One fee as a settled bill records it
/// ([ADR-0159](../../../../docs/adr/0159-a-fee-is-configuration.md) decision 4): which fee, under
/// which code and name, what it charged, and the tax on it.
///
/// The name is the rule's own as the bill froze it, so a copy of the receipt prints what the
/// original did after the fee is renamed, as a line's name does (ADR-0129, ADR-0144). No event
/// carries a translation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct BillFeeLine {
    /// The fee rule that charged it.
    pub fee_id: FeeId,
    /// The rule's code, what a report groups fees by.
    pub code: FeeCode,
    /// The rule's own name, as the bill froze it.
    pub display_name: DisplayName,
    /// What it charged.
    pub amount: Money,
    /// The tax on it: its part of the tax of every class it is taxed at. Zero for a fee that is
    /// not taxed.
    pub tax: Money,
    /// That tax by component
    /// ([ADR-0168](../../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 3):
    /// the tax on each of its class shares split as that class's tax line is, then summed by name
    /// and rate, so they sum to `tax`.
    ///
    /// Left out when empty, and absent is not zero: it reads as *not split*, which is every fee
    /// charged before this field existed and every fee taxed at rates with no components. A
    /// reader that meets a name it cannot read takes the fee as not split too, and still reads the
    /// bill.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "read_components"
    )]
    pub tax_components: Vec<BillTaxComponent>,
}

// The personal-data barrier for a part of a payload, field by field, and its fields in the snapshot.
crate::envelope::payload_part!(BillFeeLine {
    fee_id,
    code,
    display_name,
    amount,
    tax,
    tax_components,
});

/// One class's tax as a settled bill records it (ADR-0159 decision 4, roadmap-v3 B4.1): the base,
/// the rate and the tax, which rounded once on that whole base (ADR-0028), and that tax by component
/// where the rate has components (ADR-0104, ADR-0168).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct BillTaxLine {
    /// The class.
    pub tax_class_id: TaxClassId,
    /// The base the rate was applied to: the lines after discounts and comps, plus every charge
    /// taxed at this class.
    pub taxable_base: Money,
    /// The rate in force, in basis points.
    pub rate_basis_points: u32,
    /// The class's tax.
    pub tax: Money,
    /// That tax by component, in the order the `tax` node lists them
    /// ([ADR-0168](../../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 3):
    /// allocated by their rates and summing exactly to `tax`, with the residual on the last, as
    /// ADR-0104 splits it.
    ///
    /// Left out when empty, and absent is not zero: it reads as *not split*, which is every bill
    /// settled before this field existed and every rate with no components, as in Vietnam and Japan.
    /// A reader that meets a name it cannot read takes the line as not split too, and still reads
    /// the bill.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "read_components"
    )]
    pub components: Vec<BillTaxComponent>,
}

// The personal-data barrier for a part of a payload, field by field, and its fields in the snapshot.
crate::envelope::payload_part!(BillTaxLine {
    tax_class_id,
    taxable_base,
    rate_basis_points,
    tax,
    components,
});

/// One named part of a settled tax line's tax, or of a fee's
/// ([ADR-0168](../../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 3):
/// `CGST` at 9 % and the tax it accounts for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct BillTaxComponent {
    /// The component, as the `tax` node names it.
    pub name: TaxComponentName,
    /// Its part of the rate, in basis points.
    pub rate_basis_points: u32,
    /// Its part of the tax.
    pub tax: Money,
}

// The personal-data barrier for a part of a payload, field by field, and its fields in the snapshot.
crate::envelope::payload_part!(BillTaxComponent {
    name,
    rate_basis_points,
    tax,
});

/// Reads a line's components, all of them or none: a list with a name that is not a
/// [`TaxComponentName`] reads as empty, *not split*, and never as part of a split.
///
/// Lenient where [`TaxComponentName`] alone is strict, so a token a later release widens costs an
/// older reader the breakdown and not the bill: a settle it could not decode would leave the
/// cloud's rollup without the bill's revenue. Every other field reads as strictly as before.
fn read_components<'de, D>(deserializer: D) -> Result<Vec<BillTaxComponent>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    /// A component as the wire carries it. Its name is text only until it is checked, and a name
    /// that fails the check is dropped with the list.
    #[derive(Deserialize)]
    struct Unchecked {
        name: String,
        rate_basis_points: u32,
        tax: Money,
    }

    let unchecked = Vec::<Unchecked>::deserialize(deserializer)?;
    Ok(unchecked
        .into_iter()
        .map(|component| {
            TaxComponentName::new(&component.name).map(|name| BillTaxComponent {
                name,
                rate_basis_points: component.rate_basis_points,
                tax: component.tax,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_default())
}
