//! `hfc`: install, scan, bootstrap and inspect Horizon Fabric Compatibility.
//!
//! The launcher only needs `hfc bootstrap --mods <mods folder>`; the other
//! commands exist for people who want to see what HFC found without starting
//! the game.

use hfc_bootstrap::{Bootstrap, Config, RunOptions};
use hfc_core::Limits;
use hfc_fabric::{EnvironmentInfo, discover, resolve, scan_path};
use std::{env, path::PathBuf, process::ExitCode};

const HELP: &str = "\
Horizon Fabric Compatibility

Usage: hfc <command> [options]

Commands:
  install    Create horizon-mods/HorizonFabricCompat/hfc.toml
  bootstrap  Scan, convert, install and report (what the launcher runs)
  scan       Analyze jars and print findings; writes nothing
  status     Show what the last bootstrap installed
  prune      Remove cache entries for jars no longer present

Options:
  --mods <dir>     The game's horizon-mods folder (default: ./horizon-mods)
  --list <file>    horizon-mods.txt location (default: beside the mods folder)
  --dry-run        Analyze and report, install nothing
  --force          Ignore the conversion cache
  --force-install  Overwrite an existing hfc.toml on `install`
  --json           Machine-readable output for `scan`
  --strict         Exit non-zero when any finding is an error

Every switch above also takes an explicit value (`--dry-run=false`,
`--json=true`); an invalid value is an error, not a silent enable.
  -h, --help       This help
  -V, --version    hfc's version

Nothing is written outside the mods folder, the compatibility folder and
horizon-mods.txt. Original jars are never modified.";

#[derive(Debug)]
struct Args {
    command: String,
    mods: PathBuf,
    list: Option<PathBuf>,
    dry_run: bool,
    force: bool,
    force_install: bool,
    json: bool,
    strict: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("hfc: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> std::result::Result<ExitCode, String> {
    let args = match parse_args()? {
        Some(args) => args,
        None => return Ok(ExitCode::SUCCESS),
    };
    match args.command.as_str() {
        "install" => install(&args),
        "bootstrap" => bootstrap(&args),
        "scan" => scan(&args),
        "status" => status(&args),
        "prune" => prune(&args),
        other => Err(format!("unknown command {other:?}; try --help")),
    }
}

fn parse_args() -> std::result::Result<Option<Args>, String> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        println!("{HELP}");
        return Ok(None);
    };
    parse_command(&command, args.collect())
}

/// Parse everything after the command word. Split out of [`parse_args`] so the
/// flag rules are testable without spawning the binary.
fn parse_command(command: &str, rest: Vec<String>) -> std::result::Result<Option<Args>, String> {
    if matches!(command, "-h" | "--help" | "help") {
        println!("{HELP}");
        return Ok(None);
    }
    if matches!(command, "-V" | "--version") {
        println!("hfc {}", hfc_core::HFC_VERSION);
        return Ok(None);
    }
    let mut parsed = Args {
        command: command.to_owned(),
        mods: PathBuf::from("horizon-mods"),
        list: None,
        dry_run: false,
        force: false,
        force_install: false,
        json: false,
        strict: false,
    };
    let mut args = rest.into_iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        match flag {
            "--mods" => {
                parsed.mods = PathBuf::from(flag_value("--mods", inline, &mut args)?);
            }
            "--list" => {
                parsed.list = Some(PathBuf::from(flag_value("--list", inline, &mut args)?));
            }
            // A plain switch is true; `--flag=true`/`--flag=false` is honoured
            // rather than ignored. Anything else is an error, never a silent
            // enable.
            "--dry-run" => parsed.dry_run = bool_value(flag, inline)?,
            "--force" => parsed.force = bool_value(flag, inline)?,
            "--force-install" => parsed.force_install = bool_value(flag, inline)?,
            "--json" => parsed.json = bool_value(flag, inline)?,
            "--strict" => parsed.strict = bool_value(flag, inline)?,
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(None);
            }
            other => return Err(format!("unknown option {other:?}; try --help")),
        }
    }
    Ok(Some(parsed))
}

