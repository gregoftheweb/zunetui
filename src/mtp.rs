use std::{
    error::Error,
    ffi::{CStr, CString, c_char},
    fmt,
    io::{Read, Seek, SeekFrom},
    os::unix::ffi::OsStrExt,
    path::Path,
};

use lofty::{
    config::ParseOptions,
    file::{AudioFile, TaggedFileExt},
    probe::Probe,
    tag::{Accessor, Tag},
};

use crate::{debug_log, raw, stdout_guard};

#[derive(Debug, Clone, Copy)]
pub struct ZuneNotFound;

impl fmt::Display for ZuneNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "No MTP device found. Checklist:\n \
             - Zune plugged in and its screen awake\n \
             - `mtp-detect` (from mtp-tools) sees it\n \
             - you have permission to open the USB device (try sudo once to confirm, then fix with a udev rule)"
        )
    }
}

impl Error for ZuneNotFound {}

pub struct Device {
    ptr: *mut raw::LIBMTP_mtpdevice_t,
}

impl Drop for Device {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            debug_log::log("LIBMTP_Release_Device()");
            stdout_guard::silenced(|| unsafe { raw::LIBMTP_Release_Device(self.ptr) });
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub friendly_name: String,
    pub model: String,
    pub serial: String,
    pub version: String,
    pub capacity_bytes: u64,
    pub free_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct Folder {
    pub id: u32,
    pub storage_id: u32,
    pub name: String,
    pub children: Vec<Folder>,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub id: u32,
    pub parent_id: u32,
    pub storage_id: u32,
    pub name: String,
    pub filename: String,
}

#[derive(Debug, Clone)]
pub struct Album {
    pub id: u32,
    pub name: String,
    pub track_ids: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct Playlist {
    pub id: u32,
    pub parent_id: u32,
    pub storage_id: u32,
    pub name: String,
    pub track_ids: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct DeviceSnapshot {
    pub info: DeviceInfo,
    pub folders: Vec<Folder>,
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub playlists: Vec<Playlist>,
}

pub enum UploadResult {
    Uploaded,
    SkippedDuplicate,
}

pub fn open_device() -> Result<Device, ZuneNotFound> {
    debug_log::log("LIBMTP_Init()");
    stdout_guard::silenced(|| unsafe {
        raw::LIBMTP_Init();
        debug_log::log("LIBMTP_Get_First_Device()");
        let ptr = raw::LIBMTP_Get_First_Device();
        if ptr.is_null() {
            debug_log::log("LIBMTP_Get_First_Device() -> NULL");
            Err(ZuneNotFound)
        } else {
            debug_log::log("LIBMTP_Get_First_Device() -> device");
            let mut extension = (*ptr).extensions;
            let mut extensions = Vec::new();
            while !extension.is_null() {
                let name = copy_borrowed_string((*extension).name);
                extensions.push(format!(
                    "{} {}.{}",
                    if name.is_empty() { "<unnamed>" } else { &name },
                    (*extension).major,
                    (*extension).minor
                ));
                extension = (*extension).next;
            }
            debug_log::log(format!(
                "device extensions: {}",
                if extensions.is_empty() {
                    "<none>".to_owned()
                } else {
                    extensions.join(", ")
                }
            ));
            Ok(Device { ptr })
        }
    })
}

impl Device {
    pub fn snapshot(&self) -> DeviceSnapshot {
        DeviceSnapshot {
            info: self.info(),
            folders: self.folders(),
            tracks: self.tracks(),
            albums: self.albums(),
            playlists: self.playlists(),
        }
    }

    pub fn info(&self) -> DeviceInfo {
        debug_log::log("LIBMTP device information calls");
        stdout_guard::silenced(|| unsafe {
            let friendly_name = take_string(raw::LIBMTP_Get_Friendlyname(self.ptr));
            let model = take_string(raw::LIBMTP_Get_Modelname(self.ptr));
            let serial = take_string(raw::LIBMTP_Get_Serialnumber(self.ptr));
            let version = take_string(raw::LIBMTP_Get_Deviceversion(self.ptr));

            let mut capacity_bytes = 0_u64;
            let mut free_bytes = 0_u64;
            debug_log::log("LIBMTP_Get_Storage(sortby=0)");
            if raw::LIBMTP_Get_Storage(self.ptr, 0) == 0 {
                let mut storage = (*self.ptr).storage;
                while !storage.is_null() {
                    debug_log::log(format!(
                        "LIBMTP storage: id={} (0x{:08x}), StorageType=0x{:04x}, FilesystemType=0x{:04x}, AccessCapability=0x{:04x}, MaxCapacity={}, FreeSpaceInBytes={}, FreeSpaceInObjects={}, StorageDescription={:?}, VolumeIdentifier={:?}",
                        (*storage).id,
                        (*storage).id,
                        (*storage).StorageType,
                        (*storage).FilesystemType,
                        (*storage).AccessCapability,
                        (*storage).MaxCapacity,
                        (*storage).FreeSpaceInBytes,
                        (*storage).FreeSpaceInObjects,
                        copy_borrowed_string((*storage).StorageDescription),
                        copy_borrowed_string((*storage).VolumeIdentifier),
                    ));
                    capacity_bytes = capacity_bytes.saturating_add((*storage).MaxCapacity);
                    free_bytes = free_bytes.saturating_add((*storage).FreeSpaceInBytes);
                    storage = (*storage).next;
                }
            }

            let info = DeviceInfo {
                friendly_name,
                model,
                serial,
                version,
                capacity_bytes,
                free_bytes,
            };
            debug_log::log(format!(
                "device info: friendly_name={:?}, model={:?}, serial={:?}, firmware={:?}, capacity_bytes={}, free_bytes={}",
                info.friendly_name,
                info.model,
                info.serial,
                info.version,
                info.capacity_bytes,
                info.free_bytes
            ));
            info
        })
    }

    pub fn folders(&self) -> Vec<Folder> {
        debug_log::log("LIBMTP_Get_Folder_List()");
        stdout_guard::silenced(|| unsafe {
            let folders = raw::LIBMTP_Get_Folder_List(self.ptr);
            if folders.is_null() {
                return Vec::new();
            }

            let copied = copy_folder_list(folders, 0);
            raw::LIBMTP_destroy_folder_t(folders);
            copied
        })
    }

    pub fn tracks(&self) -> Vec<Track> {
        debug_log::log("LIBMTP_Get_Tracklisting()");
        stdout_guard::silenced(|| unsafe {
            let tracks = raw::LIBMTP_Get_Tracklisting(self.ptr);
            if tracks.is_null() {
                return Vec::new();
            }
            let copied = copy_track_list(tracks);
            raw::LIBMTP_destroy_track_t(tracks);
            copied
        })
    }

    pub fn albums(&self) -> Vec<Album> {
        debug_log::log("LIBMTP_Get_Album_List()");
        stdout_guard::silenced(|| unsafe {
            let albums = raw::LIBMTP_Get_Album_List(self.ptr);
            if albums.is_null() {
                return Vec::new();
            }
            let copied = copy_album_list(albums);
            destroy_album_list(albums);
            copied
        })
    }

    pub fn playlists(&self) -> Vec<Playlist> {
        debug_log::log("LIBMTP_Get_Playlist_List()");
        stdout_guard::silenced(|| unsafe {
            let playlists = raw::LIBMTP_Get_Playlist_List(self.ptr);
            if playlists.is_null() {
                return Vec::new();
            }
            let copied = copy_playlist_list(playlists);
            destroy_playlist_list(playlists);
            copied
        })
    }

    pub fn create_playlist(&self, name: &str, storage_id: u32) -> Result<(), String> {
        let name = CString::new(name).map_err(|_| "playlist name contains a NUL byte")?;
        debug_log::log(format!(
            "LIBMTP_Create_New_Playlist(name={:?}, storage_id={storage_id})",
            name.to_string_lossy()
        ));
        stdout_guard::silenced(|| unsafe {
            let playlist = raw::LIBMTP_new_playlist_t();
            if playlist.is_null() {
                return Err("libmtp could not allocate playlist metadata".to_owned());
            }
            (*playlist).name = libc::strdup(name.as_ptr());
            (*playlist).storage_id = storage_id;
            let result = raw::LIBMTP_Create_New_Playlist(self.ptr, playlist);
            let error = (result != 0).then(|| self.take_error_stack());
            raw::LIBMTP_destroy_playlist_t(playlist);
            match error {
                None => Ok(()),
                Some(error) => Err(error),
            }
        })
    }

    pub fn update_playlist_tracks(
        &self,
        playlist: &Playlist,
        track_ids: &[u32],
    ) -> Result<(), String> {
        debug_log::log(format!(
            "LIBMTP_Update_Playlist(playlist_id={}, name={:?}, track_ids={track_ids:?})",
            playlist.id, playlist.name
        ));
        stdout_guard::silenced(|| unsafe {
            let raw_playlist = raw::LIBMTP_new_playlist_t();
            if raw_playlist.is_null() {
                return Err("libmtp could not allocate playlist metadata".to_owned());
            }
            let name = CString::new(playlist.name.as_str())
                .map_err(|_| "playlist name contains a NUL byte".to_owned())?;
            (*raw_playlist).playlist_id = playlist.id;
            (*raw_playlist).parent_id = playlist.parent_id;
            (*raw_playlist).storage_id = playlist.storage_id;
            (*raw_playlist).name = libc::strdup(name.as_ptr());
            (*raw_playlist).no_tracks = u32::try_from(track_ids.len())
                .map_err(|_| "playlist has too many tracks".to_owned())?;
            if !track_ids.is_empty() {
                (*raw_playlist).tracks = libc::malloc(std::mem::size_of_val(track_ids)).cast();
                if (*raw_playlist).tracks.is_null() {
                    raw::LIBMTP_destroy_playlist_t(raw_playlist);
                    return Err("could not allocate playlist track list".to_owned());
                }
                std::ptr::copy_nonoverlapping(
                    track_ids.as_ptr(),
                    (*raw_playlist).tracks,
                    track_ids.len(),
                );
            }
            let result = raw::LIBMTP_Update_Playlist(self.ptr, raw_playlist);
            let error = (result != 0).then(|| self.take_error_stack());
            raw::LIBMTP_destroy_playlist_t(raw_playlist);
            match error {
                None => Ok(()),
                Some(error) => Err(error),
            }
        })
    }

    /// Deletes a device object. `is_folder` marks `object_id` as a folder:
    /// the device won't delete a folder that still holds objects it never
    /// told us about (the Zune stashes its own album-art thumbnails inside
    /// album folders, invisible to `tracks()`), so its full contents are
    /// wiped recursively first. The caller has already confirmed this
    /// deletion, so everything inside goes with it.
    pub fn delete_object(&self, object_id: u32, is_folder: bool, name: &str) -> Result<(), String> {
        debug_log::log(format!(
            "delete requested: object_id={object_id}, name={name:?}, is_folder={is_folder}"
        ));
        if is_folder {
            self.wipe_folder_contents(object_id, name)?;
        }
        debug_log::log(format!(
            "LIBMTP_Delete_Object(object_id={object_id}, name={name:?})"
        ));
        stdout_guard::silenced(|| unsafe {
            if raw::LIBMTP_Delete_Object(self.ptr, object_id) == 0 {
                debug_log::log(format!(
                    "LIBMTP_Delete_Object(object_id={object_id}, name={name:?}) -> success"
                ));
                Ok(())
            } else {
                let error = self.take_error_stack();
                debug_log::log(format!(
                    "LIBMTP_Delete_Object(object_id={object_id}, name={name:?}) -> failure: {error}"
                ));
                Err(error)
            }
        })
    }

    /// Recursively deletes every object parented directly or indirectly
    /// under `folder_id`, leaving the folder itself empty for the caller to
    /// delete. `LIBMTP_Get_Files_And_Folders` can't be used here: libmtp
    /// refuses it once the device's object cache is built (logged as "tried
    /// to use LIBMTP_Get_Files_And_Folders on a cached device!"), which is
    /// always true for us since `folders()`/`tracks()` build that cache at
    /// connection time. Instead this re-fetches the folder tree (for
    /// subfolder ids) and the full file listing (for anything parented
    /// there that isn't a `Track` — e.g. the Zune's own album-art files),
    /// both of which are cache-safe.
    fn wipe_folder_contents(&self, folder_id: u32, name: &str) -> Result<(), String> {
        let folders = self.folders();
        let mut folder_ids = vec![folder_id];
        let mut delete_subfolders_first = Vec::new();
        if let Some(target) = find_folder_ref(&folders, folder_id) {
            collect_descendant_ids_post_order(target, &mut delete_subfolders_first);
            folder_ids.extend(delete_subfolders_first.iter().copied());
        } else {
            debug_log::log(format!(
                "wipe_folder_contents: folder_id={folder_id} ({name:?}) not found in a fresh folder listing; clearing files by parent_id only"
            ));
        }
        debug_log::log(format!(
            "wipe_folder_contents: folder_id={folder_id} ({name:?}), subfolder_ids={delete_subfolders_first:?}"
        ));

        debug_log::log("LIBMTP_Get_Filelisting_With_Callback() to find files hidden from tracks()");
        let files = stdout_guard::silenced(|| unsafe {
            let head = raw::LIBMTP_Get_Filelisting_With_Callback(self.ptr, None, std::ptr::null());
            let mut files = Vec::new();
            let mut current = head;
            while !current.is_null() {
                let entry = &*current;
                files.push((entry.item_id, entry.parent_id, entry.filetype));
                current = entry.next;
            }
            if !head.is_null() {
                raw::LIBMTP_destroy_file_t(head);
            }
            files
        });
        debug_log::log(format!(
            "LIBMTP_Get_Filelisting_With_Callback() -> {} file(s) on device",
            files.len()
        ));

        for (item_id, parent_id, filetype) in &files {
            if *filetype == raw::LIBMTP_filetype_t_LIBMTP_FILETYPE_FOLDER
                || !folder_ids.contains(parent_id)
            {
                continue;
            }
            debug_log::log(format!(
                "clearing stray file before folder delete: item_id={item_id}, parent_id={parent_id}, filetype={filetype}"
            ));
            stdout_guard::silenced(|| unsafe {
                if raw::LIBMTP_Delete_Object(self.ptr, *item_id) == 0 {
                    Ok(())
                } else {
                    let error = self.take_error_stack();
                    debug_log::log(format!(
                        "LIBMTP_Delete_Object(object_id={item_id}) -> failure: {error}"
                    ));
                    Err(format!(
                        "could not clear a stray file inside the folder before deleting it: {error}"
                    ))
                }
            })?;
        }

        for subfolder_id in delete_subfolders_first {
            debug_log::log(format!(
                "LIBMTP_Delete_Object(object_id={subfolder_id}) — emptied subfolder of {folder_id}"
            ));
            stdout_guard::silenced(|| unsafe {
                if raw::LIBMTP_Delete_Object(self.ptr, subfolder_id) == 0 {
                    Ok(())
                } else {
                    let error = self.take_error_stack();
                    debug_log::log(format!(
                        "LIBMTP_Delete_Object(object_id={subfolder_id}) -> failure: {error}"
                    ));
                    Err(format!("could not delete an emptied subfolder: {error}"))
                }
            })?;
        }
        Ok(())
    }

    pub fn upload_track_resolved(
        &self,
        source: &Path,
        fallback_parent_id: u32,
        fallback_storage_id: u32,
        folders: &mut [Folder],
        tracks: &mut Vec<Track>,
    ) -> Result<UploadResult, String> {
        let metadata = std::fs::metadata(source).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("source is not a regular file".to_owned());
        }
        let path = path_cstring(source)?;
        let filename = source
            .file_name()
            .ok_or_else(|| "source has no filename".to_owned())?;
        let filename_c = os_cstring(filename)?;
        let parsed = UploadTrackMetadata::read(source, filename)?;
        let (parent_id, storage_id) = self.resolve_upload_destination(
            folders,
            source,
            fallback_parent_id,
            fallback_storage_id,
            parsed.artist.as_deref(),
            parsed.album.as_deref(),
        )?;
        let title = parsed.title.as_deref().unwrap_or_default();
        if duplicate_track_exists(tracks, parent_id, title) {
            debug_log::log(format!(
                "upload skipped: duplicate title={title:?}, parent_id={parent_id}, source={source:?}"
            ));
            return Ok(UploadResult::SkippedDuplicate);
        }

        debug_log::log(format!(
            "LIBMTP_Send_Track_From_File(storage_id={storage_id}, parent_id={parent_id}, filename={:?})",
            source
        ));
        stdout_guard::silenced(|| unsafe {
            let track = raw::LIBMTP_new_track_t();
            if track.is_null() {
                return Err("libmtp could not allocate track metadata".to_owned());
            }
            (*track).filename = libc::strdup(filename_c.as_ptr());
            (*track).title = strdup_optional(parsed.title.as_deref());
            (*track).artist = strdup_optional(parsed.artist.as_deref());
            (*track).album = strdup_optional(parsed.album.as_deref());
            (*track).genre = strdup_optional(parsed.genre.as_deref());
            (*track).date = strdup_optional(parsed.date.as_deref());
            (*track).tracknumber = parsed.tracknumber;
            (*track).duration = parsed.duration;
            (*track).samplerate = parsed.samplerate;
            (*track).nochannels = parsed.nochannels;
            (*track).wavecodec = parsed.wavecodec;
            (*track).bitrate = parsed.bitrate;
            (*track).bitratetype = parsed.bitratetype;
            (*track).filesize = metadata.len();
            (*track).parent_id = parent_id;
            (*track).storage_id = storage_id;
            (*track).filetype = filetype_for(source);

            if (*track).filename.is_null() || (*track).title.is_null() {
                raw::LIBMTP_destroy_track_t(track);
                return Err("could not allocate track strings".to_owned());
            }

            debug_log::log(format!(
                "LIBMTP track fields: title={:?}, artist={:?}, album={:?}, genre={:?}, date={:?}, tracknumber={}, duration={}, samplerate={}, nochannels={}, wavecodec=0x{:08x}, bitrate={}, bitratetype={}",
                parsed.title,
                parsed.artist,
                parsed.album,
                parsed.genre,
                parsed.date,
                (*track).tracknumber,
                (*track).duration,
                (*track).samplerate,
                (*track).nochannels,
                (*track).wavecodec,
                (*track).bitrate,
                (*track).bitratetype,
            ));

            let result = raw::LIBMTP_Send_Track_From_File(
                self.ptr,
                path.as_ptr(),
                track,
                None,
                std::ptr::null(),
            );
            let uploaded_id = (*track).item_id;
            let error = if result != 0 {
                Some(self.take_error_stack())
            } else if let Some(album) = parsed.album.as_deref() {
                self.associate_album(track, album, &parsed).err()
            } else {
                None
            };
            raw::LIBMTP_destroy_track_t(track);
            match error {
                None => {
                    debug_log::log("LIBMTP_Send_Track_From_File() -> success");
                    tracks.push(Track {
                        id: uploaded_id,
                        parent_id,
                        storage_id,
                        name: parsed.title.clone().unwrap_or_default(),
                        filename: filename.to_string_lossy().into_owned(),
                    });
                    Ok(UploadResult::Uploaded)
                }
                Some(message) => {
                    debug_log::log("LIBMTP_Send_Track_From_File() -> failure");
                    Err(message)
                }
            }
        })
    }

    /// Picks the upload destination. A folder the caller explicitly picked
    /// on the device (an artist or album folder under Music) always wins
    /// over tag-derived placement — that's how "put this album under that
    /// artist" is honored even when the file's tags disagree. Otherwise
    /// falls back to matching/creating a Music/Artist/Album path from tags,
    /// or the caller's fallback folder when there are no usable tags.
    fn resolve_upload_destination(
        &self,
        folders: &mut [Folder],
        source: &Path,
        fallback_parent_id: u32,
        fallback_storage_id: u32,
        artist: Option<&str>,
        album: Option<&str>,
    ) -> Result<(u32, u32), String> {
        match explicit_destination(folders, fallback_parent_id) {
            ExplicitDestination::Album {
                folder_id,
                storage_id,
            } => Ok((folder_id, storage_id)),
            ExplicitDestination::Artist {
                artist_folder_id,
                storage_id,
            } => {
                let album_name = album
                    .map(str::to_owned)
                    .unwrap_or_else(|| local_album_folder_name(source));
                let id = self.find_or_create_album_under_artist(
                    folders,
                    artist_folder_id,
                    storage_id,
                    &album_name,
                )?;
                Ok((id, storage_id))
            }
            ExplicitDestination::None => match (artist, album) {
                (Some(artist), Some(album)) => {
                    self.resolve_music_destination(folders, fallback_storage_id, artist, album)
                }
                _ => Ok((fallback_parent_id, fallback_storage_id)),
            },
        }
    }

    fn find_or_create_album_under_artist(
        &self,
        folders: &mut [Folder],
        artist_folder_id: u32,
        storage_id: u32,
        album_name: &str,
    ) -> Result<u32, String> {
        let artist_folder = find_folder_mut(folders, artist_folder_id)
            .ok_or_else(|| "selected artist folder is no longer available".to_owned())?;
        if let Some(index) = matching_folder_index(&artist_folder.children, album_name) {
            return Ok(artist_folder.children[index].id);
        }
        let name = name_without_leading_the(album_name);
        let id = self.create_folder(name, artist_folder.id, storage_id)?;
        artist_folder.children.push(Folder {
            id,
            storage_id,
            name: name.to_owned(),
            children: Vec::new(),
        });
        Ok(id)
    }

    fn resolve_music_destination(
        &self,
        folders: &mut [Folder],
        _fallback_storage_id: u32,
        artist: &str,
        album: &str,
    ) -> Result<(u32, u32), String> {
        let music = folders
            .iter_mut()
            .find(|folder| folder.name.eq_ignore_ascii_case("Music"))
            .ok_or_else(|| "device has no top-level Music folder".to_owned())?;
        let storage_id = music.storage_id;
        let artist_index = if let Some(index) = matching_folder_index(&music.children, artist) {
            index
        } else {
            let name = name_without_leading_the(artist);
            let id = self.create_folder(name, music.id, storage_id)?;
            music.children.push(Folder {
                id,
                storage_id,
                name: name.to_owned(),
                children: Vec::new(),
            });
            music.children.len() - 1
        };
        let artist_folder = &mut music.children[artist_index];
        let album_index = if let Some(index) = matching_folder_index(&artist_folder.children, album)
        {
            index
        } else {
            let name = name_without_leading_the(album);
            let id = self.create_folder(name, artist_folder.id, storage_id)?;
            artist_folder.children.push(Folder {
                id,
                storage_id,
                name: name.to_owned(),
                children: Vec::new(),
            });
            artist_folder.children.len() - 1
        };
        Ok((artist_folder.children[album_index].id, storage_id))
    }

    fn create_folder(&self, name: &str, parent_id: u32, storage_id: u32) -> Result<u32, String> {
        let name = CString::new(name).map_err(|_| "folder name contains a null byte".to_owned())?;
        debug_log::log(format!(
            "LIBMTP_Create_Folder(name={:?}, parent_id={parent_id}, storage_id={storage_id})",
            name.to_string_lossy()
        ));
        stdout_guard::silenced(|| unsafe {
            let id = raw::LIBMTP_Create_Folder(
                self.ptr,
                name.as_ptr().cast_mut(),
                parent_id,
                storage_id,
            );
            if id == 0 {
                Err(self.take_error_stack())
            } else {
                debug_log::log(format!("LIBMTP_Create_Folder() -> folder_id={id}"));
                Ok(id)
            }
        })
    }

    pub fn download_track(&self, track: &Track, destination: &Path) -> Result<(), String> {
        if destination.exists() {
            return Err("destination file already exists".to_owned());
        }
        let path = path_cstring(destination)?;
        debug_log::log(format!(
            "LIBMTP_Get_Track_To_File(track_id={}, storage_id={}, parent_id={}, filename={:?})",
            track.id, track.storage_id, track.parent_id, destination
        ));
        stdout_guard::silenced(|| unsafe {
            let result = raw::LIBMTP_Get_Track_To_File(
                self.ptr,
                track.id,
                path.as_ptr(),
                None,
                std::ptr::null(),
            );
            if result == 0 {
                debug_log::log("LIBMTP_Get_Track_To_File() -> success");
                Ok(())
            } else {
                debug_log::log("LIBMTP_Get_Track_To_File() -> failure");
                let message = self.take_error_stack();
                let _ = std::fs::remove_file(destination);
                Err(message)
            }
        })
    }

    unsafe fn take_error_stack(&self) -> String {
        let mut error = unsafe { raw::LIBMTP_Get_Errorstack(self.ptr) };
        let mut messages = Vec::new();
        while !error.is_null() {
            let current = unsafe { &*error };
            let message = unsafe { copy_borrowed_string(current.error_text) };
            if !message.is_empty() {
                debug_log::log(format!(
                    "LIBMTP error stack: number={} message={message}",
                    current.errornumber
                ));
                messages.push(message);
            }
            error = current.next;
        }
        unsafe { raw::LIBMTP_Clear_Errorstack(self.ptr) };
        if messages.is_empty() {
            "libmtp transfer failed without an error-stack message".to_owned()
        } else {
            messages.join(": ")
        }
    }

    unsafe fn associate_album(
        &self,
        track: *const raw::LIBMTP_track_t,
        album_name: &str,
        metadata: &UploadTrackMetadata,
    ) -> Result<(), String> {
        debug_log::log(format!(
            "associating track {} with album {album_name:?}",
            unsafe { (*track).item_id }
        ));
        let albums = unsafe { raw::LIBMTP_Get_Album_List(self.ptr) };
        let mut current = albums;
        let mut matching = std::ptr::null_mut();
        while !current.is_null() {
            let name = unsafe { copy_borrowed_string((*current).name) };
            let artist = unsafe { copy_borrowed_string((*current).artist) };
            if name == album_name
                && metadata
                    .artist
                    .as_deref()
                    .is_none_or(|expected| artist.is_empty() || artist == expected)
            {
                matching = current;
                break;
            }
            current = unsafe { (*current).next };
        }

        let result = if matching.is_null() {
            unsafe { self.create_album_for_track(track, album_name, metadata) }
        } else {
            unsafe { self.append_track_to_album(matching, (*track).item_id) }
        };
        unsafe { destroy_album_list(albums) };
        result
    }

    unsafe fn create_album_for_track(
        &self,
        track: *const raw::LIBMTP_track_t,
        album_name: &str,
        metadata: &UploadTrackMetadata,
    ) -> Result<(), String> {
        let album = unsafe { raw::LIBMTP_new_album_t() };
        if album.is_null() {
            return Err("track uploaded, but libmtp could not allocate album metadata".to_owned());
        }
        unsafe {
            (*album).name = strdup_optional(Some(album_name));
            (*album).artist = strdup_optional(metadata.artist.as_deref());
            (*album).genre = strdup_optional(metadata.genre.as_deref());
            (*album).storage_id = (*track).storage_id;
            (*album).tracks = libc::malloc(std::mem::size_of::<u32>()).cast();
            (*album).no_tracks = 1;
        }
        if unsafe { (*album).name.is_null() || (*album).tracks.is_null() } {
            unsafe { raw::LIBMTP_destroy_album_t(album) };
            return Err("track uploaded, but album metadata allocation failed".to_owned());
        }
        unsafe { *(*album).tracks = (*track).item_id };
        let result = unsafe { raw::LIBMTP_Create_New_Album(self.ptr, album) };
        let error = (result != 0).then(|| unsafe { self.take_error_stack() });
        unsafe { raw::LIBMTP_destroy_album_t(album) };
        match error {
            None => {
                debug_log::log("LIBMTP_Create_New_Album() -> success");
                Ok(())
            }
            Some(error) => Err(format!(
                "track uploaded, but album creation failed: {error}"
            )),
        }
    }

    unsafe fn append_track_to_album(
        &self,
        album: *mut raw::LIBMTP_album_t,
        track_id: u32,
    ) -> Result<(), String> {
        let old_len = unsafe { (*album).no_tracks as usize };
        let old_tracks = unsafe { (*album).tracks };
        let existing = if old_tracks.is_null() {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(old_tracks, old_len) }
        };
        if existing.contains(&track_id) {
            return Ok(());
        }

        let new_len = old_len + 1;
        let new_len_u32 = u32::try_from(new_len)
            .map_err(|_| "track uploaded, but the album has too many tracks".to_owned())?;
        let new_tracks =
            unsafe { libc::malloc(new_len * std::mem::size_of::<u32>()).cast::<u32>() };
        if new_tracks.is_null() {
            return Err("track uploaded, but album track-list allocation failed".to_owned());
        }
        unsafe {
            if old_len != 0 {
                std::ptr::copy_nonoverlapping(old_tracks, new_tracks, old_len);
            }
            *new_tracks.add(old_len) = track_id;
            libc::free(old_tracks.cast());
            (*album).tracks = new_tracks;
            (*album).no_tracks = new_len_u32;
        }
        let result = unsafe { raw::LIBMTP_Update_Album(self.ptr, album) };
        if result == 0 {
            debug_log::log("LIBMTP_Update_Album() -> success");
            Ok(())
        } else {
            Err(format!(
                "track uploaded, but album update failed: {}",
                unsafe { self.take_error_stack() }
            ))
        }
    }
}

enum ExplicitDestination {
    /// The caller picked an album-level folder (Music/Artist/Album) directly:
    /// use it as-is, no tag matching at all.
    Album { folder_id: u32, storage_id: u32 },
    /// The caller picked an artist-level folder (Music/Artist): file the
    /// upload under it, resolving only the album name.
    Artist {
        artist_folder_id: u32,
        storage_id: u32,
    },
    /// The caller's folder isn't an artist/album folder under Music (e.g.
    /// Music itself, or an unrelated folder) — defer to tag-based placement.
    None,
}

fn explicit_destination(folders: &[Folder], folder_id: u32) -> ExplicitDestination {
    let Some(path) = folder_path(folders, folder_id) else {
        return ExplicitDestination::None;
    };
    let Some(music_index) = path
        .iter()
        .position(|folder| folder.name.eq_ignore_ascii_case("Music"))
    else {
        return ExplicitDestination::None;
    };
    let depth_from_music = path.len() - 1 - music_index;
    let storage_id = path.last().expect("path is non-empty").storage_id;
    match depth_from_music {
        1 => ExplicitDestination::Artist {
            artist_folder_id: folder_id,
            storage_id,
        },
        2 => ExplicitDestination::Album {
            folder_id,
            storage_id,
        },
        _ => ExplicitDestination::None,
    }
}

fn folder_path(folders: &[Folder], target: u32) -> Option<Vec<&Folder>> {
    for folder in folders {
        if folder.id == target {
            return Some(vec![folder]);
        }
        if let Some(mut path) = folder_path(&folder.children, target) {
            path.insert(0, folder);
            return Some(path);
        }
    }
    None
}

fn find_folder_mut(folders: &mut [Folder], target: u32) -> Option<&mut Folder> {
    for folder in folders {
        if folder.id == target {
            return Some(folder);
        }
        if let Some(found) = find_folder_mut(&mut folder.children, target) {
            return Some(found);
        }
    }
    None
}

fn find_folder_ref(folders: &[Folder], target: u32) -> Option<&Folder> {
    folders.iter().find_map(|folder| {
        if folder.id == target {
            Some(folder)
        } else {
            find_folder_ref(&folder.children, target)
        }
    })
}

/// Appends every descendant folder id in post-order (children before their
/// own parent), so deleting the ids in order always empties a subfolder
/// before the folder that contains it.
fn collect_descendant_ids_post_order(folder: &Folder, out: &mut Vec<u32>) {
    for child in &folder.children {
        collect_descendant_ids_post_order(child, out);
        out.push(child.id);
    }
}

fn local_album_folder_name(source: &Path) -> String {
    source
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Unknown Album".to_owned())
}

/// True when `folder_id` is an artist-level folder (Music/Artist) — the same
/// classification `resolve_upload_destination` uses to decide whether an
/// upload's destination is explicit. Exposed so the UI can warn before
/// placing an album under an artist folder whose name doesn't match the
/// files being uploaded.
pub fn is_artist_folder(folders: &[Folder], folder_id: u32) -> bool {
    matches!(
        explicit_destination(folders, folder_id),
        ExplicitDestination::Artist { .. }
    )
}

/// Case-insensitive, "The "-agnostic artist name comparison — the same
/// notion of "same artist" used when matching/creating folders on the
/// device.
pub fn same_artist_name(a: &str, b: &str) -> bool {
    normalized_folder_name(a) == normalized_folder_name(b)
}

/// Best-effort guess at a local file's artist, for warning the user before
/// an upload — not used for the actual upload's metadata (see
/// `UploadTrackMetadata::read`), so a failure to read tags is just `None`
/// rather than an error.
pub fn detect_artist(source: &Path) -> Option<String> {
    let tag_artist = lofty::read_from_path(source).ok().and_then(|tagged| {
        tagged
            .primary_tag()
            .or_else(|| tagged.first_tag())
            .and_then(Accessor::artist)
            .map(|value| value.into_owned())
            .filter(|value| !value.trim().is_empty())
    });
    tag_artist.or_else(|| folder_artist_album(source).0)
}

fn normalized_folder_name(name: &str) -> String {
    let lower = name.to_lowercase();
    lower.strip_prefix("the ").unwrap_or(&lower).to_owned()
}

fn name_without_leading_the(name: &str) -> &str {
    if name
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("the "))
    {
        &name[4..]
    } else {
        name
    }
}

