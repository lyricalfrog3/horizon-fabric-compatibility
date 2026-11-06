//! Time a cold read of registered block facts and shapes.
//!
//! ```sh
//! cargo run --release -p hfc-runtime --example block_profile -- <mod jars...>
//! ```
//!
//! Loads the jars in order (put Fabric API first) on the machine's Minecraft,
//! runs `init` (which reads the facts) and then reads every block's shapes,
//! as a first start without `.horizon-cache/fabric-blocks` does. With
//! `RUST_LOG=debug` it prints one line per block and phase.

use std::time::Instant;

struct Stderr(log::LevelFilter);

impl log::Log for Stderr {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.0
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) && record.target().starts_with("hfc_runtime") {
            eprintln!("{:5} {}", record.level(), record.args());
        }
    }
    fn flush(&self) {}
}

fn main() {
    let level = match std::env::var("RUST_LOG").as_deref() {
        Ok("debug") => log::LevelFilter::Debug,
        Ok("trace") => log::LevelFilter::Trace,
        _ => log::LevelFilter::Info,
    };
    static LOGGER: std::sync::OnceLock<Stderr> = std::sync::OnceLock::new();
    log::set_logger(LOGGER.get_or_init(|| Stderr(level))).unwrap();
    log::set_max_level(level);
    let Some(mut bridge) = hfc_runtime::discovery::bridge_with_vanilla() else {
        eprintln!("no Minecraft installation found");
        std::process::exit(1);
    };
    for jar in std::env::args().skip(1) {
        bridge.load_mod_jar(std::path::PathBuf::from(jar)).unwrap();
    }
    let started = Instant::now();
    bridge.init().unwrap();
    let init = started.elapsed();
    let blocks = bridge.registered_blocks();
    let states: usize = blocks.iter().map(|b| b.states.len()).sum();
    let started = Instant::now();
    bridge.read_block_shapes(&[]);
    let shapes = started.elapsed();
    let blocks = bridge.registered_blocks();
    let missing: usize = blocks
        .iter()
        .flat_map(|b| b.states.values())
        .filter(|s| s.outline.is_none() || s.collision.is_none())
        .count();
    let unreadable: usize = blocks.iter().map(|b| b.unreadable.len()).sum();
    println!(
        "{} blocks, {states} states; init (with facts) {init:.1?}, shapes {shapes:.1?}; \
         {missing} states without a shape, {unreadable} unreadable lines",
        blocks.len()
    );
}
