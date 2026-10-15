use std::collections::BTreeMap;

use pos_proto::ids::{MenuItemId, TaxClassId, TenantId};
use pos_proto::ulid::Ulid;

use super::{ImportError, ItemUpsert, NewItem, RowAction, parse_items_csv};
use crate::catalog::{CatalogItem, ItemCategoryId, ItemSubcategoryId};
use crate::media::MediaId;
use crate::registry::EntityStatus;
use crate::version::{Version, Versioned};

fn tenant() -> TenantId {
    TenantId::new(Ulid::from_u128(1))
}

fn tax_class() -> TaxClassId {
    TaxClassId::new(Ulid::from_u128(2))
}

fn category() -> ItemCategoryId {
    ItemCategoryId::new(Ulid::from_u128(3))
}

fn item_id() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(4))
}

/// One stored item, carrying a Vietnamese name the CSV format cannot express.
fn stored() -> Vec<Versioned<CatalogItem>> {
    vec![Versioned::new(
        CatalogItem {
            menu_item_id: item_id(),
            tenant_id: tenant(),
            name: "Pho".to_owned(),
            name_translations: BTreeMap::from([("vi".to_owned(), "Phở".to_owned())]),
            tax_class_id: tax_class(),
            item_category_id: None,
            item_subcategory_id: None,
            course_id: None,
            image_ref: None,
            status: EntityStatus::Active,
        },
        Version::new("v1"),
    )]
}

const HEADER: &str = "menu_item_id,name,status,tax_class_id,item_category_id,\
                      item_subcategory_id,image_ref\n";

#[test]
fn a_row_naming_a_stored_item_updates_it_and_a_row_naming_none_creates_one() {
    let csv = format!(
        "{HEADER}{id},Pho bo,active,{tax},{cat},,\n\
         ,Tra da,active,{tax},,,\n",
        id = item_id(),
        tax = tax_class(),
        cat = category(),
    );
    let (upserts, report) = parse_items_csv(
        csv.as_bytes(),
        tenant(),
        &stored(),
        &[tax_class()],
        &[category()],
        &[],
    )
    .expect("parse");

    assert_eq!(report.update_count, 1);
    assert_eq!(report.create_count, 1);
    assert_eq!(report.reject_count, 0);
    assert_eq!(report.rows[0].action, RowAction::Update);
    assert_eq!(report.rows[1].action, RowAction::Create);

    // The update carries the file's fields *and* the per-locale name the file could not carry.
    // An import that silently dropped translations would be a data loss nobody sees until a
    // Vietnamese till shows an English word.
    let ItemUpsert::Update { item, expected } = &upserts[0] else {
        panic!("the first row updates");
    };
    assert_eq!(item.name, "Pho bo");
    assert_eq!(item.item_category_id, Some(category()));
    assert_eq!(
        item.name_translations.get("vi").map(String::as_str),
        Some("Phở")
    );
    assert_eq!(expected, &Version::new("v1"), "the write stays conditional");
}

#[test]
fn an_id_this_tenant_does_not_have_is_rejected_rather_than_created() {
    // The id is the identity an inbound order names. A file that could mint one would let a
    // spreadsheet create a wire id — and, worse, could quietly adopt another tenant's.
    let csv = format!(
        "{HEADER}{stranger},Pho,active,{tax},,,\n",
        stranger = MenuItemId::new(Ulid::from_u128(99)),
        tax = tax_class(),
    );
    let (upserts, report) = parse_items_csv(
        csv.as_bytes(),
        tenant(),
        &stored(),
        &[tax_class()],
        &[],
        &[],
    )
    .expect("parse");
    assert!(upserts.is_empty());
    assert_eq!(report.reject_count, 1);
    let RowAction::Reject { reason } = &report.rows[0].action else {
        panic!("the row is rejected");
    };
    assert!(reason.contains("does not have"), "{reason}");
}

#[test]
fn a_foreign_key_the_tenant_does_not_have_is_rejected_by_name_and_the_file_is_read_on() {
    let csv = format!(
        "{HEADER},No tax,active,{stranger},,,\n\
         ,Fine,active,{tax},,,\n",
        stranger = TaxClassId::new(Ulid::from_u128(98)),
        tax = tax_class(),
    );
    let (upserts, report) = parse_items_csv(
        csv.as_bytes(),
        tenant(),
        &stored(),
        &[tax_class()],
        &[],
        &[],
    )
    .expect("parse");
    assert_eq!(report.reject_count, 1);
    assert_eq!(
        report.create_count, 1,
        "a bad row does not stop the file: the operator wants every verdict, not the first"
    );
    assert_eq!(upserts.len(), 1);
}

#[test]
fn a_missing_name_and_an_unknown_status_are_rejections_not_errors() {
    let csv = format!(
        "{HEADER},,active,{tax},,,\n\
         ,Pho,retired,{tax},,,\n",
        tax = tax_class(),
    );
    let (upserts, report) = parse_items_csv(
        csv.as_bytes(),
        tenant(),
        &stored(),
        &[tax_class()],
        &[],
        &[],
    )
    .expect("parse");
    assert!(upserts.is_empty());
    assert_eq!(report.reject_count, 2);
}

