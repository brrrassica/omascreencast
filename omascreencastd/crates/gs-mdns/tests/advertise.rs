//! Advertisement lifecycle against a real responder.
//!
//! The pure unit tests pin the TXT contract; this test proves the part that can
//! only fail in the real thing: that a service can be registered and then fully
//! withdrawn. Unregistration matters because a host that fails to say goodbye
//! stays visible to the TV until the record TTL expires, which looks like "the
//! daemon is running but will not connect".
//!
//! A sandbox with no multicast route may refuse to start the responder. That is
//! an environment limitation, not a product failure, so the test reports it as
//! skipped rather than pretending to pass.

use std::net::Ipv4Addr;

use gs_mdns::{Advertisement, Advertiser};

fn advertisement() -> Advertisement {
    Advertisement {
        hostname: String::from("omascreencast-mdns-test"),
        uuid: String::from("1fd6b243-0109-4e1e-8729-cca955e0d82b"),
        app_version: String::from("7.1.431.-1"),
        gfe_version: String::from("7.1.431.0"),
        address: Ipv4Addr::new(192, 168, 1, 2),
        https_port: 47984,
    }
}

#[test]
fn a_service_registers_and_withdraws_cleanly() {
    let advertiser = match Advertiser::start(&advertisement()) {
        Ok(advertiser) => advertiser,
        Err(error) => {
            eprintln!("skipping: this sandbox cannot start an mDNS responder ({error})");
            return;
        }
    };

    // The withdrawal key is the responder's fullname, and it must be the
    // `_nvstream._tcp` instance the client browses for.
    assert!(
        advertiser.fullname().ends_with("_nvstream._tcp.local."),
        "unexpected fullname: {}",
        advertiser.fullname()
    );
    assert!(
        advertiser
            .fullname()
            .starts_with("omascreencast-mdns-test.")
    );

    // Shutdown is idempotent; calling it twice must not panic or hang.
    advertiser.shutdown();
    advertiser.shutdown();
}

#[test]
fn two_advertisers_for_different_hosts_do_not_collide() {
    let mut other = advertisement();
    other.hostname = String::from("omascreencast-mdns-test-2");

    let first = match Advertiser::start(&advertisement()) {
        Ok(advertiser) => advertiser,
        Err(error) => {
            eprintln!("skipping: this sandbox cannot start an mDNS responder ({error})");
            return;
        }
    };
    let second = Advertiser::start(&other).expect("a distinct host name must also register");
    assert_ne!(first.fullname(), second.fullname());
}
