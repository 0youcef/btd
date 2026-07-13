![Demo](demo.gif)
# btd

A statistical disk usage analyzer for btrfs, inspired by [btdu](https://github.com/CyberShadow/btdu). Instead of walking every extent, it samples random addresses across the filesystem's allocated space and resolves each one back to a path via `LOGICAL_INO`/`INO_PATHS`. Given enough samples this converges to accurate size estimates without the I/O cost of a full scan, and you get usable numbers within seconds instead of waiting for a complete walk.

## Why

`btrfs filesystem du` and friends do exhaustive backref walks. That's accurate but slow on large or heavily-snapshotted filesystems. Sampling trades a small amount of precision for something you can actually run interactively on a multi-terabyte volume with thousands of snapshots.

## Building

```
cargo build --release
```

Needs a reasonably current Rust toolchain (edition 2021, some dependencies want rustc 1.8x+). Nothing unusual otherwise.

## Running

```
sudo ./target/release/btd /mnt/wherever
```

Root (or `CAP_SYS_ADMIN`) is required — everything here goes through btrfs ioctls that the kernel restricts. `/mnt/wherever` can be the filesystem's top-level mountpoint or any subdirectory on it.

Flags:

- `-w, --workers N` — number of sampler threads. Defaults to 2. More workers means faster convergence but more concurrent ioctl load; on filesystems with a lot of snapshot/reflink sharing, `LOGICAL_INO` can be genuinely expensive per call, so don't just crank this up blindly.
- `-c, --cpu PERCENT` — target CPU duty cycle per worker thread, default 50. Each thread paces itself to spend roughly this fraction of a core actively sampling, regardless of how slow any individual ioctl turns out to be.

## Using it

Arrow keys / `j`/`k` move the selection, `Enter`/`l`/right descend into a directory, `Backspace`/`h`/left go back up. `m` toggles the physical disk map. `p` pauses sampling. `q` quits.

Sizes are live estimates — they get more accurate the longer it runs. The `n=` figure next to each entry is the raw sample count backing that estimate; treat anything with a low count as rough. The header shows a running total and a crude confidence estimate.

Rows are colored by category: green is data exclusively owned by that path, yellow is shared (reflinked or shared with a snapshot), cyan is btrfs metadata, magenta is SYSTEM chunks, gray is unallocated space inside an otherwise-allocated block group. The bar next to each entry is a stacked composition of these, so you can tell at a glance whether a directory's size is "real" exclusive data or mostly shared-with-something-else.

## The disk map

`m` switches to a physical layout view — one row per device, showing where on the actual disk the sampled data lives, not just which file it belongs to. Useful for understanding RAID/multi-device layouts or just seeing whether your data is fragmented across the device. Density shading (light to solid) reflects how many samples have landed in that region so far; it fills in the longer you leave it running.

## Known limitations

- Physical disk map only shows one stripe copy for RAID1/DUP profiles, not every replica.
- No `--compare` baseline diffing yet (compare two runs over time to see what grew).
- Category model is exclusive/shared/metadata/system/unallocated — no separate "distributed" size (fair-share attribution across every subvolume that references a block) yet.
- Confidence numbers in the header are a rough 1/√n approximation, not a proper Wilson score interval.