fn matching_folder_index(folders: &[Folder], wanted: &str) -> Option<usize> {
    let wanted = normalized_folder_name(wanted);
    folders
        .iter()
        .position(|folder| normalized_folder_name(&folder.name) == wanted)
}

fn duplicate_track_exists(tracks: &[Track], parent_id: u32, title: &str) -> bool {
    tracks
        .iter()
        .any(|track| track.parent_id == parent_id && track.name == title)
}

#[derive(Debug)]
struct UploadTrackMetadata {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    genre: Option<String>,
    date: Option<String>,
    tracknumber: u16,
    duration: u32,
    samplerate: u32,
    nochannels: u16,
    wavecodec: u32,
    bitrate: u32,
    bitratetype: u16,
}

impl UploadTrackMetadata {
    fn read(source: &Path, filename: &std::ffi::OsStr) -> Result<Self, String> {
        let tagged = match lofty::read_from_path(source) {
            Ok(tagged) => tagged,
            Err(tag_error) => {
                debug_log::log(format!(
                    "audio tag parse failed for {source:?}; retrying properties only: {tag_error}"
                ));
                Probe::open(source)
                    .map_err(|error| format!("could not open audio file: {error}"))?
                    .options(ParseOptions::new().read_tags(false))
                    .read()
                    .map_err(|error| format!("could not read audio properties: {error}"))?
            }
        };
        let properties = tagged.properties();
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

        let duration = u32::try_from(properties.duration().as_millis())
            .map_err(|_| "audio duration is too large for MTP metadata".to_owned())?;
        let samplerate = properties
            .sample_rate()
            .filter(|value| *value != 0)
            .ok_or_else(|| "audio sample rate could not be determined".to_owned())?;
        let nochannels = properties
            .channels()
            .filter(|value| *value != 0)
            .map(u16::from)
            .ok_or_else(|| "audio channel count could not be determined".to_owned())?;
        let bitrate_kbps = properties
            .audio_bitrate()
            .filter(|value| *value != 0)
            .ok_or_else(|| "audio bitrate could not be determined".to_owned())?;
        let bitrate = bitrate_kbps
            .checked_mul(1_000)
            .ok_or_else(|| "audio bitrate is too large for MTP metadata".to_owned())?;

        let fallback_title = source.file_stem().unwrap_or(filename).to_string_lossy();
        let title = tag
            .and_then(Accessor::title)
            .filter(|value| !value.is_empty())
            .map(|value| value.into_owned())
            .unwrap_or_else(|| fallback_title.into_owned());
        let (folder_artist, folder_album) = folder_artist_album(source);
        let tag_artist = tag
            .and_then(Accessor::artist)
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.into_owned());
        let tag_album = tag
            .and_then(Accessor::album)
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.into_owned());

        Ok(Self {
            title: Some(title),
            artist: tag_artist.or(folder_artist),
            album: tag_album.or(folder_album),
            genre: tag
                .and_then(Accessor::genre)
                .map(|value| value.into_owned()),
            date: tag.and_then(Tag::date).map(|value| {
                format!(
                    "{:04}{:02}{:02}T{:02}{:02}{:02}.0",
                    value.year,
                    value.month.unwrap_or(1),
                    value.day.unwrap_or(1),
                    value.hour.unwrap_or(0),
                    value.minute.unwrap_or(0),
                    value.second.unwrap_or(0),
                )
            }),
            tracknumber: tag
                .and_then(Accessor::track)
                .map(u16::try_from)
                .transpose()
                .map_err(|_| "track number is too large for MTP metadata".to_owned())?
                .unwrap_or(0),
            duration,
            samplerate,
            nochannels,
            // WAVE_FORMAT_MPEGLAYER3, the codec identifier used by MTP for MP3 tracks.
            wavecodec: 0x0055,
            bitrate,
            bitratetype: mp3_bitrate_type(source)?,
        })
    }
}

