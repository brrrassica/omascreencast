//! JSON DTOs for the GameStream HTTPS endpoints.
//!
//! The field names are part of the protocol: Moonlight reads them by exact
//! spelling and casing, so every struct here mirrors the wire names and the
//! serde renames are deliberate and pinned by tests.
//!
//! Shapes were taken from the M0 spike responder
//! (`tools/m0-gamestream/fake_server.py`) and will be reconciled
//! byte-for-byte with the oracle capture in M1-T11; where the two disagree, the
//! capture wins and this module changes.
//!
//! Forward compatibility: unknown fields from a newer client are ignored on
//! input, and optional fields are omitted from output rather than emitted as
//! `null`, because the client's JSON parser treats a present-but-null field as
//! a different thing from an absent one.

use serde::{Deserialize, Serialize};

use crate::constants::{APP_STATE_FREE, DESKTOP_APP_ID, STATE_FREE};

/// `/serverinfo` response.
///
/// Moonlight uses this to decide whether it can show a launch button: it needs
/// `hostname`, the version fields, `HttpsPort`, `PairStatus`, and the codec
/// mask. `SessionUrl0` is what `/launch` will later point at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Human-readable host name shown in the client's list.
    pub hostname: String,
    /// Protocol version, e.g. [`crate::PROTOCOL_VERSION`].
    pub appversion: String,
    /// Server-feature version, e.g. [`crate::SERVER_FEATURE_VERSION`].
    #[serde(rename = "GfeVersion")]
    pub gfe_version: String,
    /// Stable host identity. Clients cache trust against this value.
    pub uniqueid: String,
    /// HTTPS control port the client should use for subsequent requests.
    #[serde(rename = "HttpsPort")]
    pub https_port: u16,
    /// External port; identical to [`Self::https_port`] on a LAN.
    #[serde(rename = "ExternalPort")]
    pub external_port: u16,
    /// Host MAC address, lowercase colon-separated.
    pub mac: String,
    /// LAN address of the host.
    #[serde(rename = "LocalIP")]
    pub local_ip: String,
    /// Codec capability bitmask; see [`crate::SERVER_CODEC_MODE_SUPPORT`].
    #[serde(rename = "ServerCodecModeSupport")]
    pub server_codec_mode_support: u32,
    /// `1` when unpaired, `0` when at least one client is paired.
    #[serde(rename = "PairStatus")]
    pub pair_status: u8,
    /// Currently running app id, `0` when idle.
    ///
    /// Serialised as a 32-bit integer on the wire: the captured oracle emits
    /// values that do not fit a `u16`.
    pub currentgame: u32,
    /// Host state string; see [`STATE_FREE`].
    pub state: String,
    /// RTSP URL for the first session slot.
    #[serde(rename = "SessionUrl0")]
    pub session_url0: String,
}

impl ServerInfo {
    /// Build a `/serverinfo` payload for an idle host.
    #[must_use]
    pub fn idle(
        hostname: impl Into<String>,
        uniqueid: impl Into<String>,
        mac: impl Into<String>,
        local_ip: impl Into<String>,
        https_port: u16,
        rtsp_port: u16,
        pair_status: u8,
    ) -> Self {
        let local_ip = local_ip.into();
        Self {
            hostname: hostname.into(),
            appversion: crate::PROTOCOL_VERSION.to_owned(),
            gfe_version: crate::SERVER_FEATURE_VERSION.to_owned(),
            uniqueid: uniqueid.into(),
            https_port,
            external_port: https_port,
            mac: mac.into(),
            session_url0: format!("rtsp://{local_ip}:{rtsp_port}"),
            local_ip,
            server_codec_mode_support: crate::SERVER_CODEC_MODE_SUPPORT,
            pair_status,
            currentgame: 0,
            state: STATE_FREE.to_owned(),
        }
    }
}

/// `/applist` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppList {
    /// Host state string, matching `/serverinfo`'s `state`.
    #[serde(rename = "AppState")]
    pub app_state: String,
    /// Advertised apps.
    #[serde(rename = "App")]
    pub apps: Vec<App>,
}

impl AppList {
    /// The single synthetic "Desktop" app omascreencast advertises (M2-T5).
    #[must_use]
    pub fn desktop_only() -> Self {
        Self {
            app_state: APP_STATE_FREE.to_owned(),
            apps: vec![App::desktop()],
        }
    }
}

