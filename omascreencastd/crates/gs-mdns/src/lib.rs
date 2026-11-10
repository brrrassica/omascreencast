//! mDNS advertisement of `_nvstream._tcp` so Moonlight can find the host
//! (M1-T9).
//!
//! # Why the TXT set is a pure function
//!
//! The TXT records are what the client filters on before it will show a host at
//! all. If a record disagrees with the `/serverinfo` payload — a different
//! `appversion`, a different `uuid` — the host either disappears from the list
//! or appears and then is rejected on connect, and the symptom looks like "the
//! daemon is not running". So the record set is built from the same inputs as
//! `/serverinfo`, by a pure function that a test can pin, and the socket
//! plumbing is kept separate from it.
//!
//! # Lifecycle
//!
//! [`Advertiser`] owns the responder and the registered service. It withdraws
//! the service on [`Advertiser::shutdown`] and again on `Drop`, because a
//! crashed-but-not-reaped process would otherwise leave clients pointing at a
//! dead host until the record's TTL runs out. Withdrawal is best-effort:
//! failing to send a goodbye must not turn a clean daemon stop into an error.
//!
//! # Discovery is not advertised as a dependency
//!
//! Nothing here browses. The panel is the advertiser and the TV is the client,
//! which is the inversion recorded in plan §5.2; a browse path would be dead
//! code that implies we still scan for receivers.

#![deny(missing_docs)]

use std::collections::HashMap;
use std::sync::Mutex;

use mdns_sd::{ServiceDaemon, ServiceInfo};

/// One TXT record entry, in wire order.
pub type TxtRecord = (String, String);

/// The GameStream service type the client browses for.
pub const SERVICE_TYPE: &str = "_nvstream._tcp.local.";

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

/// Everything the responder needs to publish one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertisement {
    /// Name shown in the client's list, and the mDNS instance name.
    pub hostname: String,
    /// Stable host id; must equal `/serverinfo`'s `uniqueid`.
    pub uuid: String,
    /// Protocol version string; must equal `/serverinfo`'s `appversion`.
    pub app_version: String,
    /// Server-feature version; must equal `/serverinfo`'s `GfeVersion`.
    pub gfe_version: String,
    /// LAN address the client should connect to.
    pub address: std::net::Ipv4Addr,
    /// HTTPS control port.
    pub https_port: u16,
}

impl Advertisement {
    /// Build the TXT record set for this advertisement.
    #[must_use]
    pub fn txt_records(&self) -> Vec<TxtRecord> {
        txt_records(
            &self.hostname,
            &self.uuid,
            &self.app_version,
            &self.gfe_version,
        )
    }

    /// The `.local.` hostname the responder publishes for this host.
    ///
    /// Kept distinct from [`Self::instance_name`] because mDNS treats them as
    /// different records: one names the service instance, the other names the
    /// machine's A record. Using the instance name here would make the
    /// responder publish an invalid host record.
    #[must_use]
    pub fn mdns_hostname(&self) -> String {
        format!("{}.local.", self.hostname)
    }

    /// Build the responder's service description.
    ///
    /// # Errors
    ///
    /// [`AdvertiseError::Build`] when the hostname or a TXT value is not
    /// publishable — for example a hostname containing a dot, or a value over
    /// the 255-byte DNS limit. That is a configuration error, not a transient
    /// one, so it is reported rather than retried.
    pub fn service_info(&self) -> Result<ServiceInfo, AdvertiseError> {
        let properties: HashMap<String, String> = self.txt_records().into_iter().collect();
        ServiceInfo::new(
            SERVICE_TYPE,
            &self.hostname,
            &self.mdns_hostname(),
            self.address.to_string(),
            self.https_port,
            properties,
        )
        .map_err(|error| AdvertiseError::Build(error.to_string()))
    }
}

/// Why advertisement could not start, stop, or be built.
#[derive(Debug, thiserror::Error)]
pub enum AdvertiseError {
    /// The mDNS responder could not be started, usually because the multicast
    /// socket could not be bound.
    #[error("mdns responder: {0}")]
    Daemon(String),
    /// The service description was rejected before it reached the wire.
    #[error("cannot advertise this host: {0}")]
    Build(String),
    /// The responder accepted the registration call but could not publish the
    /// service, for example because every interface was down.
    #[error("mdns registration failed: {0}")]
    Register(String),
}

/// A registered `_nvstream._tcp` advertisement.
///
/// Holds the responder alive; dropping the advertiser withdraws the service.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
    /// Guards against a double unregister from `shutdown` followed by `Drop`.
    withdrawn: Mutex<bool>,
}

impl std::fmt::Debug for Advertiser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Advertiser")
            .field("fullname", &self.fullname)
            .finish_non_exhaustive()
    }
}