fn folder_artist_album(source: &Path) -> (Option<String>, Option<String>) {
    let Some(album) = source.parent().and_then(Path::file_name) else {
        return (None, None);
    };
    let Some(artist_dir) = source.parent().and_then(Path::parent) else {
        return (None, None);
    };
    let Some(artist) = artist_dir.file_name() else {
        return (None, None);
    };
    let Some(bucket) = artist_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    else {
        return (None, None);
    };
    if bucket.len() != 1 || !bucket.as_bytes()[0].is_ascii_alphabetic() {
        return (None, None);
    }

    let artist = artist.to_str().filter(|value| !value.trim().is_empty());
    let album = album.to_str().filter(|value| !value.trim().is_empty());
    match (artist, album) {
        (Some(artist), Some(album)) => (Some(artist.to_owned()), Some(album.to_owned())),
        _ => (None, None),
    }
}

fn mp3_bitrate_type(source: &Path) -> Result<u16, String> {
    let mut file = std::fs::File::open(source).map_err(|error| error.to_string())?;
    let mut id3_header = [0_u8; 10];
    file.read_exact(&mut id3_header)
        .map_err(|error| format!("could not read MP3 header: {error}"))?;
    let audio_offset = if &id3_header[..3] == b"ID3" {
        let size = id3_header[6..10]
            .iter()
            .fold(0_u64, |size, byte| (size << 7) | u64::from(byte & 0x7f));
        10 + size + u64::from(id3_header[5] & 0x10 != 0) * 10
    } else {
        0
    };
    file.seek(SeekFrom::Start(audio_offset))
        .map_err(|error| format!("could not seek to MP3 audio: {error}"))?;
    let mut first_frames = [0_u8; 4096];
    let read = file
        .read(&mut first_frames)
        .map_err(|error| format!("could not inspect MP3 bitrate type: {error}"))?;
    let first_frames = &first_frames[..read];
    if first_frames
        .windows(4)
        .any(|window| matches!(window, b"Xing" | b"VBRI"))
    {
        Ok(2)
    } else {
        // LAME's "Info" header explicitly denotes CBR; files without a VBR
        // header are also conventionally CBR.
        Ok(1)
    }
}

