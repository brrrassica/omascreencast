//! `omascreencastd` — the 0.2.0 GameStream host.
//!
//! M1 lands the identity, pairing, trust store, and IPC surface. `serve` starts
//! the HTTPS listener, the event socket, and the control socket; `pair` drives
//! operator approval over the control socket. RTSP and streaming arrive in M2.
//!
//! One binary with subcommands is D13: there is deliberately no second CLI.

#![deny(missing_docs)]

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use gs_crypto::{Identity, TrustStore, identity_paths, paths_under};
use gs_proto::ipc::{CommandReply, IpcCommand};
use osc_daemon::control::{control_path, send_command};
use osc_daemon::{
    ControlServer, DaemonState, HttpServer, HubEventSink, IpcHub, IpcServer, command_handler,
};
use tracing_subscriber::EnvFilter;

/// Hostname used when `/proc` is unavailable.
const FALLBACK_HOSTNAME: &str = "omascreencast";

/// The 0.2.0 GameStream host for omascreencast.
#[derive(Debug, Parser)]
#[command(
    name = "omascreencastd",
    version,
    about = "Moonlight-compatible GameStream host (0.2.0 transport)",
    long_about = "A clean-room GameStream host. The client discovers this \
                  daemon over mDNS, pairs with a PIN, and launches a stream."
)]
struct Cli {
    /// Override the data directory (default: `$XDG_DATA_HOME/omascreencast/gamestream`).
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Increase logging verbosity (`-v`, `-vv`).
    #[arg(long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create or load the server identity and print its fingerprint.
    Identity,
    /// Run the HTTPS, IPC, and control listeners (M1-T5..T10).
    Serve {
        /// Address to bind the HTTPS listener to.
        #[arg(long, default_value = "0.0.0.0")]
        bind: Ipv4Addr,
        /// HTTPS port; `0` asks the OS for an ephemeral port.
        #[arg(long, default_value_t = gs_proto::DEFAULT_HTTPS_PORT)]
        https_port: u16,
        /// RTSP port advertised to clients (bound in M2).
        #[arg(long, default_value_t = gs_proto::DEFAULT_RTSP_PORT)]
        rtsp_port: u16,
        /// MAC address to advertise. Defaults to a locally-administered value
        /// derived from nothing, which is honest: this host has no MAC of its
        /// own in the protocol's sense.
        #[arg(long, default_value = "00:11:22:33:44:55")]
        mac: String,
    },
    /// Show or approve a pending pairing request (M1-T7).
    Pair {
        /// The four digits displayed on the TV. Omit to list pending requests.
        #[arg(long, value_name = "PIN")]
        pin: Option<String>,
        /// Pairing id, when more than one request is pending.
        #[arg(long, value_name = "ID")]
        id: Option<String>,
    },
    /// List the clients that have paired with this host (M1-T8).
    Clients,
    /// Forget a paired client, by id (M1-T8).
    Unpair {
        /// Pairing id shown by `clients`.
        id: String,
    },
    /// Check the host's prerequisites.
    Doctor,
}

fn main() {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let result = match &cli.command {
        Command::Identity => identity(&cli),
        Command::Clients => clients(&cli),
        Command::Unpair { id } => unpair(&cli, id),
        Command::Serve {
            bind,
            https_port,
            rtsp_port,
            mac,
        } => serve(&cli, *bind, *https_port, *rtsp_port, mac),
        Command::Pair { pin, id } => pair(pin.as_deref(), id.as_deref()),
        // Not yet implemented; keep the failure explicit rather than silently
        // exiting 0, so a half-built daemon is never mistaken for a working one.
        Command::Doctor => Err(Box::new(NotYetImplemented) as Box<dyn std::error::Error>),
    };

    if let Err(error) = result {
        eprintln!("omascreencastd: {error}");
        std::process::exit(1);
    }
}

