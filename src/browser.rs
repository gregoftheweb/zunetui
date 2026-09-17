use std::{
    collections::HashSet,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

use tui_tree_widget::TreeItem;

pub struct LocalTree {
    roots: Vec<Directory>,
    home: PathBuf,
    mounts_root: PathBuf,
}

const MOUNTS_ROOT_ID: &str = "zunetui://mounted-drives";

struct Directory {
    path: PathBuf,
    name: String,
    children: Option<Vec<Directory>>,
    files: Vec<PathBuf>,
}

impl LocalTree {
    pub fn home() -> io::Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut home_directory = Directory::new(home.clone());
        home_directory.name = format!("Home ({})", home.display());
        home_directory.load()?;

        let mounts_root = PathBuf::from(MOUNTS_ROOT_ID);
        let mut mounted_directories = Vec::new();
        for mount in mounted_volumes()? {
            // A mount inside Home is already reachable from the Home tree. Adding
            // it again would also create duplicate TreeItem identifiers.
            if mount.starts_with(&home) {
                continue;
            }
            let mut directory = Directory::new(mount.clone());
            directory.name = format!("{} ({})", directory.name, mount.display());
            mounted_directories.push(directory);
        }
        let mounted_root = Directory {
            path: mounts_root.clone(),
            name: "Mounted Drives".to_owned(),
            children: Some(mounted_directories),
            files: Vec::new(),
        };
        Ok(Self {
            roots: vec![mounted_root, home_directory],
            home,
            mounts_root,
        })
    }

    pub fn items(&self, marked: &HashSet<PathBuf>) -> Vec<TreeItem<'static, PathBuf>> {
        self.roots.iter().map(|root| root.to_item(marked)).collect()
    }

    pub fn load(&mut self, path: &Path) {
        if let Some(directory) = self.roots.iter_mut().find_map(|root| root.find_mut(path)) {
            let _ = directory.load();
        }
    }

    pub fn refresh(&mut self, path: &Path) {
        if let Some(directory) = self.roots.iter_mut().find_map(|root| root.find_mut(path)) {
            directory.children = None;
            directory.files.clear();
            let _ = directory.load();
        }
    }

    pub fn root_path(&self) -> PathBuf {
        self.home.clone()
    }

    pub fn mounts_root_path(&self) -> PathBuf {
        self.mounts_root.clone()
    }
}

/// Return user-accessible storage mount points from Linux's per-process mount
/// table. Device-backed and common network filesystems are included; kernel
/// pseudo-filesystems (proc, sysfs, cgroups, and friends) are not useful in a
/// music browser and are deliberately omitted.
fn mounted_volumes() -> io::Result<Vec<PathBuf>> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    Ok(mounted_volumes_from(&mountinfo))
}

fn mounted_volumes_from(mountinfo: &str) -> Vec<PathBuf> {
    let mut mounts = Vec::new();
    for line in mountinfo.lines() {
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        let Some(encoded_path) = before.split_whitespace().nth(4) else {
            continue;
        };
        let mut fields = after.split_whitespace();
        let (Some(filesystem), Some(source)) = (fields.next(), fields.next()) else {
            continue;
        };
        if encoded_path == "/"
            || !(source.starts_with("/dev/") || is_network_filesystem(filesystem))
        {
            continue;
        }
        let path = PathBuf::from(unescape_mount_field(encoded_path));
        if path.is_absolute() && !mounts.contains(&path) {
            mounts.push(path);
        }
    }
    mounts.sort_by_key(|path| path.to_string_lossy().to_lowercase());
    mounts
}

fn is_network_filesystem(filesystem: &str) -> bool {
    matches!(filesystem, "cifs" | "nfs" | "nfs4" | "smb3" | "sshfs")
        || filesystem.starts_with("fuse.sshfs")
}

