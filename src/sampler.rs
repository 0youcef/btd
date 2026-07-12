use crate::aggregator::DrawResult;
use crate::model::{Category, Sample, SampledBlockGroup};
use crate::mounts::MountedSubvol;
use btrfs_uapi::inode::{ino_paths, logical_ino, subvolid_resolve};
use crossbeam_channel::Sender;
use rand::Rng;
use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct SamplerConfig {
    pub mountpoint_fd: Arc<File>,
    pub mountpoint_path: PathBuf,
    pub block_groups: Arc<Vec<SampledBlockGroup>>,

    pub cumulative: Arc<Vec<u64>>,
    pub total_weight: u64,

    pub duty_cycle: f64,

    pub mounted_subvols: Arc<HashMap<u64, MountedSubvol>>,
}

struct SubvolEntry {
    display_name: String,

    dir_fd: Option<Arc<File>>,
}

type SubvolCache = HashMap<u64, SubvolEntry>;

pub fn run(
    config: SamplerConfig,
    tx: Sender<DrawResult>,
    shutdown: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
) {
    let mut rng = rand::thread_rng();
    let mut subvol_cache: SubvolCache = HashMap::new();

    let duty_cycle = config.duty_cycle.clamp(0.01, 1.0);
    let sleep_multiplier = (1.0 - duty_cycle) / duty_cycle;

    while !shutdown.load(Ordering::Relaxed) {
        if paused.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        let draw_start = Instant::now();

        let Some(bg) = pick_block_group(&config, &mut rng) else {
            break;
        };

        let logical = if bg.length == 0 {
            bg.logical_start
        } else {
            bg.logical_start + rng.gen_range(0..bg.length)
        };

        let samples = resolve_draw(&config, bg, logical, &mut subvol_cache);

        if tx.send(DrawResult { samples }).is_err() {
            break;
        }

        if sleep_multiplier > 0.0 {
            let elapsed = draw_start.elapsed();
            let sleep_for = elapsed.mul_f64(sleep_multiplier);
            if sleep_for > Duration::ZERO {
                std::thread::sleep(sleep_for);
            }
        }
    }
}

fn pick_block_group<'a>(
    config: &'a SamplerConfig,
    rng: &mut impl Rng,
) -> Option<&'a SampledBlockGroup> {
    if config.total_weight == 0 {
        return None;
    }
    let target = rng.gen_range(0..config.total_weight);
    let idx = config.cumulative.partition_point(|&c| c <= target);
    config.block_groups.get(idx)
}

fn resolve_draw(
    config: &SamplerConfig,
    bg: &SampledBlockGroup,
    logical: u64,
    subvol_cache: &mut SubvolCache,
) -> Vec<Sample> {

    let physical = match (bg.phys_devid, bg.phys_start) {
        (Some(devid), Some(start)) => Some((devid, start + (logical - bg.logical_start))),
        _ => None,
    };

    if bg.is_metadata {
        return vec![Sample {
            path_components: vec!["<Metadata>".to_string()],
            category: Category::Metadata,
            weight: 1.0,
            inode: None,
            physical,
        }];
    }
    if bg.is_system {
        return vec![Sample {
            path_components: vec!["<System>".to_string()],
            category: Category::System,
            weight: 1.0,
            inode: None,
            physical,
        }];
    }

    let top_fd = config.mountpoint_fd.as_fd();
    let results = match logical_ino(top_fd, logical, true, None) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    if results.is_empty() {
        return vec![Sample {
            path_components: vec!["<Unallocated>".to_string()],
            category: Category::Unallocated,
            weight: 1.0,
            inode: None,
            physical,
        }];
    }

    let n = results.len();
    let category = if n > 1 {
        Category::Shared
    } else {
        Category::Exclusive
    };
    let per_ref_weight = 1.0 / n as f64;

    results
        .into_iter()
        .filter_map(|r| {
            let entry = subvol_cache
                .entry(r.root)
                .or_insert_with(|| build_subvol_entry(config, r.root));

            match &entry.dir_fd {
                Some(f) => {

                    let file_path = ino_paths(f.as_fd(), r.inode)
                        .ok()
                        .and_then(|mut v| v.pop())
                        .unwrap_or_else(|| format!("<inode {}>", r.inode));

                    let mut components = vec![entry.display_name.clone()];
                    components.extend(
                        file_path
                            .split('/')
                            .filter(|s| !s.is_empty())
                            .map(String::from),
                    );

                    Some(Sample {
                        path_components: components,
                        category,
                        weight: per_ref_weight,
                        inode: Some(r.inode),
                        physical,
                    })
                }
                None => {

                    Some(Sample {
                        path_components: vec![entry.display_name.clone()],
                        category,
                        weight: per_ref_weight,
                        inode: None,
                        physical,
                    })
                }
            }
        })
        .collect()
}

fn build_subvol_entry(config: &SamplerConfig, root_id: u64) -> SubvolEntry {

    if let Some(mounted) = config.mounted_subvols.get(&root_id) {
        return SubvolEntry {
            display_name: format!("<subvol:{}>", mounted.path.display()),
            dir_fd: Some(Arc::clone(&mounted.fd)),
        };
    }

    let top_fd = config.mountpoint_fd.as_fd();
    let rel_path = match subvolid_resolve(top_fd, root_id) {
        Ok(p) => p,
        Err(_) => {
            return SubvolEntry {
                display_name: format!("<unresolved-subvol:{}>", root_id),
                dir_fd: None,
            }
        }
    };

    match File::open(config.mountpoint_path.join(&rel_path)) {
        Ok(f) => SubvolEntry {
            display_name: format!("<subvol:{}>", rel_path),
            dir_fd: Some(Arc::new(f)),
        },
        Err(_) => SubvolEntry {

            display_name: format!("<unresolved-subvol:{}>", root_id),
            dir_fd: None,
        },
    }
}