/// The value of an option that always needs one, inline (`--mods=dir`) or as
/// the next argument.
fn flag_value(
    name: &str,
    inline: Option<String>,
    args: &mut impl Iterator<Item = String>,
) -> std::result::Result<String, String> {
    if let Some(value) = inline {
        return Ok(value);
    }
    args.next().ok_or_else(|| format!("{name} needs a value"))
}

/// The value of a boolean switch. Absent means enabled; an explicit value must
/// parse as a boolean, so `--dry-run=maybe` is an error instead of an enable.
fn bool_value(name: &str, inline: Option<String>) -> std::result::Result<bool, String> {
    match inline {
        None => Ok(true),
        Some(value) => match value.as_str() {
            "true" | "1" | "yes" | "on" => Ok(true),
            "false" | "0" | "no" | "off" => Ok(false),
            other => Err(format!(
                "{name} takes true or false, not {other:?}; write {name}=false to disable it"
            )),
        },
    }
}

fn install(args: &Args) -> std::result::Result<ExitCode, String> {
    if !args.mods.is_dir() {
        return Err(format!(
            "{} is not a folder; point --mods at the game's horizon-mods folder",
            args.mods.display()
        ));
    }
    let compat = hfc_bootstrap::config::compat_dir(&args.mods);
    let marker = compat.join(hfc_bootstrap::CONFIG_FILE);
    if marker.exists() && !args.force_install {
        println!(
            "HFC is already installed at {} (use --force-install to rewrite hfc.toml)",
            compat.display()
        );
        return Ok(ExitCode::SUCCESS);
    }
    std::fs::create_dir_all(&compat).map_err(|e| format!("{}: {e}", compat.display()))?;
    hfc_core::atomic_write(&marker, Config::template().as_bytes()).map_err(|e| e.to_string())?;
    println!("Installed HFC in {}", compat.display());
    println!(
        "Drop Fabric .jar files in {} and launch Horizon.",
        args.mods.display()
    );
    Ok(ExitCode::SUCCESS)
}

fn bootstrap(args: &Args) -> std::result::Result<ExitCode, String> {
    let Some(bootstrap) = Bootstrap::open(&args.mods).map_err(|e| e.to_string())? else {
        println!(
            "No HFC installation at {} (no hfc.toml); nothing to do.",
            hfc_bootstrap::config::compat_dir(&args.mods).display()
        );
        return Ok(ExitCode::SUCCESS);
    };
    let bootstrap = match &args.list {
        Some(list) => bootstrap.with_mod_list(list),
        None => bootstrap,
    };
    let report = bootstrap
        .run(RunOptions {
            dry_run: args.dry_run,
            force: args.force,
            ..RunOptions::default()
        })
        .map_err(|e| e.to_string())?;
    println!(
        "HFC {}: {} jar(s), {} installed, {} reused, {} not portable, {} blocked, {} failed",
        hfc_core::HFC_VERSION,
        report.summary.jars_found,
        report.summary.installed,
        report.summary.reused,
        report.summary.not_portable,
        report.summary.blocked,
        report.summary.failed
    );
    if report.summary.errors > 0 || report.summary.warnings > 0 {
        println!(
            "{} error(s), {} warning(s): {}",
            report.summary.errors,
            report.summary.warnings,
            hfc_core::REPORT_MD
        );
    }
    println!(
        "Report: {}",
        bootstrap.compat_dir().join(hfc_core::REPORT_MD).display()
    );
    if args.strict && report.summary.errors > 0 {
        return Ok(ExitCode::from(2));
    }
    Ok(ExitCode::SUCCESS)
}