fn unescape_mount_field(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

pub fn album_audio_files(path: &Path) -> Option<Vec<PathBuf>> {
    let mut files: Vec<_> = fs::read_dir(path)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_type().ok()?.is_file().then(|| entry.path()))
        .filter(|path| is_audio_file(path))
        .collect();
    files.sort();
    (!files.is_empty()).then_some(files)
}

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "wav" | "mp3" | "wma" | "ogg" | "aac" | "flac" | "m4a"
            )
        })
}

impl Directory {
    fn new(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .unwrap_or_else(|| OsStr::new("/"))
            .to_string_lossy()
            .into_owned();
        Self {
            path,
            name,
            children: None,
            files: Vec::new(),
        }
    }

    fn load(&mut self) -> io::Result<()> {
        if self.children.is_some() {
            return Ok(());
        }

        let mut children = Vec::new();
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.path)?.filter_map(Result::ok) {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => children.push(Self::new(entry.path())),
                Ok(kind) if kind.is_file() => files.push(entry.path()),
                _ => {}
            }
        }
        children.sort_by_key(|directory| directory.name.to_lowercase());
        files.sort_by_key(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase()
        });
        self.children = Some(children);
        self.files = files;
        Ok(())
    }

    fn find_mut(&mut self, path: &Path) -> Option<&mut Self> {
        if self.path == path {
            return Some(self);
        }
        self.children
            .as_mut()?
            .iter_mut()
            .find_map(|child| child.find_mut(path))
    }

    fn to_item(&self, marked: &HashSet<PathBuf>) -> TreeItem<'static, PathBuf> {
        let mut children: Vec<_> = self
            .children
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|directory| directory.to_item(marked))
            .collect();
        children.extend(self.files.iter().map(|path| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let indicator = if marked.contains(path) { "[x]" } else { "[ ]" };
            TreeItem::new_leaf(path.clone(), format!("{indicator} {name}"))
        }));
        let audio_files: Vec<_> = self
            .files
            .iter()
            .filter(|path| is_audio_file(path))
            .collect();
        let label = if audio_files.is_empty() {
            self.name.clone()
        } else {
            let indicator = if audio_files.iter().all(|path| marked.contains(*path)) {
                "[x]"
            } else {
                "[ ]"
            };
            format!("{indicator} {}", self.name)
        };
        TreeItem::new(self.path.clone(), label, children)
            .expect("filesystem directory names are unique within a parent")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_audio_files_accepts_any_folder_with_audio_files() {
        let root =
            std::env::temp_dir().join(format!("zunetui-browser-test-{}", std::process::id()));
        let album = root.join("A/Artist/Album");
        fs::create_dir_all(&album).unwrap();
        fs::write(album.join("song.mp3"), b"audio").unwrap();
        fs::write(album.join("cover.jpg"), b"image").unwrap();
        assert_eq!(
            album_audio_files(&album).unwrap(),
            vec![album.join("song.mp3")]
        );

        let loose = root.join("Music/_Country/Jake Owen/American Love");
        fs::create_dir_all(&loose).unwrap();
        fs::write(loose.join("song.mp3"), b"audio").unwrap();
        assert_eq!(
            album_audio_files(&loose).unwrap(),
            vec![loose.join("song.mp3")]
        );

        let empty = root.join("Music/_Country/Jake Owen");
        fs::remove_file(loose.join("song.mp3")).unwrap();
        assert!(album_audio_files(&empty).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mounted_volumes_selects_storage_and_unescapes_paths() {
        let mountinfo = "36 25 0:32 / /proc rw - proc proc rw\n\
                         37 25 8:1 / / rw - ext4 /dev/sda1 rw\n\
                         38 25 8:17 / /run/media/me/My\\040Music rw - exfat /dev/sdb1 rw\n\
                         39 25 0:48 / /mnt/server rw - nfs4 server:/music rw\n";
        assert_eq!(
            mounted_volumes_from(mountinfo),
            vec![
                PathBuf::from("/mnt/server"),
                PathBuf::from("/run/media/me/My Music")
            ]
        );
    }
}
