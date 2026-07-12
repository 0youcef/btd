use btrfs_uapi::filesystem::filesystem_info;
use btrfs_uapi::inode::lookup_path_rootid;
use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

pub struct MountedSubvol {
    pub path: PathBuf,
    pub fd: Arc<File>,
}

pub fn discover_mounted_subvolumes(target_uuid: Uuid) -> HashMap<u64, MountedSubvol> {
    let mut result = HashMap::new();

    let entries = match parse_mountinfo() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("warning: couldn't read /proc/self/mountinfo ({e}); subvolume path resolution will be best-effort only");
            return result;
        }
    };

    for (mountpoint, fstype) in entries {
        if fstype != "btrfs" {
            continue;
        }

        let file = match File::open(&mountpoint) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let fd = file.as_fd();

        let info = match filesystem_info(fd) {
            Ok(i) => i,
            Err(_) => continue,
        };
        if info.uuid != target_uuid {
            continue;
        }

        let subvol_id = match lookup_path_rootid(fd) {
            Ok(id) => id,
            Err(_) => continue,
        };

        result
            .entry(subvol_id)
            .or_insert_with(|| MountedSubvol {
                path: mountpoint.clone(),
                fd: Arc::new(file),
            });
    }

    result
}

fn parse_mountinfo() -> std::io::Result<Vec<(PathBuf, String)>> {
    let contents = std::fs::read_to_string("/proc/self/mountinfo")?;
    let mut out = Vec::new();

    for line in contents.lines() {

        let fields: Vec<&str> = line.split(' ').collect();
        let Some(dash_idx) = fields.iter().position(|&f| f == "-") else {
            continue;
        };
        if fields.len() < 5 || dash_idx + 1 >= fields.len() {
            continue;
        }

        let mount_point = unescape_octal(fields[4]);
        let fstype = fields[dash_idx + 1].to_string();
        out.push((PathBuf::from(mount_point), fstype));
    }

    Ok(out)
}

fn unescape_octal(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            if let Ok(val) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(val as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}