fn scan(args: &Args) -> std::result::Result<ExitCode, String> {
    let jars = discover(&args.mods).map_err(|e| e.to_string())?;
    let mut scans = Vec::new();
    for jar in jars {
        match scan_path(&jar.path, &Limits::default()) {
            Ok(scan) => scans.push(scan),
            Err(error) => eprintln!("{}: {error}", jar.file_name),
        }
    }
    let environment = match Bootstrap::open(&args.mods).map_err(|e| e.to_string())? {
        Some(bootstrap) => EnvironmentInfo {
            minecraft: bootstrap.config().compat.minecraft.clone(),
            fabric_loader: bootstrap.config().compat.fabric_loader.clone(),
            java: bootstrap.config().compat.java.clone(),
        },
        None => EnvironmentInfo::default(),
    };
    let resolution = resolve(&scans, &environment).map_err(|e| e.to_string())?;
    if args.json {
        let text = serde_json::to_string_pretty(&resolution).map_err(|e| e.to_string())?;
        println!("{text}");
    } else {
        println!(
            "{} jar(s), {} Fabric mod(s), {} installable in dependency order",
            scans.len(),
            resolution.mods.len(),
            resolution.order.len()
        );
        for node in &resolution.mods {
            println!(
                "  {:<24} {:<12} {:<12} {}",
                node.id,
                node.version,
                node.status(),
                node.origin.file_name()
            );
        }
        if !resolution.order.is_empty() {
            println!("Order: {}", resolution.order.join(" -> "));
        }
        for issue in &resolution.issues {
            let level = match issue.severity {
                hfc_fabric::Severity::Error => "error",
                hfc_fabric::Severity::Warning => "warning",
                hfc_fabric::Severity::Info => "info",
            };
            println!("  {level}: {}", issue.message);
        }
        for skipped in &resolution.skipped {
            println!("  skipped {}: {}", skipped.file_name, skipped.reason);
        }
    }
    if args.strict && resolution.has_errors() {
        return Ok(ExitCode::from(2));
    }
    Ok(ExitCode::SUCCESS)
}

