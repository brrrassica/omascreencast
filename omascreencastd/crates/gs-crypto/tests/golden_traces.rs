//! Golden-trace replay for the pairing crypto (M1-T11).
//!
//! Loads the captured pairing known-answer vectors from
//! `tests/fixtures/pairing/` and replays them against the active
//! [`PairingCrypto`]. The corpus is empty until M0-T5 runs, so the test skips
//! (and says so) rather than failing; it starts asserting the moment a capture
//! lands.
//!
//! The crypto under test is [`ReferenceCrypto`] today. When M0-T5 pins the real
//! bytes, the production implementation replaces it here and the same fixtures
//! become the interop gate.

use gs_crypto::ReferenceCrypto;
use gs_crypto::replay::{fixtures_dir, load_fixtures, replay_all};

#[test]
fn pairing_corpus_replays_against_the_active_crypto() {
    let dir = fixtures_dir();
    let fixtures = load_fixtures(&dir).expect("fixtures should load");
    let crypto = ReferenceCrypto;
    replay_all(&crypto, &fixtures)
        .unwrap_or_else(|error| panic!("pairing corpus replay failed: {error}"));
    if fixtures.is_empty() {
        eprintln!(
            "M1-T11: no pairing fixtures in {} yet; the corpus lands with M0-T5",
            dir.display()
        );
    }
}
