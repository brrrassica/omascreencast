//! Golden-trace replay (M2-T9).
//!
//! Loads the captured RTSP `DESCRIBE`/`SETUP`/`PLAY`/`TEARDOWN` exchanges from
//! `tests/fixtures/rtsp/` and replays them against the real parser and the
//! `rtspenc` cipher. The corpus is empty until M0-T5 runs, so the test skips
//! (and says so) rather than failing; it starts asserting the moment a capture
//! lands.
//!
//! The cipher under test is the provisional [`gs_rtsp::RtspCipher`]. When M0-T5
//! pins the real `rtspenc` derivation, the production implementation replaces
//! it and the same fixtures become the interop gate.

use gs_rtsp::replay::{fixtures_dir, load_fixtures, replay_all};

#[test]
fn rtsp_corpus_replays_against_the_real_parser() {
    let dir = fixtures_dir();
    let fixtures = load_fixtures(&dir).expect("fixtures should load");
    replay_all(&fixtures).unwrap_or_else(|error| panic!("rtsp corpus replay failed: {error}"));
    if fixtures.is_empty() {
        eprintln!(
            "M2-T9: no RTSP fixtures in {} yet; the corpus lands with M0-T5",
            dir.display()
        );
    }
}