/// One entry in [`AppList::apps`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    /// Display title.
    #[serde(rename = "AppTitle")]
    pub title: String,
    /// App id as a **string**; the client sends it back as a query parameter.
    #[serde(rename = "ID")]
    pub id: String,
    /// `"1"`/`"0"` string rather than a boolean, matching what clients parse.
    #[serde(rename = "IsHdrSupported")]
    pub is_hdr_supported: String,
    /// Duplicate of the HDR flag as a real boolean; both are sent because
    /// different client versions read different ones.
    #[serde(rename = "HdrSupported")]
    pub hdr_supported: bool,
}

impl App {
    /// The synthetic desktop app entry.
    #[must_use]
    pub fn desktop() -> Self {
        Self {
            title: "Desktop".to_owned(),
            id: DESKTOP_APP_ID.to_string(),
            is_hdr_supported: "0".to_owned(),
            hdr_supported: false,
        }
    }
}

/// `/launch` and `/resume` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchResponse {
    /// Session id as a string; `"1"` is a single-session host.
    #[serde(rename = "Gamesession")]
    pub gamesession: String,
    /// RTSP URL the client should `DESCRIBE` next.
    #[serde(rename = "SessionUrl0")]
    pub session_url0: String,
}

impl LaunchResponse {
    /// Build a launch response pointing at `rtsp://{local_ip}:{rtsp_port}`.
    #[must_use]
    pub fn new(local_ip: &str, rtsp_port: u16) -> Self {
        Self {
            gamesession: "1".to_owned(),
            session_url0: format!("rtsp://{local_ip}:{rtsp_port}"),
        }
    }
}

/// Query parameters Moonlight sends on `/launch` and `/resume`.
///
/// Only the parameters the host actually acts on are typed; the rest are kept
/// verbatim in [`LaunchParams::extra`] so a trace replay can assert that the
/// host saw them without the host having to understand them yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchParams {
    /// Requested app id.
    #[serde(default)]
    pub appid: Option<u32>,
    /// Requested `WIDTHxHEIGHTxFPS` mode.
    #[serde(default)]
    pub mode: Option<String>,
    /// `1` when the client wants HDR.
    #[serde(default)]
    pub hdr: Option<u8>,
    /// `rikey` key material, if the client sent any.
    #[serde(default)]
    pub rikey: Option<String>,
    /// `rikeyid`, the key id paired with [`Self::rikey`].
    #[serde(default)]
    pub rikeyid: Option<i64>,
    /// `localAudio` flag.
    #[serde(default, rename = "localAudio")]
    pub local_audio: Option<u8>,
    /// `surroundAudioInfo` bitfield.
    #[serde(default, rename = "surroundAudioInfo")]
    pub surround_audio_info: Option<u32>,
    /// Parameters this host does not act on, preserved verbatim.
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, String>,
}