fn status(args: &Args) -> std::result::Result<ExitCode, String> {
    let compat = hfc_bootstrap::config::compat_dir(&args.mods);
    let report_path = compat.join(hfc_core::REPORT_JSON);
    if !report_path.is_file() {
        println!(
            "No HFC report at {}; run `hfc bootstrap`.",
            report_path.display()
        );
        return Ok(ExitCode::SUCCESS);
    }
    let report = hfc_bootstrap::Report::read(&report_path).map_err(|e| e.to_string())?;
    println!(
        "HFC {} for Horizon {} · {} jar(s) · {} installed · {} reused · {} blocked",
        report.hfc_version,
        report.horizon_version,
        report.summary.jars_found,
        report.summary.installed,
        report.summary.reused,
        report.summary.blocked
    );
    for m in &report.mods {
        println!(
            "  {:<24} {:<12} {:<12} {}",
            m.id.as_deref().unwrap_or(&m.file_name),
            m.version,
            m.status.label(),
            m.verdict.clone().unwrap_or_default()
        );
    }
    let state_path = compat.join(hfc_core::STATE);
    let (state, note) = hfc_bootstrap::State::load(&state_path);
    for note in note.into_iter() {
        println!("note: {note}");
    }
    println!(
        "{} generated mod(s) recorded in state.json",
        state.installed.len()
    );
    let cache = hfc_cache::Cache::new(compat.join("cache"));
    if let Ok(stats) = cache.stats() {
        println!(
            "cache: {} entr{}, {}",
            stats.entries,
            if stats.entries == 1 { "y" } else { "ies" },
            human_bytes(stats.bytes)
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn prune(args: &Args) -> std::result::Result<ExitCode, String> {
    let compat = hfc_bootstrap::config::compat_dir(&args.mods);
    let cache_dir = compat.join("cache");
    if !cache_dir.is_dir() {
        println!(
            "No cache directory at {}; nothing to prune.",
            cache_dir.display()
        );
        return Ok(ExitCode::SUCCESS);
    }
    let mut keep = std::collections::BTreeSet::new();
    if let Ok(jars) = discover(&args.mods) {
        for jar in jars {
            if let Ok(sha) = hfc_core::sha256_file(&jar.path) {
                keep.insert(sha);
            }
        }
    }
    let state_path = compat.join(hfc_core::STATE);
    let (state, _) = hfc_bootstrap::State::load(&state_path);
    for installed in state.installed {
        if !installed.source_sha256.is_empty() {
            keep.insert(installed.source_sha256);
        }
    }
    let cache = hfc_cache::Cache::new(&cache_dir);
    let report = cache.prune(&keep).map_err(|e| e.to_string())?;
    println!(
        "Pruned {} entry(ies), reclaimed {}.",
        report.pruned_entries,
        human_bytes(report.reclaimed_bytes)
    );
    if let Ok(stats) = cache.stats() {
        println!(
            "Remaining cache: {} entr{}, {}",
            stats.entries,
            if stats.entries == 1 { "y" } else { "ies" },
            human_bytes(stats.bytes)
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> std::result::Result<Args, String> {
        let mut all = arguments.iter().map(|a| (*a).to_owned());
        let command = all.next().expect("a command");
        match parse_command(&command, all.collect()) {
            Ok(Some(args)) => Ok(args),
            Ok(None) => Err("help or version was printed".into()),
            Err(error) => Err(error),
        }
    }

    #[test]
    fn a_bare_boolean_flag_enables_it() {
        let args = parse(&["scan", "--json", "--strict"]).unwrap();
        assert!(args.json);
        assert!(args.strict);
        assert!(!args.dry_run);
        assert!(!args.force);
    }

    #[test]
    fn an_inline_false_disables_a_boolean_flag() {
        let args = parse(&["bootstrap", "--dry-run=false", "--json=false"]).unwrap();
        assert!(!args.dry_run, "--dry-run=false must not enable the flag");
        assert!(!args.json, "--json=false must not enable the flag");

        let args = parse(&[
            "bootstrap",
            "--dry-run=true",
            "--force=yes",
            "--json=off",
            "--strict=1",
        ])
        .unwrap();
        assert!(args.dry_run);
        assert!(args.force);
        assert!(!args.json);
        assert!(args.strict);

        let args = parse(&["bootstrap", "--dry-run=true", "--dry-run=false"]).unwrap();
        assert!(!args.dry_run, "the last value of a repeated flag wins");
    }

    #[test]
    fn an_invalid_boolean_value_is_an_error() {
        for (flag, value) in [
            ("--dry-run", "maybe"),
            ("--json", "2"),
            ("--force", ""),
            ("--strict", "False"),
        ] {
            let argument = format!("{flag}={value}");
            let error = parse(&["bootstrap", &argument]).unwrap_err();
            assert!(
                error.contains("takes true or false") && error.contains(flag),
                "{argument}: {error}"
            );
        }
    }

    #[test]
    fn options_with_values_still_work() {
        let args = parse(&["bootstrap", "--mods", "/tmp/mods", "--list=/tmp/list.txt"]).unwrap();
        assert_eq!(args.mods, PathBuf::from("/tmp/mods"));
        assert_eq!(args.list, Some(PathBuf::from("/tmp/list.txt")));
        assert!(parse(&["bootstrap", "--mods"]).is_err());
        assert!(parse(&["bootstrap", "--mods", "/tmp/mods", "--nope"]).is_err());
    }

    #[test]
    fn an_unknown_boolean_notation_is_not_silently_accepted() {
        // `--dry-run` followed by a value is not the same as `--dry-run=false`:
        // the bare flag is enabled and the stray word is reported.
        assert!(parse(&["bootstrap", "--dry-run", "false"]).is_err());
    }
}