unsafe fn strdup_optional(value: Option<&str>) -> *mut c_char {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return std::ptr::null_mut();
    };
    let Ok(value) = CString::new(value) else {
        return std::ptr::null_mut();
    };
    unsafe { libc::strdup(value.as_ptr()) }
}

unsafe fn copy_folder_list(mut folder: *mut raw::LIBMTP_folder_t, depth: usize) -> Vec<Folder> {
    let mut copied = Vec::new();
    while !folder.is_null() {
        let current = unsafe { &*folder };
        let name = unsafe { copy_borrowed_string(current.name) };
        debug_log::log(format!(
            "LIBMTP raw folder: depth={depth}, folder_id={}, parent_id={}, storage_id={}, name={name:?}",
            current.folder_id, current.parent_id, current.storage_id
        ));
        copied.push(Folder {
            id: current.folder_id,
            storage_id: current.storage_id,
            name,
            children: unsafe { copy_folder_list(current.child, depth + 1) },
        });
        folder = current.sibling;
    }
    copied
}

unsafe fn copy_track_list(mut track: *mut raw::LIBMTP_track_t) -> Vec<Track> {
    let mut copied = Vec::new();
    while !track.is_null() {
        let current = unsafe { &*track };
        let title = unsafe { copy_borrowed_string(current.title) };
        let filename = unsafe { copy_borrowed_string(current.filename) };
        copied.push(Track {
            id: current.item_id,
            parent_id: current.parent_id,
            storage_id: current.storage_id,
            name: if title.is_empty() {
                filename.clone()
            } else {
                title
            },
            filename,
        });
        track = current.next;
    }
    copied
}