impl LaunchParams {
    /// Build parameters from decoded query-string pairs.
    ///
    /// Tolerant by design: a known field with an unparseable value becomes
    /// `None` and the pair is kept in [`Self::extra`], so a launch request is
    /// never rejected purely because one numeric field was malformed.
    /// Later duplicates overwrite earlier ones.
    #[must_use]
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut params = Self::default();
        for (key, value) in pairs {
            let key = key.as_ref();
            let value = value.as_ref();
            match key {
                "appid" => params.appid = value.parse().ok(),
                "mode" => params.mode = Some(value.to_owned()),
                "hdr" => params.hdr = value.parse().ok(),
                "rikey" => params.rikey = Some(value.to_owned()),
                "rikeyid" => params.rikeyid = value.parse().ok(),
                "localAudio" => params.local_audio = value.parse().ok(),
                "surroundAudioInfo" => params.surround_audio_info = value.parse().ok(),
                _ => {}
            }
            // Every pair is preserved verbatim, typed or not, so a trace replay
            // can assert exactly what the client sent.
            params.extra.insert(key.to_owned(), value.to_owned());
        }
        params
    }

    /// Parse the requested mode, if present and well-formed.
    ///
    /// Returns `None` when the client omitted `mode` or sent something
    /// unparseable; the caller decides whether to fall back to a default.
    #[must_use]
    pub fn stream_mode(&self) -> Option<crate::StreamMode> {
        self.mode
            .as_deref()
            .and_then(|raw| crate::StreamMode::parse(raw).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serverinfo_serialises_with_the_wire_field_names() {
        let info = ServerInfo::idle(
            "host",
            "uuid-1",
            "aa:bb:cc:dd:ee:ff",
            "10.0.0.5",
            47984,
            48010,
            1,
        );
        let json = serde_json::to_value(&info).unwrap();
        for key in [
            "hostname",
            "appversion",
            "GfeVersion",
            "uniqueid",
            "HttpsPort",
            "ExternalPort",
            "mac",
            "LocalIP",
            "ServerCodecModeSupport",
            "PairStatus",
            "currentgame",
            "state",
            "SessionUrl0",
        ] {
            assert!(json.get(key).is_some(), "missing wire field {key}");
        }
        assert_eq!(json["PairStatus"], 1);
        assert_eq!(json["SessionUrl0"], "rtsp://10.0.0.5:48010");
    }

    #[test]
    fn serverinfo_round_trips() {
        let info = ServerInfo::idle("host", "u", "m", "1.2.3.4", 1, 2, 0);
        let text = serde_json::to_string(&info).unwrap();
        assert_eq!(serde_json::from_str::<ServerInfo>(&text).unwrap(), info);
    }

    #[test]
    fn app_list_has_one_desktop_app_with_string_id() {
        let list = AppList::desktop_only();
        assert_eq!(list.app_state, APP_STATE_FREE);
        assert_eq!(list.apps.len(), 1);
        let json = serde_json::to_value(&list).unwrap();
        assert_eq!(json["App"][0]["AppTitle"], "Desktop");
        assert_eq!(json["App"][0]["ID"], "1");
        assert_eq!(json["App"][0]["IsHdrSupported"], "0");
        assert_eq!(json["App"][0]["HdrSupported"], false);
    }

    #[test]
    fn launch_response_points_at_the_rtsp_port() {
        let response = LaunchResponse::new("192.168.1.9", 48010);
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["Gamesession"], "1");
        assert_eq!(json["SessionUrl0"], "rtsp://192.168.1.9:48010");
    }

    #[test]
    fn launch_params_keep_unknown_query_fields() {
        // Query parameters are strings on the wire. Parsing them through
        // `from_pairs` is what the HTTP layer will do; values the host does not
        // understand yet still survive in `extra`.
        let pairs = [
            ("appid", "1"),
            ("mode", "1920x1080x60"),
            ("rikey", "k"),
            ("rikeyid", "7"),
            ("localAudio", "1"),
            ("surroundAudioInfo", "196610"),
            ("framerate", "60"),
        ];
        let params = LaunchParams::from_pairs(pairs.iter().copied());
        assert_eq!(params.appid, Some(1));
        assert_eq!(params.mode.as_deref(), Some("1920x1080x60"));
        assert_eq!(params.rikey.as_deref(), Some("k"));
        assert_eq!(params.rikeyid, Some(7));
        assert_eq!(params.surround_audio_info, Some(196_610));
        assert_eq!(
            params.extra.get("framerate").map(String::as_str),
            Some("60")
        );
    }

    #[test]
    fn launch_params_ignore_unparseable_known_fields() {
        // A malformed numeric field must not sink the whole request: the host
        // falls back rather than rejecting a launch that might otherwise work.
        let pairs = [
            ("appid", "not-a-number"),
            ("rikeyid", "x"),
            ("mode", "1280x720x60"),
        ];
        let params = LaunchParams::from_pairs(pairs.iter().copied());
        assert_eq!(params.appid, None);
        assert_eq!(params.rikeyid, None);
        assert_eq!(params.stream_mode().map(|mode| mode.width), Some(1280));
    }

    #[test]
    fn launch_params_mode_is_clampable_via_stream_mode() {
        let params: LaunchParams = serde_json::from_str(r#"{"mode":"3840x2160x120"}"#).unwrap();
        let mode = params.stream_mode().unwrap().clamp();
        assert_eq!(mode.width, crate::MAX_WIDTH);
        assert_eq!(mode.fps, crate::MAX_FPS);
    }

    #[test]
    fn launch_params_absent_mode_is_none() {
        let params = LaunchParams::default();
        assert!(params.stream_mode().is_none());
    }
}
