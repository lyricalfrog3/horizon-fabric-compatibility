//! Compatibility census over a folder of Fabric mods.
//!
//! ```sh
//! cargo run --release -p hfc-runtime --example census -- \
//!     --mods test-mods/corpus --out /tmp/census [--resources <prepared resources>] \
//!     [--compare /tmp/census-old/census.json] [--alone | --together]
//! ```
//!
//! It measures what the runtime lacks, by how many mods hit it. Zero missing
//! symbols means init hit nothing the runtime lacks; it does not mean the mod
//! works.

use hfc_runtime::census::{self, Options};
use std::path::PathBuf;

fn main() {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| -> PathBuf {
            args.next()
                .unwrap_or_else(|| {
                    eprintln!("{name} needs a path");
                    std::process::exit(2);
                })
                .into()
        };
        match arg.as_str() {
            "--mods" => options.mods = value("--mods"),
            "--out" => options.out = Some(value("--out")),
            "--resources" => options.resources = Some(value("--resources")),
            "--compare" => options.compare = Some(value("--compare")),
            "--alone" => options.alone = true,
            "--together" => options.alone = false,
            "-h" | "--help" => {
                println!(
                    "usage: census --mods <folder> [--out <dir>] [--resources <dir>] \
                     [--compare <old census.json>] [--alone | --together]"
                );
                return;
            }
            other => {
                eprintln!("unknown argument {other}; see --help");
                std::process::exit(2);
            }
        }
    }
    match census::run(&options) {
        Ok(written) => {
            println!("{} mods", written.report.mods.len());
            println!("wrote {}", written.json.display());
            println!("wrote {}", written.markdown.display());
        }
        Err(error) => {
            eprintln!("census: {error}");
            std::process::exit(1);
        }
    }
}
