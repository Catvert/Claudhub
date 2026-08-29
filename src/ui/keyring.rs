//! Where a token lives, when it does not live in `settings.json`.
//!
//! Three forms, and they answer three different questions:
//!
//! - the value itself, in `settings.json` — written `0600`, and in clear;
//! - `$NAME`, read from the environment **of the worker**, because that is
//!   where the process that makes the request runs (`outside::from_env`);
//! - `keyring:…`, read from the system keyring **here**, because a keyring
//!   belongs to a desktop session — which is the Windows side when the workers
//!   live in WSL. Resolving it in the worker would look for a session bus a
//!   headless distribution does not have.
//!
//! That last line is the whole reason this module is on the interface's side of
//! the wire, and it is what the plugin system's secrets were resolved by before
//! Sentry became Rust again.

use std::collections::HashMap;
use std::sync::Mutex;

/// A keyring reference, as a settings file writes it.
///
/// Written `keyring:account` or `keyring:service/account`. The service defaults
/// to `claudhub`, which is what one wants nine times out of ten — and naming it
/// is what lets Claudhub read an entry some other program created.
#[derive(Debug, PartialEq, Eq)]
pub struct KeyringEntry {
    service: String,
    account: String,
}

impl KeyringEntry {
    const PREFIX: &'static str = "keyring:";
    const DEFAULT_SERVICE: &'static str = "claudhub";

    /// `None` when the value is not a keyring reference at all.
    pub fn parse(value: &str) -> Option<Self> {
        let rest = value.trim().strip_prefix(Self::PREFIX)?.trim();
        if rest.is_empty() {
            return None;
        }
        Some(match rest.split_once('/') {
            Some((service, account)) if !service.is_empty() && !account.is_empty() => Self {
                service: service.to_string(),
                account: account.to_string(),
            },
            // A lone slash on either side is a typo, not a service: what is
            // there is taken as the account under the default service, which is
            // what the writer meant.
            _ => Self {
                service: Self::DEFAULT_SERVICE.to_string(),
                account: rest.trim_matches('/').to_string(),
            },
        })
    }

    pub fn describe(&self) -> String {
        format!("keyring {}/{}", self.service, self.account)
    }

    fn read(&self) -> Result<String, keyring::Error> {
        keyring::Entry::new(&self.service, &self.account)?.get_password()
    }
}

/// The value a settings field stands for, keyring references resolved.
///
/// **Cached after the first read.** Opening a keyring can ask the user to
/// unlock it, and a panel that reads on every gesture would ask again and
/// again — which is what makes this worth a global rather than a call.
///
/// `None` when the reference names nothing: an unresolved placeholder sent as
/// the token is a request refused with a 401, which sends one looking at the
/// account rather than at the entry that is missing.
pub fn resolve(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let Some(entry) = KeyringEntry::parse(value) else {
        // Either the value itself or a `$NAME` the worker expands: both travel
        // as they stand.
        return Some(value.to_string());
    };
    static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    if let Ok(cache) = CACHE.lock() {
        if let Some(hit) = cache.as_ref().and_then(|cache| cache.get(value)) {
            return Some(hit.clone());
        }
    }
    match entry.read() {
        Ok(found) => {
            if let Ok(mut cache) = CACHE.lock() {
                cache
                    .get_or_insert_with(HashMap::new)
                    .insert(value.to_string(), found.clone());
            }
            Some(found)
        }
        Err(e) => {
            // Said and not silently empty: it is the difference between "the
            // token is wrong" and "there is no entry under that name".
            log::warn!("{}: {e}", entry.describe());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reference_names_a_service_and_an_account() {
        assert_eq!(
            KeyringEntry::parse("keyring:sentry.token"),
            Some(KeyringEntry {
                service: "claudhub".into(),
                account: "sentry.token".into(),
            })
        );
        assert_eq!(
            KeyringEntry::parse("keyring:gh/token"),
            Some(KeyringEntry {
                service: "gh".into(),
                account: "token".into(),
            })
        );
        // A lone slash is a typo, not a service.
        assert_eq!(
            KeyringEntry::parse("keyring:/token"),
            Some(KeyringEntry {
                service: "claudhub".into(),
                account: "token".into(),
            })
        );
        // Anything else is the value itself.
        assert_eq!(KeyringEntry::parse("sntrys_hunter2"), None);
        assert_eq!(KeyringEntry::parse("$SENTRY_TOKEN"), None);
        assert_eq!(KeyringEntry::parse("keyring:"), None);
    }

    /// What is not a reference travels as it stands — the value itself, and the
    /// `$NAME` the worker expands.
    #[test]
    fn what_names_no_keyring_is_handed_back_whole() {
        assert_eq!(resolve("sntrys_hunter2").as_deref(), Some("sntrys_hunter2"));
        assert_eq!(resolve("$SENTRY_TOKEN").as_deref(), Some("$SENTRY_TOKEN"));
        assert_eq!(resolve("   "), None);
    }
}