fn identity(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let paths = resolved_paths(cli)?;
    let common_name = hostname();
    let identity = Identity::load_or_generate(&paths, &common_name)?;
    println!("identity: {}", paths.cert.display());
    println!("key:      {}", paths.key.display());
    println!("subject:  {common_name}");
    println!("uniqueid: {}", DaemonState::derive_uniqueid(&identity));
    println!("bits:     {}", gs_crypto::identity::KEY_BITS);
    println!("sha256:   {}", identity.fingerprint_hex());
    Ok(())
}

fn serve(
    cli: &Cli,
    bind: Ipv4Addr,
    https_port: u16,
    rtsp_port: u16,
    mac: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // `/serverinfo` has to name the port the client should use, so an ephemeral
    // request is resolved before the config is built rather than after bind.
    let https_port = resolve_https_port(bind, https_port)?;
    let paths = resolved_paths(cli)?;
    let common_name = hostname();
    let identity = Identity::load_or_generate(&paths, &common_name)?;
    // A corrupt trust store is fatal: starting empty would silently un-pair
    // every device while looking healthy.
    let trust = TrustStore::load(&paths)?;

    let hub = IpcHub::new();
    let events = Arc::new(HubEventSink::new(hub.clone()));
    let event_hub = hub.clone();
    let local_ip = gs_http::local_ipv4();
    let uniqueid = DaemonState::derive_uniqueid(&identity);
    let config = gs_http::ServerConfig::new(
        &common_name,
        &uniqueid,
        mac,
        local_ip,
        https_port,
        rtsp_port,
    );
    let state = DaemonState::new(identity, paths.clone(), config.clone(), events, trust);

    // Events first, so the panel can see the phase as soon as anything moves.
    let mut ipc = IpcServer::bind(osc_daemon::socket_path()?, event_hub)?;
    let mut control = ControlServer::bind(control_path()?, command_handler(state.clone()))?;
    let mut https = HttpServer::bind(bind, https_port, state.clone())?;

    // Advertise last, so the host is not discoverable before it can answer.
    // A responder that cannot start is reported and skipped rather than fatal:
    // the host is still usable by IP, and D16 authorises by pairing, not by
    // discovery.
    let advertisement = gs_mdns::Advertisement {
        hostname: config.hostname.clone(),
        uuid: config.uniqueid.clone(),
        app_version: gs_proto::PROTOCOL_VERSION.to_owned(),
        gfe_version: gs_proto::SERVER_FEATURE_VERSION.to_owned(),
        address: local_ip,
        https_port: https.local_addr().port(),
    };
    let advertiser = match gs_mdns::Advertiser::start(&advertisement) {
        Ok(advertiser) => Some(advertiser),
        Err(error) => {
            eprintln!("omascreencastd: mDNS advertisement unavailable: {error}");
            eprintln!(
                "omascreencastd: Moonlight will not discover this host automatically; \
                 connect by IP to {}",
                https.local_addr()
            );
            None
        }
    };

    ipc.hub().emit(&gs_proto::ipc::IpcEvent::phase(
        gs_proto::ipc::phase::STARTING,
    ));
    println!("https:   https://{}", https.local_addr());
    println!("event:   {}", ipc.path().display());
    println!("control: {}", control.path().display());
    println!("uniqueid: {}", config.uniqueid);
    println!(
        "mdns:    {}",
        advertiser
            .as_ref()
            .map_or("unavailable", gs_mdns::Advertiser::fullname)
    );
    println!(
        "paired:  {} client(s) in {}",
        state.paired_count(),
        paths.clients.display()
    );

    wait_for_shutdown();

    // Withdraw before closing the listener: a client that still sees the
    // record must not be pointed at a closed port.
    if let Some(advertiser) = &advertiser {
        advertiser.shutdown();
    }
    https.shutdown();
    control.shutdown();
    ipc.shutdown();
    Ok(())
}