unsafe fn copy_album_list(mut album: *mut raw::LIBMTP_album_t) -> Vec<Album> {
    let mut copied = Vec::new();
    while !album.is_null() {
        let current = unsafe { &*album };
        let track_ids = if current.tracks.is_null() {
            Vec::new()
        } else {
            unsafe {
                std::slice::from_raw_parts(current.tracks, current.no_tracks as usize).to_vec()
            }
        };
        let name = unsafe { copy_borrowed_string(current.name) };
        let artist = unsafe { copy_borrowed_string(current.artist) };
        debug_log::log(format!(
            "LIBMTP raw album: album_id={}, parent_id={}, storage_id={}, name={name:?}, artist={artist:?}, track_ids={track_ids:?}",
            current.album_id, current.parent_id, current.storage_id
        ));
        copied.push(Album {
            id: current.album_id,
            name,
            track_ids,
        });
        album = current.next;
    }
    copied
}

unsafe fn destroy_album_list(mut album: *mut raw::LIBMTP_album_t) {
    while !album.is_null() {
        let next = unsafe { (*album).next };
        unsafe { raw::LIBMTP_destroy_album_t(album) };
        album = next;
    }
}

unsafe fn copy_playlist_list(mut playlist: *mut raw::LIBMTP_playlist_t) -> Vec<Playlist> {
    let mut copied = Vec::new();
    while !playlist.is_null() {
        let current = unsafe { &*playlist };
        let track_ids = if current.tracks.is_null() {
            Vec::new()
        } else {
            unsafe {
                std::slice::from_raw_parts(current.tracks, current.no_tracks as usize).to_vec()
            }
        };
        let name = unsafe { copy_borrowed_string(current.name) };
        debug_log::log(format!(
            "LIBMTP raw playlist: playlist_id={}, parent_id={}, storage_id={}, name={name:?}, track_ids={track_ids:?}",
            current.playlist_id, current.parent_id, current.storage_id
        ));
        copied.push(Playlist {
            id: current.playlist_id,
            parent_id: current.parent_id,
            storage_id: current.storage_id,
            name,
            track_ids,
        });
        playlist = current.next;
    }
    copied
}

