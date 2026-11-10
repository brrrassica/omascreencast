//! Workspace helper tasks (`cargo xtask ...`).
//!
//! **Scaffold only.** Release assembly lands with M7 and fixture capture with
//! M0-T5; for now every invocation prints the workspace roadmap so the alias is
//! wired up and testable.

#![deny(missing_docs)]

use std::env;

fn main() {
    let task = env::args().nth(1);
    match task.as_deref() {
        Some("fixtures") => {
            println!("xtask fixtures: replay/regenerate the trace corpus (lands with M0-T5/M1-T11)")
        }
        Some("release") => println!(
            "xtask release: stage the prebuilt binary into the plugin folder (lands with M7-T1)"
        ),
        Some("-h" | "--help" | "help") | None => usage(),
        Some(other) => {
            eprintln!("xtask: unknown task {other:?}");
            usage();
            std::process::exit(2);
        }
    }
}

fn usage() {
    println!("usage: cargo xtask <fixtures|release>");
}
