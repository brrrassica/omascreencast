//! Golden-trace replay (M1-T11).
//!
//! Loads the captured HTTP exchanges from `tests/fixtures/http/` and replays
//! them against the real handler. The corpus is empty until M0-T5 runs, so the
//! test skips (and says so) rather than failing; it starts asserting the moment
//! a capture lands.

use std::net::Ipv4Addr;
use std::sync::Arc;

use gs_crypto::{Identity, TrustStore};
use gs_http::replay::{ServerSpec, fixtures_dir, load_fixtures, replay_all};
use gs_http::{DaemonState, NullEventSink, ServerConfig};

/// Build a state for one fixture file, honouring its `server` directive.
fn build_state(spec: Option<&ServerSpec>) -> (tempfile::TempDir, DaemonState) {
    let dir = tempfile::tempdir().unwrap();
    let paths = gs_crypto::paths_under(dir.path());
    let identity = Identity::generate("golden-host").unwrap();
    let uniqueid = DaemonState::derive_uniqueid(&identity);
    let config = match spec {
        Some(spec) => spec.to_config(uniqueid),
        None => ServerConfig::new(
            "golden-host",
            uniqueid,
            "00:11:22:33:44:55",
            Ipv4Addr::LOCALHOST,
            47984,
            48010,
        ),
    };
    let state = DaemonState::new(
        identity,
        paths,
        config,
        Arc::new(NullEventSink),
        TrustStore::empty(),
    );
    (dir, state)
}

#[test]
fn http_corpus_replays_against_the_real_handler() {
    let dir = fixtures_dir().join("http");
    let files = load_fixtures(&dir).expect("fixtures should load");
    let mut replayed = 0;
    for file in &files {
        let (_tmp, state) = build_state(file.server.as_ref());
        replay_all(&state, &file.fixtures)
            .unwrap_or_else(|error| panic!("{}: {error}", file.path.display()));
        replayed += file.fixtures.len();
    }
    if replayed == 0 {
        eprintln!(
            "M1-T11: no HTTP fixtures in {} yet; the corpus lands with M0-T5",
            dir.display()
        );
    }
}
