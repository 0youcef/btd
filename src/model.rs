use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {

    Exclusive,

    Shared,

    Metadata,

    System,

    Unallocated,
}

impl Category {
    pub const ALL: [Category; 5] = [
        Category::Exclusive,
        Category::Shared,
        Category::Metadata,
        Category::System,
        Category::Unallocated,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Exclusive => "exclusive",
            Category::Shared => "shared",
            Category::Metadata => "metadata",
            Category::System => "system",
            Category::Unallocated => "unallocated",
        }
    }

    pub fn explanation(self) -> &'static str {
        match self {
            Category::Exclusive => "Data referenced by exactly one file. Deleting that file frees this space.",
            Category::Shared => "Data referenced by more than one file (reflink/dedup) or shared with a snapshot. Freed only once every referencing file/snapshot is gone.",
            Category::Metadata => "btrfs metadata (b-trees: extent tree, fs trees, csum tree, ...), not attributable to a single file.",
            Category::System => "btrfs SYSTEM chunks: bookkeeping for the chunk tree itself. Normally tiny.",
            Category::Unallocated => "A hole inside an allocated DATA block group with no current owner (e.g. after deletion, before reclaim).",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Sample {
    pub path_components: Vec<String>,
    pub category: Category,
    pub weight: f64,

    pub inode: Option<u64>,

    pub physical: Option<(u64, u64)>,
}

#[derive(Debug, Clone)]
pub struct SampledBlockGroup {
    pub logical_start: u64,
    pub length: u64,
    pub is_data: bool,
    pub is_metadata: bool,
    pub is_system: bool,

    pub phys_devid: Option<u64>,
    pub phys_start: Option<u64>,
}

#[derive(Debug, Default)]
pub struct TreeNode {
    pub children: HashMap<String, TreeNode>,
    pub hits: HashMap<Category, f64>,
    pub counts: HashMap<Category, u64>,
    pub inode: Option<u64>,
}

impl TreeNode {
    pub fn total_hits(&self) -> f64 {
        self.hits.values().sum()
    }

    pub fn total_count(&self) -> u64 {
        self.counts.values().sum()
    }

    pub fn hits_for(&self, cat: Category) -> f64 {
        *self.hits.get(&cat).unwrap_or(&0.0)
    }

    pub fn count_for(&self, cat: Category) -> u64 {
        *self.counts.get(&cat).unwrap_or(&0)
    }
}

#[derive(Clone)]
pub struct DirRow {
    pub name: String,
    pub total_bytes: f64,
    pub total_samples: u64,
    pub bytes_by_category: HashMap<Category, f64>,
    pub samples_by_category: HashMap<Category, u64>,
    pub has_children: bool,
    pub inode: Option<u64>,
}

impl DirRow {
    pub fn bytes_for(&self, cat: Category) -> f64 {
        *self.bytes_by_category.get(&cat).unwrap_or(&0.0)
    }

    pub fn samples_for(&self, cat: Category) -> u64 {
        *self.samples_by_category.get(&cat).unwrap_or(&0)
    }

    pub fn dominant_category(&self) -> Option<Category> {
        Category::ALL
            .into_iter()
            .max_by(|a, b| self.bytes_for(*a).partial_cmp(&self.bytes_for(*b)).unwrap())
            .filter(|_| self.total_bytes > 0.0)
    }
}

#[derive(Debug, Default, Clone)]
pub struct MapCell {
    pub hits: HashMap<Category, f64>,
    pub count: u64,
}

#[derive(Debug, Clone)]
pub struct DeviceMap {
    pub devid: u64,
    pub path: String,
    pub total_bytes: u64,
    pub cells: Vec<MapCell>,
}

impl DeviceMap {
    pub fn new(devid: u64, path: String, total_bytes: u64, num_cells: usize) -> Self {
        Self {
            devid,
            path,
            total_bytes,
            cells: vec![MapCell::default(); num_cells.max(1)],
        }
    }

    pub fn cell_index(&self, offset: u64) -> usize {
        if self.total_bytes == 0 {
            return 0;
        }
        let frac = offset as f64 / self.total_bytes as f64;
        ((frac * self.cells.len() as f64) as usize).min(self.cells.len() - 1)
    }
}