unsafe fn destroy_playlist_list(mut playlist: *mut raw::LIBMTP_playlist_t) {
    while !playlist.is_null() {
        let next = unsafe { (*playlist).next };
        unsafe { raw::LIBMTP_destroy_playlist_t(playlist) };
        playlist = next;
    }
}

fn path_cstring(path: &Path) -> Result<CString, String> {
    os_cstring(path.as_os_str())
}

fn os_cstring(value: &std::ffi::OsStr) -> Result<CString, String> {
    CString::new(value.as_bytes()).map_err(|_| "path contains a NUL byte".to_owned())
}

fn filetype_for(path: &Path) -> raw::LIBMTP_filetype_t {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "wav" => raw::LIBMTP_filetype_t_LIBMTP_FILETYPE_WAV,
        "mp3" => raw::LIBMTP_filetype_t_LIBMTP_FILETYPE_MP3,
        "wma" => raw::LIBMTP_filetype_t_LIBMTP_FILETYPE_WMA,
        "ogg" => raw::LIBMTP_filetype_t_LIBMTP_FILETYPE_OGG,
        "aac" => raw::LIBMTP_filetype_t_LIBMTP_FILETYPE_AAC,
        "flac" => raw::LIBMTP_filetype_t_LIBMTP_FILETYPE_FLAC,
        "m4a" => raw::LIBMTP_filetype_t_LIBMTP_FILETYPE_M4A,
        _ => raw::LIBMTP_filetype_t_LIBMTP_FILETYPE_UNKNOWN,
    }
}

