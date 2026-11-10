//! `omascreencastd` — the 0.2.0 GameStream host.
//!
//! Only the subcommand surface and the `identity` command are implemented at
//! M1-T3. `serve`, `pair`, and `doctor` arrive with M1-T5..T9; they are present
//! as commands now so the operator-facing vocabulary is fixed early (D13: a
//! single binary with subcommands, not a second CLI).

#![deny(missing_docs)]

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use gs_crypto::{Identity, TrustStore, identity_paths, paths_under};
use tracing_subscriber::EnvFilter;

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
    /// Run the HTTPS, RTSP, and mDNS listeners (M1-T5..T9).
    Serve,
    /// Show or approve a pending pairing request (M1-T7).
    Pair,
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

    // The command is taken by value and everything else is read through `&Cli`,
    // so a subcommand can carry owned arguments without borrowing the whole
    // parsed struct.
    let result = match &cli.command {
        Command::Identity => identity(&cli),
        Command::Clients => clients(&cli),
        Command::Unpair { id } => unpair(&cli, id),
        // Not yet implemented; keep the failure explicit rather than silently
        // exiting 0, so a half-built daemon is never mistaken for a working one.
        Command::Serve | Command::Pair | Command::Doctor => {
            Err(Box::new(NotYetImplemented) as Box<dyn std::error::Error>)
        }
    };

    if let Err(error) = result {
        eprintln!("omascreencastd: {error}");
        std::process::exit(1);
    }
}

fn identity(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let paths = match &cli.data_dir {
        Some(dir) => paths_under(dir),
        None => identity_paths()?,
    };
    let common_name = hostname();
    let identity = Identity::load_or_generate(&paths, &common_name)?;
    println!("identity: {}", paths.cert.display());
    println!("key:      {}", paths.key.display());
    println!("subject:  {common_name}");
    println!("bits:     {}", gs_crypto::identity::KEY_BITS);
    println!("sha256:   {}", identity.fingerprint_hex());
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
        .unwrap_or_else(|| String::from("omascreencast"))
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
