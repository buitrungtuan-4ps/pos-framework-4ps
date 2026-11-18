// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The template-by-country matrix
//! ([ADR-0200](../../../docs/adr/0200-six-business-type-templates-ship-with-the-framework.md)
//! decision 6).
//!
//! Every business-type template (`business-types/`) is run against every shipped country pack
//! (`vn`, `jp`, `in` and the neutral `zz`): 24 cells. A cell is one store, an in-process edge over
//! the in-memory store (`Edge<FakeStore>`), configured through the same
//! [`session_from_config`](pos_edge::config_client::session_from_config) a synced store goes through,
//! from a document holding the pack's locale and tax table, the template's capability preset and
//! recommended settings, and its fake demo menu and floor. A till then drives it over the edge's own
//! HTTP routes, as the till does: it opens a shift, sells two lines (at a table where the template
//! serves at tables, at the counter otherwise), sends them to the kitchen where it has one, opens
//! the bill, pays the edge's figure in cash, and counts and closes the shift. A cell passes when
//! the bill settles with the first receipt number and the drawer closes with no variance.
//!
//! `pos-simulator matrix [--template <dir>] [--country <cc>]` runs one row, one column or all of
//! it, so a pull request that touches one template or one pack runs only its own cells.

mod sale;
mod store;
mod tab;

use pos_business_types::Template;
use pos_country::CountryModule;
use pos_proto::money::Money;

/// One cell's result: what the bill came to, and the drawer at close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellOutcome {
    /// The template's directory under `business-types/`.
    pub template: &'static str,
    /// The pack's country code.
    pub country: String,
    /// The receipt number the sale took.
    pub receipt_number: u64,
    /// What the guest paid: the bill's total due, in the pack's currency.
    pub paid: Money,
    /// What the drawer was expected to hold at close.
    pub expected_cash: Money,
}

/// Why a cell failed: the step, and what the edge answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellFailure {
    /// The template's directory under `business-types/`.
    pub template: &'static str,
    /// The pack's country code.
    pub country: String,
    /// The step of the sale that failed.
    pub step: &'static str,
    /// What went wrong, with the edge's status and answer where there was one.
    pub detail: String,
}

/// Which cells to run: every template and country, or one template's row, one country's column,
/// or one cell.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// A template's directory, such as `cafe`; `None` for all.
    pub template: Option<String>,
    /// A country code, such as `vn`; `None` for all.
    pub country: Option<String>,
}

impl Filter {
    /// Reads `--template <dir>` and `--country <cc>` from command-line arguments.
    ///
    /// # Errors
    ///
    /// A sentence naming the argument it could not read.
    pub fn from_args(args: &[String]) -> Result<Self, String> {
        let mut filter = Self::default();
        let mut rest = args.iter();
        while let Some(flag) = rest.next() {
            let value = rest
                .next()
                .ok_or_else(|| format!("{flag} needs a value"))?
                .to_ascii_lowercase();
            match flag.as_str() {
                "--template" => filter.template = Some(value),
                "--country" => filter.country = Some(value),
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(filter)
    }
}

/// Every shipped country pack, in the matrix's column order.
#[must_use]
pub fn packs() -> Vec<Box<dyn CountryModule>> {
    vec![
        Box::new(pos_country_vn::Vietnam),
        Box::new(pos_country_jp::Japan),
        Box::new(pos_country_in::India),
        Box::new(pos_country_zz::Zz),
    ]
}

/// One cell: a template and the country pack it is sold in.
pub type Cell = (Template, Box<dyn CountryModule>);

/// The cells `filter` selects, as (template, pack) pairs.
///
/// # Errors
///
/// A sentence when a template does not read, or the filter names a template or a country that is
/// not shipped, so a typo runs nothing rather than passing on zero cells.
pub fn cells(filter: &Filter) -> Result<Vec<Cell>, String> {
    let templates = pos_business_types::templates().map_err(|error| error.to_string())?;
    if let Some(wanted) = &filter.template
        && !templates
            .iter()
            .any(|template| template.directory == wanted)
    {
        return Err(format!("no template business-types/{wanted}"));
    }
    if let Some(wanted) = &filter.country
        && !packs().iter().any(|pack| code(pack.as_ref()) == *wanted)
    {
        return Err(format!("no country pack {wanted}"));
    }
    let mut cells = Vec::new();
    for template in templates {
        if filter
            .template
            .as_ref()
            .is_some_and(|wanted| template.directory != wanted)
        {
            continue;
        }
        for pack in packs() {
            if filter
                .country
                .as_ref()
                .is_some_and(|wanted| code(pack.as_ref()) != *wanted)
            {
                continue;
            }
            cells.push((template.clone(), pack));
        }
    }
    Ok(cells)
}

/// Runs every cell `filter` selects, one after another, each on a store of its own.
///
/// # Errors
///
/// A sentence when the filter selects nothing (see [`cells`]). A cell that fails is a
/// [`CellFailure`] in the result, never an early stop: the report shows the whole matrix.
pub async fn run(filter: &Filter) -> Result<Vec<Result<CellOutcome, CellFailure>>, String> {
    let mut results = Vec::new();
    for (template, pack) in cells(filter)? {
        results.push(run_cell(&template, pack.as_ref()).await);
    }
    Ok(results)
}

/// Runs one cell: one template in one country, sold end to end through the edge.
///
/// # Errors
///
/// [`CellFailure`] naming the step and what the edge answered.
pub async fn run_cell(
    template: &Template,
    pack: &dyn CountryModule,
) -> Result<CellOutcome, CellFailure> {
    let fail = |step: &'static str, detail: String| CellFailure {
        template: template.directory,
        country: code(pack),
        step,
        detail,
    };
    let till = store::Till::open(template, pack)
        .await
        .map_err(|detail| fail("open the store", detail))?;
    let sale = sale::sell(&till)
        .await
        .map_err(|(step, detail)| fail(step, detail))?;
    Ok(CellOutcome {
        template: template.directory,
        country: code(pack),
        receipt_number: sale.receipt_number,
        paid: sale.paid,
        expected_cash: sale.expected_cash,
    })
}

/// The matrix as text, one line a cell, and whether every cell passed.
#[must_use]
pub fn render(results: &[Result<CellOutcome, CellFailure>]) -> (String, bool) {
    let mut lines = vec![format!(
        "Template-by-country matrix: {} cell(s)",
        results.len()
    )];
    for result in results {
        lines.push(match result {
            Ok(cell) => format!(
                "  pass  {:<14} {:<3} receipt {} paid {} {} drawer {}",
                cell.template,
                cell.country,
                cell.receipt_number,
                cell.paid.amount_minor,
                cell.paid.currency_code.as_str(),
                cell.expected_cash.amount_minor,
            ),
            Err(failure) => format!(
                "  FAIL  {:<14} {:<3} at {}: {}",
                failure.template, failure.country, failure.step, failure.detail
            ),
        });
    }
    (lines.join("\n"), results.iter().all(Result::is_ok))
}

/// A pack's country code in lower case, as `countries/<cc>/` names it.
fn code(pack: &dyn CountryModule) -> String {
    pack.country_code().as_str().to_ascii_lowercase()
}

#[cfg(test)]
mod tests;
