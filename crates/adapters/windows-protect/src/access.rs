//! Who may read a directory, judged from its access-control list (ADR-0174 decision 5).
//!
//! The judgement is plain data, so it is tested everywhere; reading a real list is Windows-only
//! ([`crate::acl`]). A directory holding a store's state may be read by SYSTEM, the
//! Administrators group and the account the store server runs as, and by nobody else.

/// SYSTEM.
pub const SYSTEM: &str = "S-1-5-18";
/// The local Administrators group.
pub const ADMINISTRATORS: &str = "S-1-5-32-544";

/// One entry of an access-control list, as far as the judgement needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The SID the entry is for, in string form (`S-1-…`).
    pub sid: String,
    /// Whether it allows reading the directory's contents.
    pub allows_read: bool,
    /// Whether it applies only to what is created inside, not to the directory itself.
    pub inherit_only: bool,
}

/// Who may read a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readers {
    /// SYSTEM, Administrators and the store server's own account, at most.
    OwnOnly,
    /// Someone else too.
    Others,
}

/// Judges an access-control list for a directory the account `own` keeps its state in. `None` is a
/// null list, which Windows reads as "everyone, everything".
#[must_use]
pub fn readers(list: Option<&[Entry]>, own: &str) -> Readers {
    let Some(entries) = list else {
        return Readers::Others;
    };
    let other = entries.iter().any(|entry| {
        entry.allows_read
            && !entry.inherit_only
            && entry.sid != SYSTEM
            && entry.sid != ADMINISTRATORS
            && entry.sid != own
    });
    if other {
        Readers::Others
    } else {
        Readers::OwnOnly
    }
}

#[cfg(test)]
mod tests {
    use super::{ADMINISTRATORS, Entry, Readers, SYSTEM, readers};

    const SERVICE: &str = "S-1-5-80-1111-2222-3333-4444-5555";
    const USERS: &str = "S-1-5-32-545";

    fn allow(sid: &str) -> Entry {
        Entry {
            sid: sid.to_owned(),
            allows_read: true,
            inherit_only: false,
        }
    }

    #[test]
    fn system_administrators_and_the_service_only_is_closed() {
        let list = [allow(SYSTEM), allow(ADMINISTRATORS), allow(SERVICE)];
        assert_eq!(readers(Some(&list), SERVICE), Readers::OwnOnly);
    }

    #[test]
    fn users_reading_it_is_too_open() {
        let list = [
            allow(SYSTEM),
            allow(ADMINISTRATORS),
            allow(SERVICE),
            allow(USERS),
        ];
        assert_eq!(readers(Some(&list), SERVICE), Readers::Others);
    }

    #[test]
    fn a_null_list_is_everyone() {
        assert_eq!(readers(None, SERVICE), Readers::Others);
    }

    #[test]
    fn an_entry_that_grants_no_read_or_applies_only_to_children_does_not_count() {
        let list = [
            allow(SYSTEM),
            Entry {
                sid: USERS.to_owned(),
                allows_read: false,
                inherit_only: false,
            },
            Entry {
                sid: "S-1-3-0".to_owned(),
                allows_read: true,
                inherit_only: true,
            },
        ];
        assert_eq!(readers(Some(&list), SERVICE), Readers::OwnOnly);
    }

    #[test]
    fn a_box_still_on_local_system_counts_system_as_its_own() {
        let list = [allow(SYSTEM), allow(ADMINISTRATORS)];
        assert_eq!(readers(Some(&list), SYSTEM), Readers::OwnOnly);
    }
}