#[test]
fn an_escaped_name_is_read_without_the_quote_the_export_added() {
    let csv = format!(
        "{HEADER}{id},'-5 Pho,active,{tax},,,\n\
         ,'@Tra,active,{tax},,,\n\
         ,'Pho,active,{tax},,,\n",
        id = item_id(),
        tax = tax_class(),
    );
    let (upserts, report) = parse_items_csv(
        csv.as_bytes(),
        tenant(),
        &stored(),
        &[tax_class()],
        &[],
        &[],
    )
    .expect("parse");
    assert_eq!((report.update_count, report.create_count), (1, 2));
    let names: Vec<&str> = upserts
        .iter()
        .map(|upsert| match upsert {
            ItemUpsert::Update { item, .. } => item.name.as_str(),
            ItemUpsert::Create(new) => new.name.as_str(),
        })
        .collect();
    // A `'` before a trigger is the export's; one before anything else is the name's own.
    assert_eq!(names, ["-5 Pho", "@Tra", "'Pho"]);
    assert_eq!(
        report.rows[1].key, "@Tra",
        "the report names the row as it will be named"
    );
}

// --- pinned before `parse_items_csv` is split ------------------------------------------------

#[test]
fn an_update_applies_every_field_the_file_carries_and_takes_the_callers_tenant() {
    // Stored under another tenant id, so the overwrite is visible.
    let mut existing = stored();
    existing[0].record.tenant_id = TenantId::new(Ulid::from_u128(77));
    let subcategory = ItemSubcategoryId::new(Ulid::from_u128(5));
    let image = MediaId::new(Ulid::from_u128(6));
    let csv = format!(
        "{HEADER}{id},Pho ga,archived,{tax},{cat},{sub},{image}\n",
        id = item_id(),
        tax = tax_class(),
        cat = category(),
        sub = subcategory,
    );
    let (upserts, _) = parse_items_csv(
        csv.as_bytes(),
        tenant(),
        &existing,
        &[tax_class()],
        &[category()],
        &[subcategory],
    )
    .expect("parse");
    let [ItemUpsert::Update { item, .. }] = upserts.as_slice() else {
        panic!("one update");
    };
    assert_eq!(item.menu_item_id, item_id());
    assert_eq!(item.tenant_id, tenant());
    assert_eq!(item.name, "Pho ga");
    assert_eq!(item.status, EntityStatus::Archived);
    assert_eq!(item.tax_class_id, tax_class());
    assert_eq!(item.item_category_id, Some(category()));
    assert_eq!(item.item_subcategory_id, Some(subcategory));
    assert_eq!(item.image_ref, Some(image));
}

#[test]
fn a_create_carries_every_field_the_file_carries() {
    let csv = format!(
        "{HEADER},Tra da,archived,{tax},{cat},,\n",
        tax = tax_class(),
        cat = category(),
    );
    let (upserts, _) = parse_items_csv(
        csv.as_bytes(),
        tenant(),
        &stored(),
        &[tax_class()],
        &[category()],
        &[],
    )
    .expect("parse");
    let [ItemUpsert::Create(new)] = upserts.as_slice() else {
        panic!("one create");
    };
    assert_eq!(
        new,
        &NewItem {
            name: "Tra da".to_owned(),
            status: EntityStatus::Archived,
            tax_class_id: tax_class(),
            item_category_id: Some(category()),
            item_subcategory_id: None,
            image_ref: None,
        }
    );
}

#[test]
fn every_row_is_reported_in_file_order_under_its_id_or_else_its_name() {
    let csv = format!(
        "{HEADER}{id},Pho,active,{tax},,,\n\
         ,,active,{tax},,,\n\
         not-a-ulid,Bun,active,{tax},,,\n\
         ,'=Tra,active,{tax},,,\n\
         ,Com,retired,{tax},,,\n",
        id = item_id(),
        tax = tax_class(),
    );
    let (upserts, report) = parse_items_csv(
        csv.as_bytes(),
        tenant(),
        &stored(),
        &[tax_class()],
        &[],
        &[],
    )
    .expect("parse");
    let rows: Vec<(&str, String)> = report
        .rows
        .iter()
        .map(|row| {
            let action = match &row.action {
                RowAction::Create => "create".to_owned(),
                RowAction::Update => "update".to_owned(),
                RowAction::Reject { reason } => format!("reject: {reason}"),
            };
            (row.key.as_str(), action)
        })
        .collect();
    let id = item_id().to_string();
    assert_eq!(
        rows,
        [
            (id.as_str(), "update".to_owned()),
            ("", "reject: missing name".to_owned()),
            (
                "not-a-ulid",
                "reject: names an item this tenant does not have".to_owned()
            ),
            ("=Tra", "create".to_owned()),
            (
                "Com",
                "reject: `retired` is not `active` or `archived`".to_owned()
            ),
        ]
    );
    assert_eq!(
        (
            report.create_count,
            report.update_count,
            report.reject_count
        ),
        (1, 1, 3)
    );
    assert!(matches!(
        upserts.as_slice(),
        [ItemUpsert::Update { .. }, ItemUpsert::Create(_)]
    ));
}

#[test]
fn a_row_that_is_not_text_stops_the_import_as_malformed() {
    let mut bytes = format!("{HEADER},Pho,active,{tax},,,\n", tax = tax_class()).into_bytes();
    bytes.extend_from_slice(b",\xff\xfe,active,,,,\n");
    assert!(matches!(
        parse_items_csv(&bytes, tenant(), &stored(), &[tax_class()], &[], &[]),
        Err(ImportError::Malformed(_))
    ));
}

#[test]
fn a_header_that_is_not_the_export_s_is_an_error() {
    // The import accepts exactly what the export writes, so the pair round-trips. A file with
    // its own columns is a different format, and guessing at it is how a price column would
    // one day arrive by accident.
    let csv = "name,price\nPho,45000\n";
    assert!(
        parse_items_csv(
            csv.as_bytes(),
            tenant(),
            &stored(),
            &[tax_class()],
            &[],
            &[]
        )
        .is_err()
    );
}