fn pair(pin: Option<&str>, id: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let path = control_path()?;
    let command = match pin {
        None => IpcCommand::PairingList,
        Some(pin) => {
            let id = match id {
                Some(id) => id.to_owned(),
                None => {
                    // With exactly one pending request the id is inferable;
                    // otherwise the operator must disambiguate.
                    match send_command(&path, &IpcCommand::PairingList)? {
                        CommandReply::PairingList { pending } if pending.len() == 1 => {
                            pending[0].id.clone()
                        }
                        CommandReply::PairingList { pending } if pending.is_empty() => {
                            return Err("no pairing request is waiting".into());
                        }
                        CommandReply::PairingList { pending } => {
                            return Err(format!(
                                "{} pairing requests are pending; pass --id with one of: {}",
                                pending.len(),
                                pending
                                    .iter()
                                    .map(|request| format!("{} ({})", request.id, request.name))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                            .into());
                        }
                        other => return Err(format!("unexpected reply: {other:?}").into()),
                    }
                }
            };
            IpcCommand::PairingApprove {
                id,
                pin: pin.to_owned(),
            }
        }
    };

    match send_command(&path, &command)? {
        CommandReply::PairingList { pending } if pending.is_empty() => {
            println!("no pairing request is waiting");
        }
        CommandReply::PairingList { pending } => {
            for request in pending {
                println!("{}\t{}\t{}", request.id, request.name, request.address);
            }
        }
        CommandReply::Ack { ok: true, detail } => {
            println!("{}", detail.unwrap_or_else(|| String::from("ok")));
        }
        CommandReply::Ack { ok: false, detail } => {
            println!("{}", detail.unwrap_or_else(|| String::from("failed")));
        }
        CommandReply::Status { .. } => {
            println!("use `clients` for the paired-client list");
        }
        CommandReply::Error { detail } => return Err(detail.into()),
    }
    Ok(())
}

fn clients(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let paths = resolved_paths(cli)?;
    let store = TrustStore::load(&paths)?;
    if store.is_empty() {
        println!("no paired clients");
        return Ok(());
    }
    for client in store.clients() {
        println!(
            "{}\t{}\t{}\t{}",
            client.id, client.name, client.address, client.fingerprint
        );
    }
    Ok(())
}

fn unpair(cli: &Cli, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let paths = resolved_paths(cli)?;
    let mut store = TrustStore::load(&paths)?;
    if store.remove(id, &paths)? {
        println!("unpaired {id}");
    } else {
        // Idempotent: unpairing an absent client is not a failure, but say so.
        println!("no paired client with id {id}");
    }
    Ok(())
}

/// Resolve the port to advertise.
///
/// A non-zero request is used as-is. Zero asks the OS for a free port, which is
/// then advertised; the listener is bound to that exact port afterwards. The
/// gap between releasing the probe socket and binding the real one is why this
/// is documented as a development convenience rather than the normal path.
fn resolve_https_port(bind: Ipv4Addr, port: u16) -> std::io::Result<u16> {
    if port != 0 {
        return Ok(port);
    }
    let probe = std::net::TcpListener::bind((bind, 0))?;
    let resolved = probe.local_addr()?.port();
    drop(probe);
    Ok(resolved)
}

fn resolved_paths(cli: &Cli) -> Result<gs_crypto::IdentityPaths, Box<dyn std::error::Error>> {
    Ok(match &cli.data_dir {
        Some(dir) => paths_under(dir),
        None => identity_paths()?,
    })
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|value| value.trim().to_owned())
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| String::from(FALLBACK_HOSTNAME))
}

/// Block until the process is asked to stop.
///
/// SIGINT/SIGTERM are not handled specially yet: the default disposition
/// terminates the process, and the listener sockets are removed by their
/// `Drop` implementations only on a clean exit. M6-T4 owns crash-only
/// recovery, including the stale-socket cleanup that makes a kill safe.
fn wait_for_shutdown() {
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn init_tracing(verbose: u8) {
    let default = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("omascreencastd={default}")));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

#[derive(Debug)]
struct NotYetImplemented;

impl std::fmt::Display for NotYetImplemented {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "not implemented yet; see plans/towards-0.2.0.md for the milestone that lands it",
        )
    }
}

impl std::error::Error for NotYetImplemented {}
