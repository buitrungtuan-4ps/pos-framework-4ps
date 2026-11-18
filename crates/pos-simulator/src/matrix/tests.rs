// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use super::{Filter, cells, render, run};

fn filter(template: Option<&str>, country: Option<&str>) -> Filter {
    Filter {
        template: template.map(str::to_owned),
        country: country.map(str::to_owned),
    }
}

/// ADR-0200 decision 6: every template sells end to end in every shipped country.
#[tokio::test]
async fn every_template_sells_end_to_end_in_every_country() {
    let results = run(&Filter::default()).await;
    let results = results.unwrap_or_default();
    let (report, passed) = render(&results);
    assert_eq!(results.len(), 24, "{report}");
    assert!(passed, "{report}");
}

#[test]
fn a_template_selects_its_row_and_a_country_its_column() {
    assert_eq!(cells(&filter(Some("cafe"), None)).map(|c| c.len()), Ok(4));
    assert_eq!(cells(&filter(None, Some("jp"))).map(|c| c.len()), Ok(6));
    assert_eq!(
        cells(&filter(Some("bar"), Some("in"))).map(|c| c.len()),
        Ok(1)
    );
}

#[test]
fn a_filter_naming_nothing_shipped_is_refused_rather_than_passing_on_no_cells() {
    assert!(cells(&filter(Some("pizzeria"), None)).is_err());
    assert!(cells(&filter(None, Some("fr"))).is_err());
}

#[test]
fn the_command_line_reads_both_filters_and_refuses_anything_else() {
    let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        Filter::from_args(&args(&["--template", "Cafe", "--country", "VN"])),
        Ok(filter(Some("cafe"), Some("vn")))
    );
    assert!(Filter::from_args(&args(&["--template"])).is_err());
    assert!(Filter::from_args(&args(&["--fast"])).is_err());
}
