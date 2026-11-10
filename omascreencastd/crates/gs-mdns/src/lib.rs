//! mDNS advertisement of `_nvstream._tcp` so Moonlight can find the host.
//!
//! **Scaffold only.** The advertiser lands in M1-T9. The TXT record set is
//! fixed here because it is what the client filters on, and a reviewable list
//! is cheaper than debugging "the host does not appear".

#![deny(missing_docs)]

/// One TXT record entry, in wire order.
pub type TxtRecord = (String, String);

/// Build the TXT record set for a host.
///
/// The keys are the ones Moonlight inspects before it shows a host in its
/// list. Values that do not match the HTTPS `/serverinfo` payload make the
/// client hide or reject the host, so the two are built from the same inputs.
#[must_use]
pub fn txt_records(
    hostname: &str,
    uuid: &str,
    app_version: &str,
    gfe_version: &str,
) -> Vec<TxtRecord> {
    vec![
        ("txtvers".to_owned(), "1".to_owned()),
        ("version".to_owned(), gfe_version.to_owned()),
        ("hostname".to_owned(), hostname.to_owned()),
        ("uuid".to_owned(), uuid.to_owned()),
        ("appversion".to_owned(), app_version.to_owned()),
        ("GfeVersion".to_owned(), gfe_version.to_owned()),
        ("ht".to_owned(), "1".to_owned()),
        ("IsPrerelease".to_owned(), "0".to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_records_cover_every_key_the_client_filters_on() {
        let records = txt_records("host", "uuid-1", "7.1.431.-1", "7.1.431.0");
        let keys: Vec<&str> = records.iter().map(|(key, _)| key.as_str()).collect();
        for required in [
            "txtvers",
            "version",
            "hostname",
            "uuid",
            "appversion",
            "GfeVersion",
        ] {
            assert!(keys.contains(&required), "missing TXT key {required}");
        }
    }

    #[test]
    fn txt_values_echo_the_server_identity() {
        let records = txt_records("myhost", "uuid-9", "app-v", "gfe-v");
        let lookup = |key: &str| {
            records
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(lookup("hostname"), Some("myhost"));
        assert_eq!(lookup("uuid"), Some("uuid-9"));
        assert_eq!(lookup("appversion"), Some("app-v"));
        assert_eq!(lookup("GfeVersion"), Some("gfe-v"));
    }
}
