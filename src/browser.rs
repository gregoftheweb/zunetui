use std::{
    collections::HashSet,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

use tui_tree_widget::TreeItem;

pub struct LocalTree {
    root: Directory,
}

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
        let mut root = Directory::new(home);
        root.load()?;
        Ok(Self { root })
    }

    pub fn items(&self, marked: &HashSet<PathBuf>) -> Vec<TreeItem<'static, PathBuf>> {
        vec![self.root.to_item(marked)]
    }

    pub fn load(&mut self, path: &Path) {
        if let Some(directory) = self.root.find_mut(path) {
            let _ = directory.load();
        }
    }

    pub fn refresh(&mut self, path: &Path) {
        if let Some(directory) = self.root.find_mut(path) {
            directory.children = None;
            directory.files.clear();
            let _ = directory.load();
        }
    }

    pub fn root_path(&self) -> PathBuf {
        self.root.path.clone()
    }
}

pub fn album_audio_files(path: &Path) -> Option<Vec<PathBuf>> {
    if !is_album_path(path) {
        return None;
    }
    let mut files: Vec<_> = fs::read_dir(path)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_type().ok()?.is_file().then(|| entry.path()))
        .filter(|path| is_audio_file(path))
        .collect();
    files.sort();
    (!files.is_empty()).then_some(files)
}

fn is_album_path(path: &Path) -> bool {
    let Some(bucket) = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
    else {
        return false;
    };
    let bucket = bucket.to_string_lossy();
    bucket.len() == 1 && bucket.bytes().all(|byte| byte.is_ascii_alphabetic())
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
        let label = if is_album_path(&self.path) {
            let audio_files: Vec<_> = self
                .files
                .iter()
                .filter(|path| is_audio_file(path))
                .collect();
            if audio_files.is_empty() {
                self.name.clone()
            } else {
                let indicator = if audio_files.iter().all(|path| marked.contains(*path)) {
                    "[x]"
                } else {
                    "[ ]"
                };
                format!("{indicator} {}", self.name)
            }
        } else {
            self.name.clone()
        };
        TreeItem::new(self.path.clone(), label, children)
            .expect("filesystem directory names are unique within a parent")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_audio_files_requires_letter_artist_album_shape() {
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

        let loose = root.join("Artist/Album");
        fs::create_dir_all(&loose).unwrap();
        fs::write(loose.join("song.mp3"), b"audio").unwrap();
        assert!(album_audio_files(&loose).is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