impl Advertiser {
    /// Start a responder and publish `advertisement`.
    ///
    /// # Errors
    ///
    /// [`AdvertiseError`] when the responder cannot start or the service
    /// description is rejected.
    pub fn start(advertisement: &Advertisement) -> Result<Self, AdvertiseError> {
        let info = advertisement.service_info()?;
        let fullname = info.get_fullname().to_owned();
        let daemon =
            ServiceDaemon::new().map_err(|error| AdvertiseError::Daemon(error.to_string()))?;
        daemon
            .register(info)
            .map_err(|error| AdvertiseError::Register(error.to_string()))?;
        Ok(Self {
            daemon,
            fullname,
            withdrawn: Mutex::new(false),
        })
    }

    /// The fully-qualified name this advertiser owns.
    #[must_use]
    pub fn fullname(&self) -> &str {
        &self.fullname
    }

    /// Withdraw the service. Idempotent.
    ///
    /// The goodbye packet is sent by the responder thread, so a bounded wait is
    /// made for it to be acknowledged. A timeout is not an error: the record
    /// still expires on its own, and a shutdown should not hang on the network.
    pub fn shutdown(&self) {
        let Ok(mut withdrawn) = self.withdrawn.lock() else {
            return;
        };
        if *withdrawn {
            return;
        }
        *withdrawn = true;
        if let Ok(receiver) = self.daemon.unregister(&self.fullname) {
            let _ = receiver.recv_timeout(WITHDRAW_TIMEOUT);
        }
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        self.shutdown();
        let _ = self.daemon.shutdown();
    }
}

/// How long a clean stop waits for the goodbye packet to be acknowledged.
pub const WITHDRAW_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(test)]
mod tests {
    use super::*;

    fn advertisement() -> Advertisement {
        Advertisement {
            hostname: String::from("omascreencast-test"),
            uuid: String::from("1fd6b243-0109-4e1e-8729-cca955e0d82b"),
            app_version: String::from("7.1.431.-1"),
            gfe_version: String::from("7.1.431.0"),
            address: std::net::Ipv4Addr::new(192, 168, 1, 2),
            https_port: 47984,
        }
    }

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

    #[test]
    fn advertisement_records_match_serverinfo_inputs() {
        let advertisement = advertisement();
        let records = advertisement.txt_records();
        let lookup = |key: &str| {
            records
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(lookup("hostname").as_deref(), Some("omascreencast-test"));
        assert_eq!(
            lookup("uuid").as_deref(),
            Some("1fd6b243-0109-4e1e-8729-cca955e0d82b")
        );
        assert_eq!(lookup("appversion").as_deref(), Some("7.1.431.-1"));
    }

    #[test]
    fn the_instance_name_and_the_host_record_are_distinct() {
        let advertisement = advertisement();
        let info = advertisement.service_info().unwrap();
        assert_eq!(
            info.get_fullname(),
            "omascreencast-test._nvstream._tcp.local."
        );
        assert_eq!(advertisement.mdns_hostname(), "omascreencast-test.local.");
        assert_ne!(info.get_fullname(), advertisement.mdns_hostname());
    }

    #[test]
    fn service_info_carries_the_port_and_address() {
        let info = advertisement().service_info().unwrap();
        assert_eq!(info.get_port(), 47984);
        assert_eq!(info.get_hostname(), "omascreencast-test.local.");
        assert_eq!(
            info.get_fullname(),
            "omascreencast-test._nvstream._tcp.local."
        );
    }

    #[test]
    fn a_dot_in_the_hostname_is_escaped_not_silently_mangled() {
        // `mdns-sd` escapes rather than rejects, so the fullname keeps the dot
        // as data (`bad\\.name`). This test exists because a hand-built
        // `format!("{hostname}.{type}")` would *not* escape, and the mismatch
        // would make `unregister` fail against a name it never registered.
        let mut advertisement = advertisement();
        advertisement.hostname = String::from("bad.name");
        let info = advertisement.service_info().unwrap();
        assert_eq!(info.get_fullname(), "bad\\.name._nvstream._tcp.local.");
        // Crucially, the name used for withdrawal is the responder's, not ours.
        assert_ne!(
            info.get_fullname(),
            format!("bad.name.{SERVICE_TYPE}"),
            "a hand-built name must not be used as an unregister key"
        );
    }

    #[test]
    fn an_empty_hostname_still_produces_a_parseable_service() {
        // An empty name is publishable (`.` + type). It is ugly, not fatal, and
        // the daemon supplies a real name; the point is that construction does
        // not panic and the returned fullname is what gets withdrawn.
        let mut advertisement = advertisement();
        advertisement.hostname = String::new();
        let info = advertisement.service_info().unwrap();
        assert_eq!(info.get_fullname(), format!(".{SERVICE_TYPE}"));
    }
}
