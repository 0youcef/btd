mod aggregator;
mod model;
mod mounts;
mod sampler;
mod ui;

use aggregator::SharedState;
use anyhow::{bail, Context, Result};
use btrfs_uapi::chunk::chunk_list;
use btrfs_uapi::device::device_info_all;
use btrfs_uapi::filesystem::filesystem_info;
use btrfs_uapi::space::BlockGroupFlags;
use crossbeam_channel::unbounded;
use model::SampledBlockGroup;
use sampler::SamplerConfig;
use std::collections::HashSet;
use std::fs::File;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

struct Args {

    path: PathBuf,

    workers: Option<usize>,

    cpu_percent: f64,
}

fn parse_args() -> Result<Args> {
    let mut path: Option<PathBuf> = None;
    let mut workers: Option<usize> = None;
    let mut cpu_percent: f64 = 50.0;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-w" | "--workers" => {
                let v = it.next().context("--workers needs a value")?;
                workers = Some(v.parse().context("--workers must be a number")?);
            }
            "-c" | "--cpu" => {
                let v = it.next().context("--cpu needs a value")?;
                cpu_percent = v.parse().context("--cpu must be a number (percent)")?;
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: btd [-w/--workers N] [-c/--cpu PERCENT_PER_THREAD] <path>"
                );
                std::process::exit(0);
            }
            other if path.is_none() => path = Some(PathBuf::from(other)),
            other => bail!("unexpected argument: {other}"),
        }
    }

    Ok(Args {
        path: path.context("usage: btd [-w/--workers N] [-c/--cpu PERCENT_PER_THREAD] <path>")?,
        workers,
        cpu_percent,
    })
}

fn main() -> Result<()> {
    let args = parse_args()?;

    let file =
        File::open(&args.path).with_context(|| format!("failed to open {:?}", args.path))?;
    let fd = file.as_fd();

    let fs_info = filesystem_info(fd).context(
        "BTRFS_IOC_FS_INFO failed — is this path on a btrfs filesystem, and are you root?",
    )?;

    let devices = device_info_all(fd, &fs_info).context("BTRFS_IOC_DEV_INFO failed")?;
    eprintln!("Filesystem UUID {}, {} device(s):", fs_info.uuid, devices.len());
    for d in &devices {
        eprintln!(
            "  devid {:>3}  {:>12}  {}",
            d.devid,
            human_bytes(d.total_bytes as f64),
            d.path
        );
    }

    let chunks = chunk_list(fd).context("chunk tree search failed (needs CAP_SYS_ADMIN)")?;
    let mut seen_logical = HashSet::new();
    let mut block_groups = Vec::new();
    for c in chunks {
        if !seen_logical.insert(c.logical_start) {
            continue;
        }
        block_groups.push(SampledBlockGroup {
            logical_start: c.logical_start,
            length: c.length,
            is_data: c.flags.contains(BlockGroupFlags::DATA),
            is_metadata: c.flags.contains(BlockGroupFlags::METADATA),
            is_system: c.flags.contains(BlockGroupFlags::SYSTEM),

            phys_devid: Some(c.devid),
            phys_start: Some(c.physical_start),
        });
    }

    if block_groups.is_empty() {
        bail!("no allocated block groups found — nothing to sample");
    }

    let universe_bytes: u64 = block_groups.iter().map(|bg| bg.length).sum();
    let mut cumulative = Vec::with_capacity(block_groups.len());
    let mut running = 0u64;
    for bg in &block_groups {
        running += bg.length;
        cumulative.push(running);
    }
    let total_weight = running;

    let mountpoint_path = if args.path.is_dir() {
        args.path.clone()
    } else {
        args.path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let device_sizes: Vec<(u64, String, u64)> = devices
        .iter()
        .map(|d| (d.devid, d.path.clone(), d.total_bytes))
        .collect();
    let state = Arc::new(SharedState::new(universe_bytes, device_sizes));
    let (tx, rx) = unbounded();

    let mounted_subvols = Arc::new(mounts::discover_mounted_subvolumes(fs_info.uuid));
    eprintln!(
        "Resolved {} separately-mounted subvolume(s) via /proc/self/mountinfo",
        mounted_subvols.len()
    );

    {
        let state = Arc::clone(&state);
        std::thread::spawn(move || aggregator::run(state, rx));
    }

    let mountpoint_fd = Arc::new(file);
    let block_groups = Arc::new(block_groups);
    let cumulative = Arc::new(cumulative);

    let n_workers = args.workers.unwrap_or(2);

    let duty_cycle = (args.cpu_percent / 100.0).clamp(0.01, 1.0);

    let mut handles = Vec::with_capacity(n_workers);
    for _ in 0..n_workers {
        let config = SamplerConfig {
            mountpoint_fd: Arc::clone(&mountpoint_fd),
            mountpoint_path: mountpoint_path.clone(),
            block_groups: Arc::clone(&block_groups),
            cumulative: Arc::clone(&cumulative),
            total_weight,
            duty_cycle,
            mounted_subvols: Arc::clone(&mounted_subvols),
        };
        let tx = tx.clone();
        let shutdown = Arc::clone(&shutdown);
        let paused = Arc::clone(&paused);
        handles.push(std::thread::spawn(move || {
            sampler::run(config, tx, shutdown, paused)
        }));
    }
    drop(tx);

    eprintln!(
        "{n_workers} worker thread(s), each capped to ~{:.0}% CPU (duty-cycle throttled, not rate-targeted)",
        args.cpu_percent
    );

    let ui_result = ui::run(state, Arc::clone(&shutdown), paused);

    shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }

    ui_result
}

fn human_bytes(bytes: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut val = bytes;
    let mut unit = 0;
    while val >= 1024.0 && unit < UNITS.len() - 1 {
        val /= 1024.0;
        unit += 1;
    }
    format!("{val:.1}{}", UNITS[unit])
}
