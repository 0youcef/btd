use crate::model::{DeviceMap, Sample, TreeNode};
use crossbeam_channel::Receiver;
use std::sync::{Arc, Mutex};

pub const MAP_CELLS_PER_DEVICE: usize = 512;

pub struct SharedState {
    pub tree: Mutex<TreeNode>,
    pub total_draws: std::sync::atomic::AtomicU64,
    pub universe_bytes: u64,

    pub disk_map: Mutex<Vec<DeviceMap>>,
}

impl SharedState {
    pub fn new(universe_bytes: u64, devices: Vec<(u64, String, u64)>) -> Self {
        let disk_map = devices
            .into_iter()
            .map(|(devid, path, total_bytes)| {
                DeviceMap::new(devid, path, total_bytes, MAP_CELLS_PER_DEVICE)
            })
            .collect();

        Self {
            tree: Mutex::new(TreeNode::default()),
            total_draws: std::sync::atomic::AtomicU64::new(0),
            universe_bytes,
            disk_map: Mutex::new(disk_map),
        }
    }
}

pub struct DrawResult {
    pub samples: Vec<Sample>,
}

pub fn run(state: Arc<SharedState>, rx: Receiver<DrawResult>) {
    while let Ok(draw) = rx.recv() {
        state
            .total_draws
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if draw.samples.is_empty() {
            continue;
        }

        {
            let mut tree = state.tree.lock().unwrap();
            for sample in &draw.samples {
                insert(&mut tree, sample);
            }
        }
        {
            let mut map = state.disk_map.lock().unwrap();
            for sample in &draw.samples {
                if let Some((devid, offset)) = sample.physical {
                    if let Some(dev) = map.iter_mut().find(|d| d.devid == devid) {
                        let idx = dev.cell_index(offset);
                        let cell = &mut dev.cells[idx];
                        *cell.hits.entry(sample.category).or_insert(0.0) += sample.weight;
                        cell.count += 1;
                    }
                }
            }
        }
    }
}

fn insert(root: &mut TreeNode, sample: &Sample) {
    let mut node = root;
    apply(node, sample);

    for component in &sample.path_components {
        node = node
            .children
            .entry(component.clone())
            .or_insert_with(TreeNode::default);
        apply(node, sample);
    }
}

fn apply(node: &mut TreeNode, sample: &Sample) {
    *node.hits.entry(sample.category).or_insert(0.0) += sample.weight;
    *node.counts.entry(sample.category).or_insert(0) += 1;
    if let Some(inode) = sample.inode {
        node.inode = Some(inode);
    }
}