unsafe fn copy_borrowed_string(value: *const c_char) -> String {
    if value.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    }
}

unsafe fn take_string(value: *mut c_char) -> String {
    if value.is_null() {
        return String::new();
    }

    let decoded = unsafe { CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned();
    unsafe { raw::LIBMTP_FreeMemory(value.cast()) };
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    #[test]
    fn null_string_is_empty() {
        assert_eq!(unsafe { take_string(ptr::null_mut()) }, "");
        assert_eq!(unsafe { copy_borrowed_string(ptr::null()) }, "");
    }

    #[test]
    fn not_found_message_is_stable() {
        assert_eq!(
            ZuneNotFound.to_string(),
            "No MTP device found. Checklist:\n \
             - Zune plugged in and its screen awake\n \
             - `mtp-detect` (from mtp-tools) sees it\n \
             - you have permission to open the USB device (try sudo once to confirm, then fix with a udev rule)"
        );
    }

    #[test]
    fn folder_metadata_uses_letter_artist_album_shape() {
        assert_eq!(
            folder_artist_album(Path::new("/music/A/All Them Witches/ATW/Fishbelley.mp3")),
            (Some("All Them Witches".to_owned()), Some("ATW".to_owned()))
        );
    }

    #[test]
    fn real_tags_take_precedence_over_folder_metadata() {
        let (folder_artist, folder_album) =
            folder_artist_album(Path::new("/music/A/Wrong Artist/Wrong Album/song.mp3"));
        let artist = Some("Tagged Artist".to_owned()).or(folder_artist);
        let album = Some("Tagged Album".to_owned()).or(folder_album);

        assert_eq!(artist.as_deref(), Some("Tagged Artist"));
        assert_eq!(album.as_deref(), Some("Tagged Album"));
    }

    #[test]
    fn folder_metadata_rejects_unexpected_shape() {
        assert_eq!(
            folder_artist_album(Path::new("/music/1234/10,000 Days/Vicarious.mp3")),
            (None, None)
        );
        assert_eq!(
            folder_artist_album(Path::new("/music/_assets/loose.mp3")),
            (None, None)
        );
    }

    #[test]
    fn folder_matching_only_case_folds_and_drops_leading_the() {
        let folders = vec![Folder {
            id: 1,
            storage_id: 2,
            name: "Beatles".to_owned(),
            children: Vec::new(),
        }];
        assert_eq!(matching_folder_index(&folders, "The Beatles"), Some(0));
        assert_eq!(matching_folder_index(&folders, "the beatles"), Some(0));
        assert_eq!(matching_folder_index(&folders, "BEATLES"), Some(0));
        assert_eq!(matching_folder_index(&folders, "Beatles!"), None);
        assert_eq!(name_without_leading_the("The Beatles"), "Beatles");
    }

    #[test]
    fn duplicate_titles_are_scoped_to_destination_folder() {
        let tracks = vec![Track {
            id: 1,
            parent_id: 10,
            storage_id: 2,
            name: "Song".to_owned(),
            filename: "song.mp3".to_owned(),
        }];
        assert!(duplicate_track_exists(&tracks, 10, "Song"));
        assert!(!duplicate_track_exists(&tracks, 11, "Song"));
        assert!(!duplicate_track_exists(&tracks, 10, "song"));
    }

    fn music_tree() -> Vec<Folder> {
        vec![Folder {
            id: 1,
            storage_id: 9,
            name: "Music".to_owned(),
            children: vec![Folder {
                id: 2,
                storage_id: 9,
                name: "Neil Diamond".to_owned(),
                children: vec![Folder {
                    id: 3,
                    storage_id: 9,
                    name: "Hot August Night".to_owned(),
                    children: Vec::new(),
                }],
            }],
        }]
    }

    #[test]
    fn explicit_destination_recognizes_artist_and_album_folders_under_music() {
        let folders = music_tree();
        assert!(matches!(
            explicit_destination(&folders, 2),
            ExplicitDestination::Artist {
                artist_folder_id: 2,
                storage_id: 9
            }
        ));
        assert!(matches!(
            explicit_destination(&folders, 3),
            ExplicitDestination::Album {
                folder_id: 3,
                storage_id: 9
            }
        ));
    }

    #[test]
    fn explicit_destination_ignores_music_itself_and_unrelated_folders() {
        let folders = music_tree();
        assert!(matches!(
            explicit_destination(&folders, 1),
            ExplicitDestination::None
        ));
        assert!(matches!(
            explicit_destination(&folders, 42),
            ExplicitDestination::None
        ));
    }

    #[test]
    fn find_folder_mut_locates_nested_folder() {
        let mut folders = music_tree();
        let found = find_folder_mut(&mut folders, 3).expect("album folder exists");
        assert_eq!(found.name, "Hot August Night");
        found.children.push(Folder {
            id: 4,
            storage_id: 9,
            name: "extra".to_owned(),
            children: Vec::new(),
        });
        assert_eq!(folders[0].children[0].children[0].children.len(), 1);
    }

    #[test]
    fn local_album_folder_name_uses_parent_directory() {
        assert_eq!(
            local_album_folder_name(Path::new("/music/Neil Diamond/Hot August Night/song.mp3")),
            "Hot August Night"
        );
        assert_eq!(
            local_album_folder_name(Path::new("song.mp3")),
            "Unknown Album"
        );
    }

    #[test]
    fn is_artist_folder_matches_only_artist_level() {
        let folders = music_tree();
        assert!(is_artist_folder(&folders, 2));
        assert!(!is_artist_folder(&folders, 3));
        assert!(!is_artist_folder(&folders, 1));
    }

    #[test]
    fn same_artist_name_folds_case_and_leading_the() {
        assert!(same_artist_name("The Beatles", "beatles"));
        assert!(same_artist_name("Neil Diamond", "Neil Diamond"));
        assert!(!same_artist_name("Neil Diamond", "Neil Young"));
    }

    #[test]
    fn detect_artist_falls_back_to_letter_bucket_folder_when_file_is_unreadable() {
        // The path doesn't need to exist on disk: the tag read fails and
        // detect_artist falls back to the letter-bucket folder heuristic.
        assert_eq!(
            detect_artist(Path::new("/music/A/All Them Witches/ATW/Fishbelley.mp3")).as_deref(),
            Some("All Them Witches")
        );
        assert_eq!(detect_artist(Path::new("/music/_assets/loose.mp3")), None);
    }

    #[test]
    fn find_folder_ref_locates_nested_folder_immutably() {
        let folders = music_tree();
        assert_eq!(
            find_folder_ref(&folders, 3).unwrap().name,
            "Hot August Night"
        );
        assert!(find_folder_ref(&folders, 999).is_none());
    }

    #[test]
    fn descendant_ids_are_collected_children_before_parent() {
        let folders = vec![Folder {
            id: 1,
            storage_id: 9,
            name: "Music".to_owned(),
            children: vec![Folder {
                id: 2,
                storage_id: 9,
                name: "Artist".to_owned(),
                children: vec![
                    Folder {
                        id: 3,
                        storage_id: 9,
                        name: "Album A".to_owned(),
                        children: Vec::new(),
                    },
                    Folder {
                        id: 4,
                        storage_id: 9,
                        name: "Album B".to_owned(),
                        children: Vec::new(),
                    },
                ],
            }],
        }];
        let artist = find_folder_ref(&folders, 2).unwrap();
        let mut out = Vec::new();
        collect_descendant_ids_post_order(artist, &mut out);
        assert_eq!(out, vec![3, 4]);
    }
}
