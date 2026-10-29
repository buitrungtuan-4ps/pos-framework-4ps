// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What `docs/guides/add-a-setting.md` says to check once a setting is in the register, run against
//! the setting the guide walks through.
//!
//! The guide quotes this test, and `cargo xtask guides --check` holds the quote to it. It lives
//! with the edge, which reads the setting, rather than in `pos-proto`, which an owner reviews.

use std::path::Path;

use pos_proto::settings::{SettingScope, SettingShape, ValueRefusal, register, render_snapshot};

/// The setting the guide walks through.
const KEY: &str = "session.idle_lock_seconds";

#[test]
fn the_setting_the_guide_walks_through_is_in_the_register_as_the_guide_says() {
    let guide = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/guides/add-a-setting.md");
    let guide = std::fs::read_to_string(guide).expect("read docs/guides/add-a-setting.md");
    assert!(guide.contains(KEY), "the guide walks through {KEY}");

    let register = register();
    let setting = register
        .iter()
        .find(|setting| setting.key() == KEY)
        .expect("the setting is in the register");

    // Its default is what the edge did before the field existed, so it is a value it takes.
    assert_eq!(setting.check(&setting.default_value()), Ok(()));
    // A value past its bound is refused when it is written and again when it is resolved.
    let SettingShape::Int { max, .. } = setting.shape else {
        panic!("{KEY} is a whole number");
    };
    assert_eq!(
        setting.check(&serde_json::json!(max + 1)),
        Err(ValueRefusal::OutOfRange)
    );
    // It may be written at every scope, from the tenant down to one store.
    assert!(setting.scopes.contains(&SettingScope::Tenant));
    assert!(setting.scopes.contains(&SettingScope::Store));
    // And the committed snapshot names it, one fact per line.
    assert!(render_snapshot().contains(&format!("{KEY}\tdefault=0\n")));
}
