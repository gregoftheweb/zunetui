mod app_state;
mod browser;
mod debug_log;
mod mtp;
mod palette;
mod raw;
mod stdout_guard;

use std::{
    collections::HashSet,
    io::{self, Stdout},
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use app_state::PersistedState;
use browser::LocalTree;
use color_eyre::eyre::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
#[cfg(test)]
use mtp::DeviceInfo;
use mtp::{Album, DeviceSnapshot, Folder, Playlist, Track, UploadResult, ZuneNotFound};
use palette::{Theme, palette};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use tui_tree_widget::{Tree, TreeItem, TreeState};

const LOGO: &str = r#"███████╗██╗   ██╗███╗   ██╗███████╗    ████████╗██╗   ██╗██╗
╚══███╔╝██║   ██║████╗  ██║██╔════╝    ╚══██╔══╝██║   ██║██║
  ███╔╝ ██║   ██║██╔██╗ ██║█████╗         ██║   ██║   ██║██║
 ███╔╝  ██║   ██║██║╚██╗██║██╔══╝         ██║   ██║   ██║██║
███████╗╚██████╔╝██║ ╚████║███████╗       ██║   ╚██████╔╝██║
╚══════╝ ╚═════╝ ╚═╝  ╚═══╝╚══════╝       ╚═╝    ╚═════╝ ╚═╝"#;
const NOT_FOUND_SHORT: &str = "Dude, I can't find your Zune, is it connected?";

type ConnectionResult = std::result::Result<DeviceSnapshot, ZuneNotFound>;

#[derive(Clone, Copy)]
enum Screen {
    Splash { continue_pressed: bool },
    Files,
    Playlists,
    AddToPlaylist,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Pane {
    Local,
    Device,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlaylistPane {
    Playlists,
    Tracks,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum DeviceItemId {
    Folder(u32),
    Track(u32),
}

#[derive(Clone)]
struct DeleteItem {
    object_id: u32,
    name: String,
}

struct DeleteConfirmation {
    device_name: String,
    items: Vec<DeleteItem>,
    track_count: usize,
    album_count: usize,
    artist_names: Vec<String>,
}

enum PlaylistConfirmation {
    DeletePlaylists(Vec<Playlist>),
    RemoveTracks {
        playlist: Playlist,
        track_ids: Vec<u32>,
    },
}

#[derive(Clone)]
struct PhysicalAlbum {
    folder_id: u32,
    name: String,
    artist_id: u32,
    artist_name: String,
    track_ids: Vec<u32>,
    artist_album_count: usize,
}

enum TransferJob {
    Upload {
        files: Vec<PathBuf>,
        folder_id: u32,
        storage_id: u32,
    },
    Download {
        tracks: Vec<Track>,
        directory: PathBuf,
    },
    Delete {
        items: Vec<DeleteItem>,
    },
    CreatePlaylist {
        name: String,
        storage_id: u32,
    },
    DeletePlaylists {
        playlists: Vec<Playlist>,
    },
    RemovePlaylistTracks {
        playlist: Playlist,
        track_ids: Vec<u32>,
    },
    AddPlaylistTracks {
        playlist: Playlist,
        track_ids: Vec<u32>,
    },
}

enum TransferEvent {
    Progress {
        current: usize,
        total: usize,
        name: String,
        verb: &'static str,
    },
    Finished {
        completed: usize,
        failures: Vec<String>,
        snapshot: Option<DeviceSnapshot>,
        verb: &'static str,
    },
}

enum Connection {
    Loading(&'static str),
    Connected(DeviceSnapshot),
    NotFound,
}

struct App {
    screen: Screen,
    connection: Connection,
    receiver: Option<Receiver<ConnectionResult>>,
    local_tree: LocalTree,
    local_state: TreeState<PathBuf>,
    device_state: TreeState<DeviceItemId>,
    focused_pane: Pane,
    local_marks: HashSet<PathBuf>,
    device_marks: HashSet<u32>,
    local_anchor: Option<PathBuf>,
    device_anchor: Option<DeviceItemId>,
    transfer_receiver: Option<Receiver<TransferEvent>>,
    pending_transfer: Option<TransferJob>,
    transfer_status: Option<String>,
    download_directory: Option<PathBuf>,
    debug_open: bool,
    debug_scroll: usize,
    delete_confirmation: Option<DeleteConfirmation>,
    playlist_state: ListState,
    playlist_track_state: ListState,
    playlist_focus: PlaylistPane,
    playlist_delete_marks: HashSet<u32>,
    playlist_track_marks: HashSet<u32>,
    active_playlist: Option<u32>,
    playlist_name_input: Option<String>,
    playlist_confirmation: Option<PlaylistConfirmation>,
    add_to_device_state: TreeState<DeviceItemId>,
    add_to_marks: HashSet<u32>,
    add_to_focus: PlaylistPane,
    help_open: bool,
    persisted: PersistedState,
    settings_state: ListState,
}

impl App {
    fn new() -> Result<Self> {
        let persisted = PersistedState::load();
        palette::apply(persisted.theme);
        let local_tree = LocalTree::home()?;
        let mut local_state = TreeState::default();
        let root = local_tree.root_path();
        local_state.select(vec![root.clone()]);
        local_state.open(vec![root]);

        Ok(Self {
            screen: Screen::Splash {
                continue_pressed: false,
            },
            connection: Connection::Loading("Connecting..."),
            receiver: None,
            local_tree,
            local_state,
            device_state: TreeState::default(),
            focused_pane: Pane::Local,
            local_marks: HashSet::new(),
            device_marks: HashSet::new(),
            local_anchor: None,
            device_anchor: None,
            transfer_receiver: None,
            pending_transfer: None,
            transfer_status: None,
            download_directory: None,
            debug_open: false,
            debug_scroll: 0,
            delete_confirmation: None,
            playlist_state: ListState::default().with_selected(Some(0)),
            playlist_track_state: ListState::default(),
            playlist_focus: PlaylistPane::Playlists,
            playlist_delete_marks: HashSet::new(),
            playlist_track_marks: HashSet::new(),
            active_playlist: None,
            playlist_name_input: None,
            playlist_confirmation: None,
            add_to_device_state: TreeState::default(),
            add_to_marks: HashSet::new(),
            add_to_focus: PlaylistPane::Playlists,
            help_open: false,
            settings_state: ListState::default().with_selected(Some(
                Theme::ALL
                    .iter()
                    .position(|theme| *theme == persisted.theme)
                    .unwrap_or(0),
            )),
            persisted,
        })
    }

    fn collect_connection_result(&mut self) -> bool {
        if let Some(result) = self
            .receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            self.receiver = None;
            self.connection = match result {
                Ok(snapshot) => {
                    self.restore_active_playlist(&snapshot);
                    Connection::Connected(snapshot)
                }
                Err(_) => Connection::NotFound,
            };
            if matches!(
                self.screen,
                Screen::Splash {
                    continue_pressed: true
                }
            ) {
                self.screen = Screen::Playlists;
            }
            true
        } else {
            false
        }
    }

    fn start_connection(&mut self, message: &'static str) {
        debug_log::log(format!("connection attempt started: {message}"));
        self.receiver = Some(spawn_connection_attempt());
        self.connection = Connection::Loading(message);
    }

    fn enter_files(&mut self) {
        if !matches!(self.screen, Screen::Files) {
            self.clear_marks();
        }
        self.screen = Screen::Files;
        debug_log::log("screen switched: File Management");
    }

    fn clear_marks(&mut self) {
        self.local_marks.clear();
        self.device_marks.clear();
        self.local_anchor = None;
        self.device_anchor = None;
    }

    fn continue_from_splash(&mut self) {
        if matches!(self.connection, Connection::Loading(_)) {
            self.screen = Screen::Splash {
                continue_pressed: true,
            };
        } else {
            self.screen = Screen::Playlists;
        }
    }

    fn handle_file_key(&mut self, key: KeyEvent) {
        let code = key.code;
        if code == KeyCode::Tab {
            self.focused_pane = match self.focused_pane {
                Pane::Local => Pane::Device,
                Pane::Device => Pane::Local,
            };
            debug_log::log(match self.focused_pane {
                Pane::Local => "file pane focus: local",
                Pane::Device => "file pane focus: device",
            });
            return;
        }
        if code == KeyCode::Char('c') {
            self.clear_marks();
            self.transfer_status = Some("Cleared all marks.".to_owned());
            return;
        }
        if code == KeyCode::Char(' ') {
            if key
                .modifiers
                .contains(crossterm::event::KeyModifiers::SHIFT)
            {
                self.mark_range();
            } else {
                self.toggle_mark();
            }
            return;
        }
        if code == KeyCode::Delete {
            self.request_delete();
            return;
        }
        let focused_has_marks = match self.focused_pane {
            Pane::Local => !self.local_marks.is_empty(),
            Pane::Device => !self.device_marks.is_empty(),
        };
        if code == KeyCode::Enter && focused_has_marks {
            self.start_transfer();
            return;
        }

        match self.focused_pane {
            Pane::Local => match code {
                KeyCode::Up => {
                    self.local_state.key_up();
                }
                KeyCode::Down => {
                    self.local_state.key_down();
                }
                KeyCode::Left => {
                    self.local_state.key_left();
                }
                KeyCode::Right => {
                    self.load_selected_local_directory();
                    self.local_state.key_right();
                }
                KeyCode::Enter => {
                    self.load_selected_local_directory();
                    self.local_state.toggle_selected();
                }
                _ => {}
            },
            Pane::Device => match code {
                KeyCode::Up => {
                    self.device_state.key_up();
                }
                KeyCode::Down => {
                    self.device_state.key_down();
                }
                KeyCode::Left => {
                    self.device_state.key_left();
                }
                KeyCode::Right => {
                    self.device_state.key_right();
                }
                KeyCode::Enter => {
                    self.device_state.toggle_selected();
                }
                _ => {}
            },
        }
    }

    fn enter_playlists(&mut self) {
        self.screen = Screen::Playlists;
        self.playlist_state.select(Some(0));
        self.playlist_track_state.select(None);
        self.playlist_track_marks.clear();
        debug_log::log("screen switched: CRUD Playlist");
    }

    fn selected_playlist(&self) -> Option<&Playlist> {
        let index = self.playlist_state.selected()?.checked_sub(1)?;
        self.connected_snapshot()?.playlists.get(index)
    }

    fn active_playlist(&self) -> Option<&Playlist> {
        let active = self.active_playlist?;
        self.connected_snapshot()?
            .playlists
            .iter()
            .find(|playlist| playlist.id == active)
    }

    fn restore_active_playlist(&mut self, snapshot: &DeviceSnapshot) {
        self.active_playlist = self.persisted.active_playlist_id.filter(|id| {
            snapshot.playlists.iter().any(|playlist| {
                playlist.id == *id
                    && self
                        .persisted
                        .active_playlist_name
                        .as_deref()
                        .is_none_or(|name| playlist.name == name)
            })
        });
        if self.active_playlist.is_none() && self.persisted.active_playlist_id.is_some() {
            self.persisted.active_playlist_id = None;
            self.persisted.active_playlist_name = None;
            self.save_persisted_state();
        }
    }

    fn set_active_playlist(&mut self, playlist: &Playlist) {
        self.active_playlist = Some(playlist.id);
        self.persisted.active_playlist_id = Some(playlist.id);
        self.persisted.active_playlist_name = Some(playlist.name.clone());
        self.save_persisted_state();
    }

    fn save_persisted_state(&mut self) {
        if let Err(error) = self.persisted.save() {
            debug_log::log(format!("could not persist app state: {error}"));
            self.transfer_status = Some(format!("Could not save settings: {error}"));
        }
    }

    fn handle_playlist_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Tab {
            self.playlist_focus = match self.playlist_focus {
                PlaylistPane::Playlists => PlaylistPane::Tracks,
                PlaylistPane::Tracks => PlaylistPane::Playlists,
            };
            return;
        }
        match self.playlist_focus {
            PlaylistPane::Playlists => self.handle_playlist_list_key(key.code),
            PlaylistPane::Tracks => self.handle_playlist_track_key(key.code),
        }
    }

    fn handle_playlist_list_key(&mut self, code: KeyCode) {
        let length = self
            .connected_snapshot()
            .map_or(1, |snapshot| snapshot.playlists.len() + 1);
        match code {
            KeyCode::Up => {
                self.playlist_state.select_previous();
                self.playlist_track_state.select(None);
                self.playlist_track_marks.clear();
            }
            KeyCode::Down => {
                self.playlist_state.select_next();
                if self
                    .playlist_state
                    .selected()
                    .is_some_and(|index| index >= length)
                {
                    self.playlist_state.select(Some(length - 1));
                }
                self.playlist_track_state.select(None);
                self.playlist_track_marks.clear();
            }
            KeyCode::Enter if self.playlist_state.selected() == Some(0) => {
                self.playlist_name_input = Some(String::new());
            }
            KeyCode::Char(' ') => {
                if let Some(id) = self.selected_playlist().map(|playlist| playlist.id)
                    && !self.playlist_delete_marks.remove(&id)
                {
                    self.playlist_delete_marks.insert(id);
                }
            }
            KeyCode::Char('a') => {
                if let Some(playlist) = self.selected_playlist().cloned() {
                    self.set_active_playlist(&playlist);
                }
            }
            KeyCode::Delete if !self.playlist_delete_marks.is_empty() => {
                let playlists = self.connected_snapshot().map_or_else(Vec::new, |snapshot| {
                    snapshot
                        .playlists
                        .iter()
                        .filter(|playlist| self.playlist_delete_marks.contains(&playlist.id))
                        .cloned()
                        .collect()
                });
                if !playlists.is_empty() {
                    self.playlist_confirmation =
                        Some(PlaylistConfirmation::DeletePlaylists(playlists));
                }
            }
            _ => {}
        }
    }

    fn handle_playlist_track_key(&mut self, code: KeyCode) {
        let playlist = self.selected_playlist().cloned();
        self.handle_playlist_tracks_for(code, playlist);
    }

    fn handle_playlist_tracks_for(&mut self, code: KeyCode, playlist: Option<Playlist>) {
        let track_ids: Vec<_> = playlist
            .as_ref()
            .map(|playlist| playlist.track_ids.clone())
            .unwrap_or_default();
        let tracks: Vec<_> = track_ids
            .iter()
            .filter_map(|id| {
                self.connected_snapshot()?
                    .tracks
                    .iter()
                    .find(|track| track.id == *id)
            })
            .collect();
        let length = tracks.len();
        match code {
            KeyCode::Up => self.playlist_track_state.select_previous(),
            KeyCode::Down if length != 0 => {
                self.playlist_track_state.select_next();
                if self
                    .playlist_track_state
                    .selected()
                    .is_some_and(|index| index >= length)
                {
                    self.playlist_track_state.select(Some(length - 1));
                }
            }
            KeyCode::Char(' ') => {
                let id = self
                    .playlist_track_state
                    .selected()
                    .and_then(|index| tracks.get(index))
                    .map(|track| track.id);
                if let Some(id) = id
                    && !self.playlist_track_marks.remove(&id)
                {
                    self.playlist_track_marks.insert(id);
                }
            }
            KeyCode::Delete if !self.playlist_track_marks.is_empty() => {
                if let Some(playlist) = playlist {
                    let track_ids = playlist
                        .track_ids
                        .iter()
                        .filter(|id| self.playlist_track_marks.contains(id))
                        .copied()
                        .collect();
                    self.playlist_confirmation = Some(PlaylistConfirmation::RemoveTracks {
                        playlist,
                        track_ids,
                    });
                }
            }
            _ => {}
        }
    }

    fn enter_add_to_playlist(&mut self) {
        self.screen = Screen::AddToPlaylist;
        self.add_to_focus = PlaylistPane::Playlists;
        self.add_to_marks.clear();
        self.playlist_track_marks.clear();
        self.playlist_track_state.select(None);
        debug_log::log("screen switched: ADD TO Playlist");
    }

    fn enter_settings(&mut self) {
        self.screen = Screen::Settings;
        debug_log::log("screen switched: Settings");
    }

    fn handle_settings_key(&mut self, code: KeyCode) {
        let current = self.settings_state.selected().unwrap_or(0);
        let next = match code {
            KeyCode::Up => current.saturating_sub(1),
            KeyCode::Down => (current + 1).min(Theme::ALL.len() - 1),
            KeyCode::Enter => current,
            _ => return,
        };
        self.settings_state.select(Some(next));
        self.persisted.theme = Theme::ALL[next];
        palette::apply(self.persisted.theme);
        self.save_persisted_state();
        self.transfer_status = Some(format!("Theme: {}", self.persisted.theme.name()));
    }

    fn handle_add_to_playlist_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Tab {
            self.add_to_focus = match self.add_to_focus {
                PlaylistPane::Playlists => PlaylistPane::Tracks,
                PlaylistPane::Tracks => PlaylistPane::Playlists,
            };
            return;
        }
        if self.add_to_focus == PlaylistPane::Tracks {
            self.handle_playlist_tracks_for(key.code, self.active_playlist().cloned());
            return;
        }
        match key.code {
            KeyCode::Up => {
                self.add_to_device_state.key_up();
            }
            KeyCode::Down => {
                self.add_to_device_state.key_down();
            }
            KeyCode::Left => {
                self.add_to_device_state.key_left();
            }
            KeyCode::Right => {
                self.add_to_device_state.key_right();
            }
            KeyCode::Char(' ') => self.toggle_add_to_mark(),
            KeyCode::Enter => self.add_marked_tracks_to_active_playlist(),
            _ => {}
        }
    }

    fn toggle_add_to_mark(&mut self) {
        let Some(item) = self.add_to_device_state.selected().last().copied() else {
            return;
        };
        match item {
            DeviceItemId::Track(id) => {
                if !self.add_to_marks.remove(&id) {
                    self.add_to_marks.insert(id);
                }
            }
            DeviceItemId::Folder(id) => {
                let Some(album) = self
                    .connected_snapshot()
                    .and_then(|snapshot| physical_album(snapshot, id))
                else {
                    self.transfer_status =
                        Some("Only device albums and tracks can be marked.".to_owned());
                    return;
                };
                toggle_track_group(&mut self.add_to_marks, &album.track_ids);
            }
        }
    }

    fn add_marked_tracks_to_active_playlist(&mut self) {
        let Some(playlist) = self.active_playlist().cloned() else {
            self.transfer_status = Some(
                "No active playlist — set one with 'a' on the CRUD Playlist screen.".to_owned(),
            );
            return;
        };
        if self.add_to_marks.is_empty() {
            self.transfer_status = Some("Mark one or more tracks to add.".to_owned());
            return;
        }
        let track_ids = self.connected_snapshot().map_or_else(Vec::new, |snapshot| {
            snapshot
                .tracks
                .iter()
                .filter(|track| self.add_to_marks.contains(&track.id))
                .map(|track| track.id)
                .collect()
        });
        self.pending_transfer = Some(TransferJob::AddPlaylistTracks {
            playlist,
            track_ids,
        });
        self.transfer_status = Some("Adding tracks to playlist...".to_owned());
    }

    fn handle_playlist_name_input(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => {
                let Some(name) = self.playlist_name_input.take() else {
                    return;
                };
                let name = name.trim().to_owned();
                if name.is_empty() {
                    self.transfer_status = Some("Playlist name cannot be empty.".to_owned());
                    return;
                }
                let storage_id = self.connected_snapshot().and_then(snapshot_storage_id);
                if let Some(storage_id) = storage_id {
                    self.pending_transfer = Some(TransferJob::CreatePlaylist { name, storage_id });
                    self.transfer_status = Some("Creating playlist...".to_owned());
                } else {
                    self.transfer_status = Some("No Zune storage is available.".to_owned());
                }
            }
            KeyCode::Esc => self.playlist_name_input = None,
            KeyCode::Backspace => {
                if let Some(input) = &mut self.playlist_name_input {
                    input.pop();
                }
            }
            KeyCode::Char(character) => {
                if let Some(input) = &mut self.playlist_name_input {
                    input.push(character);
                }
            }
            _ => {}
        }
    }

    fn handle_playlist_confirmation(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let Some(confirmation) = self.playlist_confirmation.take() else {
                    return;
                };
                self.pending_transfer = Some(match confirmation {
                    PlaylistConfirmation::DeletePlaylists(playlists) => {
                        TransferJob::DeletePlaylists { playlists }
                    }
                    PlaylistConfirmation::RemoveTracks {
                        playlist,
                        track_ids,
                    } => TransferJob::RemovePlaylistTracks {
                        playlist,
                        track_ids,
                    },
                });
                self.transfer_status = Some("Updating playlists...".to_owned());
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.playlist_confirmation = None;
                self.transfer_status = Some("Playlist change cancelled.".to_owned());
            }
            _ => {}
        }
    }

    fn load_selected_local_directory(&mut self) {
        if let Some(path) = self.local_state.selected().last().cloned() {
            self.local_tree.load(&path);
        }
    }

    fn toggle_mark(&mut self) {
        match self.focused_pane {
            Pane::Local => {
                let Some(path) = self.local_state.selected().last().cloned() else {
                    return;
                };
                if path.is_file() {
                    if !self.local_marks.remove(&path) {
                        self.local_marks.insert(path.clone());
                    }
                } else if let Some(files) = browser::album_audio_files(&path) {
                    if files.iter().all(|file| self.local_marks.contains(file)) {
                        for file in files {
                            self.local_marks.remove(&file);
                        }
                    } else {
                        self.local_marks.extend(files);
                    }
                } else {
                    self.transfer_status = Some(
                        "Only files and Letter/Artist/Album folders can be marked.".to_owned(),
                    );
                    return;
                }
                self.local_anchor = Some(path);
            }
            Pane::Device => {
                let Some(item) = self.device_state.selected().last().copied() else {
                    return;
                };
                match item {
                    DeviceItemId::Track(id) => {
                        if !self.device_marks.remove(&id) {
                            self.device_marks.insert(id);
                        }
                    }
                    DeviceItemId::Folder(id) => {
                        let Some(album) = self
                            .connected_snapshot()
                            .and_then(|snapshot| physical_album(snapshot, id))
                        else {
                            self.transfer_status =
                                Some("Only device albums and tracks can be marked.".to_owned());
                            return;
                        };
                        let album = album.clone();
                        toggle_track_group(&mut self.device_marks, &album.track_ids);
                    }
                }
                self.device_anchor = Some(item);
            }
        }
    }

    fn mark_range(&mut self) {
        match self.focused_pane {
            Pane::Local => {
                let Some(anchor) = self.local_anchor.clone() else {
                    self.toggle_mark();
                    return;
                };
                let Some(current) = self.local_state.selected().last().cloned() else {
                    return;
                };
                let items = self.local_tree.items(&self.local_marks);
                let visible: Vec<_> = self
                    .local_state
                    .flatten(&items)
                    .into_iter()
                    .filter_map(|item| item.identifier.last().cloned())
                    .collect();
                mark_path_range(&mut self.local_marks, &visible, &anchor, &current);
            }
            Pane::Device => {
                let Some(anchor) = self.device_anchor else {
                    self.toggle_mark();
                    return;
                };
                let Some(current) = self.device_state.selected().last().copied() else {
                    return;
                };
                let Some(snapshot) = self.connected_snapshot() else {
                    return;
                };
                let items = folder_items(
                    &snapshot.folders,
                    &snapshot.tracks,
                    &snapshot.albums,
                    &self.device_marks,
                );
                let visible: Vec<_> = self
                    .device_state
                    .flatten(&items)
                    .into_iter()
                    .filter_map(|item| item.identifier.last().copied())
                    .collect();
                let physical_albums = physical_albums(snapshot);
                mark_track_range(
                    &mut self.device_marks,
                    &visible,
                    anchor,
                    current,
                    &physical_albums,
                );
            }
        }
    }

    fn connected_snapshot(&self) -> Option<&DeviceSnapshot> {
        match &self.connection {
            Connection::Connected(snapshot) => Some(snapshot),
            _ => None,
        }
    }

    fn request_delete(&mut self) {
        if self.focused_pane != Pane::Device
            || self.device_marks.is_empty()
            || self.transfer_receiver.is_some()
        {
            return;
        }
        let Some(snapshot) = self.connected_snapshot() else {
            return;
        };
        let scope = delete_scope(snapshot, &self.device_marks);
        if scope.items.is_empty() {
            return;
        }
        let device_name = if snapshot.info.friendly_name.is_empty() {
            snapshot.info.model.clone()
        } else {
            snapshot.info.friendly_name.clone()
        };
        self.delete_confirmation = Some(DeleteConfirmation {
            device_name,
            items: scope.items,
            track_count: scope.track_count,
            album_count: scope.album_count,
            artist_names: scope.artist_names,
        });
    }

    fn handle_delete_confirmation(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let Some(confirmation) = self.delete_confirmation.take() else {
                    return;
                };
                let count = confirmation.items.len();
                let device_name = confirmation.device_name;
                self.pending_transfer = Some(TransferJob::Delete {
                    items: confirmation.items,
                });
                self.transfer_status = Some(format!(
                    "Deleting {} item(s) from {}'s Zune...",
                    count, device_name
                ));
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.delete_confirmation = None;
                self.transfer_status = Some("Delete cancelled.".to_owned());
            }
            _ => {}
        }
    }

    fn start_transfer(&mut self) {
        if self.transfer_receiver.is_some() {
            self.transfer_status = Some("A transfer is already in progress.".to_owned());
            return;
        }
        let job = match self.focused_pane {
            Pane::Local => {
                let Some(DeviceItemId::Folder(folder_id)) =
                    self.device_state.selected().last().copied()
                else {
                    self.transfer_status =
                        Some("Select a destination folder in the Zune pane.".to_owned());
                    return;
                };
                let Some(snapshot) = self.connected_snapshot() else {
                    self.transfer_status = Some(NOT_FOUND_SHORT.to_owned());
                    return;
                };
                let Some(folder) = find_folder(&snapshot.folders, folder_id) else {
                    self.transfer_status =
                        Some("The selected Zune folder is unavailable.".to_owned());
                    return;
                };
                TransferJob::Upload {
                    files: self.local_marks.iter().cloned().collect(),
                    folder_id,
                    storage_id: folder.storage_id,
                }
            }
            Pane::Device => {
                let Some(directory) = self.local_state.selected().last().cloned() else {
                    self.transfer_status = Some("Select a local destination folder.".to_owned());
                    return;
                };
                if !directory.is_dir() {
                    self.transfer_status = Some("Select a local destination folder.".to_owned());
                    return;
                }
                let Some(snapshot) = self.connected_snapshot() else {
                    self.transfer_status = Some(NOT_FOUND_SHORT.to_owned());
                    return;
                };
                let tracks: Vec<_> = snapshot
                    .tracks
                    .iter()
                    .filter(|track| self.device_marks.contains(&track.id))
                    .cloned()
                    .collect();
                self.download_directory = Some(directory.clone());
                TransferJob::Download { tracks, directory }
            }
        };
        self.transfer_receiver = Some(spawn_transfer(job));
        self.transfer_status = Some("Starting transfer...".to_owned());
    }

    fn collect_transfer_events(&mut self) -> bool {
        let mut changed = false;
        while let Some(event) = self
            .transfer_receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            changed = true;
            match event {
                TransferEvent::Progress {
                    current,
                    total,
                    name,
                    verb,
                } => {
                    self.transfer_status =
                        Some(format!("{verb} {current:02}/{total:02}: {name}..."));
                }
                TransferEvent::Finished {
                    completed,
                    failures,
                    snapshot,
                    verb,
                } => {
                    self.transfer_receiver = None;
                    if let Some(snapshot) = snapshot {
                        if self.active_playlist.is_some_and(|active| {
                            !snapshot
                                .playlists
                                .iter()
                                .any(|playlist| playlist.id == active)
                        }) {
                            self.active_playlist = None;
                            self.persisted.active_playlist_id = None;
                            self.persisted.active_playlist_name = None;
                            self.save_persisted_state();
                        }
                        self.connection = Connection::Connected(snapshot);
                    }
                    if let Some(directory) = self.download_directory.take() {
                        self.local_tree.refresh(&directory);
                    }
                    self.clear_marks();
                    self.playlist_delete_marks.clear();
                    self.playlist_track_marks.clear();
                    self.add_to_marks.clear();
                    self.transfer_status = Some(if failures.is_empty() {
                        format!("{verb} {completed} item(s) successfully.")
                    } else {
                        format!("{verb} {completed}; failures: {}", failures.join(" | "))
                    });
                }
            }
        }
        changed
    }
}

fn spawn_connection_attempt() -> Receiver<ConnectionResult> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        debug_log::log("worker: opening MTP device");
        let result = mtp::open_device().map(|device| device.snapshot());
        match &result {
            Ok(snapshot) => debug_log::log(format!(
                "connection succeeded: friendly_name={:?}, model={:?}",
                snapshot.info.friendly_name, snapshot.info.model
            )),
            Err(error) => debug_log::log(format!("connection failed: {error}")),
        }
        let _ = sender.send(result);
    });
    receiver
}

fn spawn_transfer(job: TransferJob) -> Receiver<TransferEvent> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let total = match &job {
            TransferJob::Upload { files, .. } => files.len(),
            TransferJob::Download { tracks, .. } => tracks.len(),
            TransferJob::Delete { items } => items.len(),
            TransferJob::CreatePlaylist { .. } => 1,
            TransferJob::DeletePlaylists { playlists } => playlists.len(),
            TransferJob::RemovePlaylistTracks { track_ids, .. } => track_ids.len(),
            TransferJob::AddPlaylistTracks { track_ids, .. } => track_ids.len(),
        };
        let device = match mtp::open_device() {
            Ok(device) => device,
            Err(error) => {
                let _ = sender.send(TransferEvent::Finished {
                    completed: 0,
                    failures: vec![error.to_string()],
                    snapshot: None,
                    verb: "Processed",
                });
                return;
            }
        };

        let mut completed = 0;
        let mut failures = Vec::new();
        let finished_verb = match job {
            TransferJob::Upload {
                files,
                folder_id,
                storage_id,
            } => {
                let mut folders = device.folders();
                let mut tracks = device.tracks();
                for (index, path) in files.into_iter().enumerate() {
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let _ = sender.send(TransferEvent::Progress {
                        current: index + 1,
                        total,
                        name: name.clone(),
                        verb: "Copying",
                    });
                    match device.upload_track_resolved(
                        &path,
                        folder_id,
                        storage_id,
                        &mut folders,
                        &mut tracks,
                    ) {
                        Ok(UploadResult::Uploaded) => completed += 1,
                        Ok(UploadResult::SkippedDuplicate) => failures.push(format!(
                            "{name}: skipped (same title already exists in destination)"
                        )),
                        Err(error) => failures.push(format!("{name}: {error}")),
                    }
                }
                "Copied"
            }
            TransferJob::Download { tracks, directory } => {
                for (index, track) in tracks.into_iter().enumerate() {
                    let name = safe_download_name(&track);
                    let _ = sender.send(TransferEvent::Progress {
                        current: index + 1,
                        total,
                        name: name.clone(),
                        verb: "Copying",
                    });
                    match device.download_track(&track, &directory.join(&name)) {
                        Ok(()) => completed += 1,
                        Err(error) => failures.push(format!("{name}: {error}")),
                    }
                }
                "Copied"
            }
            TransferJob::Delete { items } => {
                for (index, item) in items.into_iter().enumerate() {
                    let _ = sender.send(TransferEvent::Progress {
                        current: index + 1,
                        total,
                        name: item.name.clone(),
                        verb: "Deleting",
                    });
                    match device.delete_object(item.object_id, &item.name) {
                        Ok(()) => completed += 1,
                        Err(error) => failures.push(format!("{}: {error}", item.name)),
                    }
                }
                "Deleted"
            }
            TransferJob::CreatePlaylist { name, storage_id } => {
                let _ = sender.send(TransferEvent::Progress {
                    current: 1,
                    total,
                    name: name.clone(),
                    verb: "Creating",
                });
                match device.create_playlist(&name, storage_id) {
                    Ok(()) => completed = 1,
                    Err(error) => failures.push(format!("{name}: {error}")),
                }
                "Created"
            }
            TransferJob::DeletePlaylists { playlists } => {
                for (index, playlist) in playlists.into_iter().enumerate() {
                    let _ = sender.send(TransferEvent::Progress {
                        current: index + 1,
                        total,
                        name: playlist.name.clone(),
                        verb: "Deleting",
                    });
                    match device.delete_object(playlist.id, &playlist.name) {
                        Ok(()) => completed += 1,
                        Err(error) => failures.push(format!("{}: {error}", playlist.name)),
                    }
                }
                "Deleted"
            }
            TransferJob::RemovePlaylistTracks {
                playlist,
                track_ids,
            } => {
                let retained: Vec<_> = playlist
                    .track_ids
                    .iter()
                    .filter(|id| !track_ids.contains(id))
                    .copied()
                    .collect();
                let _ = sender.send(TransferEvent::Progress {
                    current: 1,
                    total: 1,
                    name: playlist.name.clone(),
                    verb: "Updating",
                });
                match device.update_playlist_tracks(&playlist, &retained) {
                    Ok(()) => completed = track_ids.len(),
                    Err(error) => failures.push(format!("{}: {error}", playlist.name)),
                }
                "Removed"
            }
            TransferJob::AddPlaylistTracks {
                playlist,
                track_ids,
            } => {
                let (updated, added) = append_unique_track_ids(&playlist.track_ids, &track_ids);
                let _ = sender.send(TransferEvent::Progress {
                    current: added,
                    total,
                    name: playlist.name.clone(),
                    verb: "Adding",
                });
                if added == 0 {
                    completed = 0;
                } else {
                    match device.update_playlist_tracks(&playlist, &updated) {
                        Ok(()) => completed = added,
                        Err(error) => failures.push(format!("{}: {error}", playlist.name)),
                    }
                }
                "Added"
            }
        };
        let snapshot = Some(device.snapshot());
        let _ = sender.send(TransferEvent::Finished {
            completed,
            failures,
            snapshot,
            verb: finished_verb,
        });
    });
    receiver
}

fn safe_download_name(track: &Track) -> String {
    std::path::Path::new(&track.filename)
        .file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("track-{}", track.id))
}

fn append_unique_track_ids(existing: &[u32], additions: &[u32]) -> (Vec<u32>, usize) {
    let mut updated = existing.to_vec();
    let mut seen: HashSet<_> = existing.iter().copied().collect();
    for track_id in additions {
        if seen.insert(*track_id) {
            updated.push(*track_id);
        }
    }
    let added = updated.len() - existing.len();
    (updated, added)
}

fn mark_path_range(
    marks: &mut HashSet<PathBuf>,
    visible: &[PathBuf],
    anchor: &PathBuf,
    current: &PathBuf,
) {
    let Some(start) = visible.iter().position(|item| item == anchor) else {
        return;
    };
    let Some(end) = visible.iter().position(|item| item == current) else {
        return;
    };
    for path in &visible[start.min(end)..=start.max(end)] {
        if path.is_file() {
            marks.insert(path.clone());
        } else if let Some(files) = browser::album_audio_files(path) {
            marks.extend(files);
        }
    }
}

fn mark_track_range(
    marks: &mut HashSet<u32>,
    visible: &[DeviceItemId],
    anchor: DeviceItemId,
    current: DeviceItemId,
    albums: &[PhysicalAlbum],
) {
    let Some(start) = visible.iter().position(|item| *item == anchor) else {
        return;
    };
    let Some(end) = visible.iter().position(|item| *item == current) else {
        return;
    };
    for item in &visible[start.min(end)..=start.max(end)] {
        match item {
            DeviceItemId::Track(id) => {
                marks.insert(*id);
            }
            DeviceItemId::Folder(id) => {
                if let Some(album) = albums.iter().find(|album| album.folder_id == *id) {
                    marks.extend(album.track_ids.iter().copied());
                }
            }
        }
    }
}

fn find_folder(folders: &[Folder], id: u32) -> Option<&Folder> {
    folders.iter().find_map(|folder| {
        (folder.id == id)
            .then_some(folder)
            .or_else(|| find_folder(&folder.children, id))
    })
}

fn snapshot_storage_id(snapshot: &DeviceSnapshot) -> Option<u32> {
    snapshot
        .playlists
        .first()
        .map(|playlist| playlist.storage_id)
        .or_else(|| snapshot.tracks.first().map(|track| track.storage_id))
        .or_else(|| first_folder_storage_id(&snapshot.folders))
}

fn first_folder_storage_id(folders: &[Folder]) -> Option<u32> {
    folders.first().map(|folder| folder.storage_id).or_else(|| {
        folders
            .iter()
            .find_map(|folder| first_folder_storage_id(&folder.children))
    })
}

fn album_is_fully_marked(album: &Album, marks: &HashSet<u32>) -> bool {
    !album.track_ids.is_empty()
        && album
            .track_ids
            .iter()
            .all(|track_id| marks.contains(track_id))
}

fn group_is_fully_marked(track_ids: &[u32], marks: &HashSet<u32>) -> bool {
    !track_ids.is_empty() && track_ids.iter().all(|track_id| marks.contains(track_id))
}

fn toggle_track_group(marks: &mut HashSet<u32>, track_ids: &[u32]) {
    if group_is_fully_marked(track_ids, marks) {
        for track_id in track_ids {
            marks.remove(track_id);
        }
    } else {
        marks.extend(track_ids.iter().copied());
    }
}

fn physical_albums(snapshot: &DeviceSnapshot) -> Vec<PhysicalAlbum> {
    let mut albums = Vec::new();
    for music in snapshot
        .folders
        .iter()
        .filter(|folder| folder.name.eq_ignore_ascii_case("Music"))
    {
        for artist in &music.children {
            for album in &artist.children {
                let track_ids: Vec<_> = snapshot
                    .tracks
                    .iter()
                    .filter(|track| track.parent_id == album.id)
                    .map(|track| track.id)
                    .collect();
                if !track_ids.is_empty() {
                    albums.push(PhysicalAlbum {
                        folder_id: album.id,
                        name: album.name.clone(),
                        artist_id: artist.id,
                        artist_name: artist.name.clone(),
                        track_ids,
                        artist_album_count: artist.children.len(),
                    });
                }
            }
        }
    }
    albums
}

fn physical_album(snapshot: &DeviceSnapshot, folder_id: u32) -> Option<PhysicalAlbum> {
    physical_albums(snapshot)
        .into_iter()
        .find(|album| album.folder_id == folder_id)
}

struct DeleteScope {
    items: Vec<DeleteItem>,
    track_count: usize,
    album_count: usize,
    artist_names: Vec<String>,
}

fn delete_scope(snapshot: &DeviceSnapshot, marks: &HashSet<u32>) -> DeleteScope {
    let mut items: Vec<_> = snapshot
        .albums
        .iter()
        .filter(|album| album_is_fully_marked(album, marks))
        .map(|album| DeleteItem {
            object_id: album.id,
            name: format!("album {}", album.name),
        })
        .collect();
    let selected_albums: Vec<_> = physical_albums(snapshot)
        .into_iter()
        .filter(|album| group_is_fully_marked(&album.track_ids, marks))
        .collect();
    items.extend(
        snapshot
            .tracks
            .iter()
            .filter(|track| marks.contains(&track.id))
            .map(|track| DeleteItem {
                object_id: track.id,
                name: track.name.clone(),
            }),
    );
    items.extend(selected_albums.iter().map(|album| DeleteItem {
        object_id: album.folder_id,
        name: format!("album folder {}", album.name),
    }));

    let mut artist_names = Vec::new();
    let mut artist_ids = HashSet::new();
    for album in &selected_albums {
        let selected_for_artist = selected_albums
            .iter()
            .filter(|candidate| candidate.artist_id == album.artist_id)
            .count();
        if selected_for_artist == album.artist_album_count && artist_ids.insert(album.artist_id) {
            artist_names.push(album.artist_name.clone());
            items.push(DeleteItem {
                object_id: album.artist_id,
                name: format!("artist folder {}", album.artist_name),
            });
        }
    }
    artist_names.sort_by_key(|name| artist_sort_key(name));
    DeleteScope {
        items,
        track_count: snapshot
            .tracks
            .iter()
            .filter(|track| marks.contains(&track.id))
            .count(),
        album_count: selected_albums.len(),
        artist_names,
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;
    debug_log::init()?;
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = run(&mut terminal);
    disable_raw_mode()?;
    stdout_guard::synchronized(|| -> Result<()> {
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;
        Ok(())
    })?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    let mut app = App::new()?;
    stdout_guard::synchronized(|| terminal.draw(|frame| draw(frame, &mut app)))?;
    app.start_connection("Connecting...");
    let mut needs_draw = true;

    loop {
        needs_draw |= app.collect_connection_result();
        needs_draw |= app.collect_transfer_events();

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if app.help_open {
                        if matches!(key.code, KeyCode::Char('?') | KeyCode::Esc) {
                            app.help_open = false;
                        }
                        needs_draw = true;
                        continue;
                    }
                    if key.code == KeyCode::Char('?') {
                        app.help_open = true;
                        needs_draw = true;
                        continue;
                    }
                    if app.playlist_name_input.is_some() {
                        app.handle_playlist_name_input(key.code);
                        needs_draw = true;
                        continue;
                    }
                    if app.playlist_confirmation.is_some() {
                        app.handle_playlist_confirmation(key.code);
                        needs_draw = true;
                        continue;
                    }
                    if app.delete_confirmation.is_some() {
                        app.handle_delete_confirmation(key.code);
                        needs_draw = true;
                        continue;
                    }
                    if should_quit(key) {
                        return Ok(());
                    }
                    if key.code == KeyCode::Char('`') {
                        app.debug_open = !app.debug_open;
                        app.debug_scroll = 0;
                        debug_log::log(if app.debug_open {
                            "debug panel opened"
                        } else {
                            "debug panel closed"
                        });
                        needs_draw = true;
                        continue;
                    }
                    if app.debug_open {
                        match key.code {
                            KeyCode::Up => app.debug_scroll = app.debug_scroll.saturating_add(1),
                            KeyCode::Down => app.debug_scroll = app.debug_scroll.saturating_sub(1),
                            KeyCode::PageUp => {
                                app.debug_scroll = app.debug_scroll.saturating_add(10)
                            }
                            KeyCode::PageDown => {
                                app.debug_scroll = app.debug_scroll.saturating_sub(10)
                            }
                            _ => {}
                        }
                        needs_draw = true;
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('1') => app.enter_files(),
                        KeyCode::Char('2') => app.enter_playlists(),
                        KeyCode::Char('3') => app.enter_add_to_playlist(),
                        KeyCode::Char('4') => app.enter_settings(),
                        _ => match app.screen {
                            Screen::Splash { .. } => app.continue_from_splash(),
                            Screen::Files => app.handle_file_key(key),
                            Screen::Playlists => app.handle_playlist_key(key),
                            Screen::AddToPlaylist => app.handle_add_to_playlist_key(key),
                            Screen::Settings => app.handle_settings_key(key.code),
                        },
                    }
                    needs_draw = true;
                }
                Event::Resize(_, _) => needs_draw = true,
                _ => {}
            }
        }

        if needs_draw
            && let Some(result) =
                stdout_guard::try_synchronized(|| terminal.draw(|frame| draw(frame, &mut app)))
        {
            result?;
            needs_draw = false;
            if let Some(job) = app.pending_transfer.take() {
                app.transfer_receiver = Some(spawn_transfer(job));
            }
        }
    }
}

fn should_quit(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
}

fn draw(frame: &mut Frame, app: &mut App) {
    frame.render_widget(
        Block::default().style(Style::default().bg(palette().background)),
        frame.area(),
    );
    match app.screen {
        Screen::Splash { continue_pressed } => draw_splash(frame, continue_pressed),
        Screen::Files => draw_files(frame, app),
        Screen::Playlists => draw_playlists(frame, app),
        Screen::AddToPlaylist => draw_add_to_playlist(frame, app),
        Screen::Settings => draw_settings(frame, app),
    }
    if let Some(confirmation) = &app.delete_confirmation {
        draw_delete_confirmation(frame, confirmation);
    }
    if let Some(confirmation) = &app.playlist_confirmation {
        draw_playlist_confirmation(frame, confirmation);
    }
    if let Some(input) = &app.playlist_name_input {
        draw_playlist_name_input(frame, input);
    }
    if app.debug_open {
        draw_debug_overlay(frame, app.debug_scroll);
    }
    if app.help_open {
        draw_help(frame);
    }
}

fn draw_delete_confirmation(frame: &mut Frame, confirmation: &DeleteConfirmation) {
    let message = delete_confirmation_message(confirmation);
    let width = u16::try_from(message.chars().count())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centered(frame.area(), width, 3);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(message)
            .alignment(Alignment::Center)
            .style(
                Style::default()
                    .fg(palette().error)
                    .bg(palette().background)
                    .add_modifier(Modifier::BOLD),
            )
            .block(themed_block(" CONFIRM DELETE ", true)),
        area,
    );
}

fn delete_confirmation_message(confirmation: &DeleteConfirmation) -> String {
    let count_noun = |count: usize, singular: &str| {
        format!("{count} {singular}{}", if count == 1 { "" } else { "s" })
    };
    let mut parts = vec![count_noun(confirmation.track_count, "track")];
    if confirmation.album_count != 0 {
        parts.push(count_noun(confirmation.album_count, "album"));
    }
    let has_artists = !confirmation.artist_names.is_empty();
    let mut scope = if parts.len() == 1 {
        parts.remove(0)
    } else if has_artists {
        format!("{}, {}", parts[0], parts[1])
    } else {
        format!("{} and {}", parts[0], parts[1])
    };
    if has_artists {
        let artists = confirmation
            .artist_names
            .iter()
            .map(|name| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(", ");
        if confirmation.artist_names.len() == 1 {
            scope.push_str(&format!(", and the artist folder {artists}"));
        } else {
            scope.push_str(&format!(", and the artist folders {artists}"));
        }
    }
    format!(
        "Delete {scope} from {}'s Zune? [y/N]",
        confirmation.device_name
    )
}

fn draw_debug_overlay(frame: &mut Frame, scroll_from_bottom: usize) {
    let area = frame.area();
    let height = (area.height / 3).max(7).min(area.height);
    let panel = Rect::new(
        area.x,
        area.bottom().saturating_sub(height),
        area.width,
        height,
    );
    let lines = wrap_debug_lines(debug_log::lines(), panel.width.saturating_sub(2) as usize);
    let visible = panel.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible);
    let scroll_from_bottom = scroll_from_bottom.min(max_scroll);
    let end = lines.len().saturating_sub(scroll_from_bottom);
    let start = end.saturating_sub(visible);
    let text = lines[start..end].to_vec();
    frame.render_widget(Clear, panel);
    frame.render_widget(
        Paragraph::new(text)
            .style(
                Style::default()
                    .fg(palette().primary)
                    .bg(palette().background),
            )
            .block(themed_block(" DEBUG LOG — ` close · ↑/↓ scroll ", true)),
        panel,
    );
}

fn wrap_debug_lines(lines: Vec<String>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    lines
        .into_iter()
        .flat_map(|line| {
            let characters: Vec<_> = line.chars().collect();
            if characters.is_empty() {
                return vec![Line::from("")];
            }
            characters
                .chunks(width)
                .map(|chunk| Line::from(chunk.iter().collect::<String>()))
                .collect()
        })
        .collect()
}

fn draw_splash(frame: &mut Frame, continue_pressed: bool) {
    let area = centered(frame.area(), 78, 14);
    frame.render_widget(Clear, area);
    let prompt = if continue_pressed {
        "Connecting to Zune..."
    } else {
        "press any key to continue"
    };
    let logo_style = Style::default()
        .fg(palette().primary)
        .add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'_>> = LOGO
        .lines()
        .map(|line| Line::styled(line, logo_style))
        .collect();
    lines.extend([
        Line::from(""),
        Line::styled("columbia foundry llc", Style::default().fg(palette().muted)),
        Line::from(""),
        Line::styled(prompt, Style::default().fg(palette().accent)),
    ]);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .alignment(Alignment::Center)
            .block(themed_block(" ZUNE TUI ", true)),
        area,
    );
}

fn draw_files(frame: &mut Frame, app: &mut App) {
    let [panes, status] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(frame.area());
    let [local_area, device_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(panes);

    let local_items = app.local_tree.items(&app.local_marks);
    let local_widget = tree_widget(
        &local_items,
        " LOCAL — $HOME ",
        app.focused_pane == Pane::Local,
    );
    frame.render_stateful_widget(local_widget, local_area, &mut app.local_state);

    match &app.connection {
        Connection::Connected(snapshot) => {
            let device_items = folder_items(
                &snapshot.folders,
                &snapshot.tracks,
                &snapshot.albums,
                &app.device_marks,
            );
            let device_widget = tree_widget(
                &device_items,
                " ZUNE DEVICE ",
                app.focused_pane == Pane::Device,
            );
            frame.render_stateful_widget(device_widget, device_area, &mut app.device_state);
        }
        Connection::Loading(message) => draw_device_message(
            frame,
            device_area,
            message,
            app.focused_pane == Pane::Device,
            false,
        ),
        Connection::NotFound => draw_device_message(
            frame,
            device_area,
            NOT_FOUND_SHORT,
            app.focused_pane == Pane::Device,
            true,
        ),
    }

    frame.render_widget(
        Paragraph::new(status_line(app))
            .alignment(Alignment::Center)
            .style(Style::default().bg(palette().background)),
        status,
    );
}

fn draw_playlists(frame: &mut Frame, app: &mut App) {
    let [panes, status] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(frame.area());
    let [playlist_area, track_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(panes);

    match &app.connection {
        Connection::Connected(snapshot) => {
            let mut items = vec![ListItem::new("+ New Playlist")];
            items.extend(snapshot.playlists.iter().map(|playlist| {
                let marked = if app.playlist_delete_marks.contains(&playlist.id) {
                    "[x] "
                } else {
                    "[ ] "
                };
                let active = if app.active_playlist == Some(playlist.id) {
                    Span::styled(
                        "A ",
                        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
                    )
                } else {
                    Span::raw("  ")
                };
                ListItem::new(Line::from(vec![
                    Span::raw(marked),
                    active,
                    Span::raw(&playlist.name),
                ]))
            }));
            let playlist_highlight = if app.playlist_state.selected() == Some(0) {
                Style::default()
                    .fg(palette().primary)
                    .bg(palette().background)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(palette().background)
                    .bg(palette().primary)
                    .add_modifier(Modifier::BOLD)
            };
            let list = List::new(items)
                .block(themed_block(
                    " PLAYLISTS — Enter new · Space mark · a active · Del delete ",
                    app.playlist_focus == PlaylistPane::Playlists,
                ))
                .style(
                    Style::default()
                        .fg(palette().primary)
                        .bg(palette().background),
                )
                .highlight_style(playlist_highlight)
                .highlight_symbol("› ");
            frame.render_stateful_widget(list, playlist_area, &mut app.playlist_state);

            let selected = app
                .playlist_state
                .selected()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| snapshot.playlists.get(index));
            draw_playlist_tracks_pane(
                frame,
                track_area,
                snapshot,
                selected,
                &app.playlist_track_marks,
                &mut app.playlist_track_state,
                app.playlist_focus == PlaylistPane::Tracks,
                None,
            );
        }
        Connection::Loading(message) => {
            draw_device_message(frame, playlist_area, message, true, false);
            draw_device_message(frame, track_area, message, false, false);
        }
        Connection::NotFound => {
            draw_device_message(frame, playlist_area, NOT_FOUND_SHORT, true, true);
            draw_device_message(frame, track_area, NOT_FOUND_SHORT, false, true);
        }
    }
    frame.render_widget(
        Paragraph::new(status_line(app)).alignment(Alignment::Center),
        status,
    );
}

fn draw_playlist_name_input(frame: &mut Frame, input: &str) {
    let area = centered(frame.area(), 60, 3);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(format!("{input}█"))
            .style(
                Style::default()
                    .fg(palette().primary)
                    .bg(palette().background),
            )
            .block(themed_block(
                " NEW PLAYLIST — Enter create · Esc cancel ",
                true,
            )),
        area,
    );
}

fn draw_playlist_confirmation(frame: &mut Frame, confirmation: &PlaylistConfirmation) {
    let message = match confirmation {
        PlaylistConfirmation::DeletePlaylists(playlists) => format!(
            "Delete {} playlist(s): {}? [y/N]",
            playlists.len(),
            playlists
                .iter()
                .map(|playlist| format!("'{}'", playlist.name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        PlaylistConfirmation::RemoveTracks {
            playlist,
            track_ids,
        } => format!(
            "Remove {} track(s) from '{}'? [y/N]",
            track_ids.len(),
            playlist.name
        ),
    };
    let area = centered(frame.area(), 76, 5);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(message)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .style(
                Style::default()
                    .fg(palette().error)
                    .bg(palette().background),
            )
            .block(themed_block(" CONFIRM PLAYLIST CHANGE ", true)),
        area,
    );
}

fn draw_add_to_playlist(frame: &mut Frame, app: &mut App) {
    let [panes, status] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(frame.area());
    let [device_area, track_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(panes);
    match &app.connection {
        Connection::Connected(snapshot) => {
            let device_items = folder_items(
                &snapshot.folders,
                &snapshot.tracks,
                &snapshot.albums,
                &app.add_to_marks,
            );
            frame.render_stateful_widget(
                tree_widget(
                    &device_items,
                    " ZUNE DEVICE — Space mark · Enter add ",
                    app.add_to_focus == PlaylistPane::Playlists,
                ),
                device_area,
                &mut app.add_to_device_state,
            );
            let active = app
                .active_playlist
                .and_then(|id| snapshot.playlists.iter().find(|playlist| playlist.id == id));
            draw_playlist_tracks_pane(
                frame,
                track_area,
                snapshot,
                active,
                &app.playlist_track_marks,
                &mut app.playlist_track_state,
                app.add_to_focus == PlaylistPane::Tracks,
                Some("No active playlist — set one with 'a' on the CRUD Playlist screen"),
            );
        }
        Connection::Loading(message) => {
            draw_device_message(frame, device_area, message, true, false);
            draw_device_message(frame, track_area, message, false, false);
        }
        Connection::NotFound => {
            draw_device_message(frame, device_area, NOT_FOUND_SHORT, true, true);
            draw_device_message(frame, track_area, NOT_FOUND_SHORT, false, true);
        }
    }
    frame.render_widget(
        Paragraph::new(status_line(app)).alignment(Alignment::Center),
        status,
    );
}

fn draw_settings(frame: &mut Frame, app: &mut App) {
    let [main, status] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(frame.area());
    let items = Theme::ALL.iter().map(|theme| ListItem::new(theme.name()));
    let list = List::new(items)
        .block(themed_block(" SETTINGS — THEME ", true))
        .style(
            Style::default()
                .fg(palette().primary)
                .bg(palette().background),
        )
        .highlight_style(
            Style::default()
                .fg(palette().background)
                .bg(palette().primary)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, centered(main, 42, 9), &mut app.settings_state);
    frame.render_widget(
        Paragraph::new(status_line(app)).alignment(Alignment::Center),
        status,
    );
}

fn draw_help(frame: &mut Frame) {
    let area = centered(frame.area(), 100, 23);
    frame.render_widget(Clear, area);
    let heading = Style::default()
        .fg(palette().accent)
        .add_modifier(Modifier::BOLD);
    let key = Style::default()
        .fg(palette().primary)
        .add_modifier(Modifier::BOLD);
    let row = |binding: &'static str, description: &'static str| {
        Line::from(vec![
            Span::styled(format!("  {binding:<14}"), key),
            Span::raw(description),
        ])
    };
    let left = vec![
        Line::styled("Global", heading),
        row(
            "1 / 2 / 3 / 4",
            "File Management / CRUD Playlist / ADD TO Playlist / Settings",
        ),
        row("?", "open or close this help"),
        row("`", "open or close the debug log"),
        row(
            "q / Esc",
            "quit (Esc cancels an open dialog or closes Help)",
        ),
        Line::from(""),
        Line::styled("File Management", heading),
        row("Tab", "swap pane focus"),
        row("Arrows", "navigate folders and files"),
        row("Space", "mark a file, track, or recognized album"),
        row("Shift+Space", "mark a visible range"),
        row(
            "Enter",
            "copy marked items to the other pane; otherwise open folder",
        ),
        row("c", "clear all marks"),
        row("Del", "delete marked device items with confirmation"),
    ];
    let right = vec![
        Line::styled("CRUD Playlist", heading),
        row("Tab", "swap pane focus"),
        row("Up / Down", "move the playlist or track cursor"),
        row(
            "Space",
            "mark playlist for deletion or mark a playlist track",
        ),
        row("Del", "delete marked playlists or remove marked tracks"),
        row("a", "set the focused playlist active"),
        row("Enter", "create when focused on + New Playlist"),
        Line::from(""),
        Line::styled("ADD TO Playlist", heading),
        row("Tab", "swap pane focus"),
        row(
            "Arrows",
            "navigate the device tree or active-playlist tracks",
        ),
        row("Space", "mark a song/album or active-playlist track"),
        row("Enter", "add marked songs to the active playlist"),
        row("Del", "remove marked tracks from the active playlist"),
        Line::from(""),
        Line::styled("Settings", heading),
        row("Up / Down", "select and immediately apply a theme"),
    ];
    let block = themed_block(" HELP — ? / Esc close ", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(inner);
    frame.render_widget(
        Paragraph::new(Text::from(left)).wrap(Wrap { trim: false }),
        left_area,
    );
    frame.render_widget(
        Paragraph::new(Text::from(right)).wrap(Wrap { trim: false }),
        right_area,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_playlist_tracks_pane(
    frame: &mut Frame,
    area: Rect,
    snapshot: &DeviceSnapshot,
    playlist: Option<&Playlist>,
    marks: &HashSet<u32>,
    state: &mut ListState,
    focused: bool,
    empty_message: Option<&str>,
) {
    let Some(playlist) = playlist else {
        frame.render_widget(
            Paragraph::new(empty_message.unwrap_or("Select a playlist to view its tracks."))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .block(themed_block(" PLAYLIST TRACKS ", focused)),
            area,
        );
        return;
    };
    let items = playlist.track_ids.iter().filter_map(|id| {
        let track = snapshot.tracks.iter().find(|track| track.id == *id)?;
        let marked = if marks.contains(&track.id) {
            "[x]"
        } else {
            "[ ]"
        };
        Some(ListItem::new(format!("{marked} {}", track.name)))
    });
    let list = List::new(items)
        .block(themed_owned_block(
            format!(" {} — TRACKS · Space mark · Del remove ", playlist.name),
            focused,
        ))
        .style(
            Style::default()
                .fg(palette().primary)
                .bg(palette().background),
        )
        .highlight_style(
            Style::default()
                .fg(palette().background)
                .bg(palette().primary)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, area, state);
}

fn tree_widget<'a, Identifier>(
    items: &'a [TreeItem<'a, Identifier>],
    title: &'static str,
    focused: bool,
) -> Tree<'a, Identifier>
where
    Identifier: Clone + PartialEq + Eq + std::hash::Hash,
{
    Tree::new(items)
        .expect("tree item identifiers are unique within each parent")
        .block(themed_block(title, focused))
        .style(
            Style::default()
                .fg(palette().primary)
                .bg(palette().background),
        )
        .highlight_style(
            Style::default()
                .fg(palette().background)
                .bg(palette().primary)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ")
}

fn folder_items(
    folders: &[Folder],
    tracks: &[Track],
    _albums: &[Album],
    marked: &HashSet<u32>,
) -> Vec<TreeItem<'static, DeviceItemId>> {
    let album_folder_ids: HashSet<_> = folders
        .iter()
        .filter(|folder| folder.name.eq_ignore_ascii_case("Music"))
        .flat_map(|music| music.children.iter())
        .flat_map(|artist| artist.children.iter())
        .map(|album| album.id)
        .collect();
    folders
        .iter()
        .filter(|folder| !is_album_object_folder(folder))
        .map(|folder| folder_item(folder, tracks, marked, &album_folder_ids))
        .collect()
}

fn is_album_object_folder(folder: &Folder) -> bool {
    folder.name.eq_ignore_ascii_case("Albums")
}

fn folder_item(
    folder: &Folder,
    tracks: &[Track],
    marked: &HashSet<u32>,
    album_folder_ids: &HashSet<u32>,
) -> TreeItem<'static, DeviceItemId> {
    let mut child_folders: Vec<_> = folder.children.iter().collect();
    if folder.name.eq_ignore_ascii_case("Music") {
        child_folders.sort_by_key(|child| artist_sort_key(&child.name));
    }
    let mut children: Vec<_> = child_folders
        .into_iter()
        .map(|child| folder_item(child, tracks, marked, album_folder_ids))
        .collect();
    children.extend(
        tracks
            .iter()
            .filter(|track| track.parent_id == folder.id)
            .map(|track| {
                let indicator = if marked.contains(&track.id) {
                    "[x]"
                } else {
                    "[ ]"
                };
                TreeItem::new_leaf(
                    DeviceItemId::Track(track.id),
                    format!("{indicator} {}", track.name),
                )
            }),
    );
    let direct_track_ids: Vec<_> = tracks
        .iter()
        .filter(|track| track.parent_id == folder.id)
        .map(|track| track.id)
        .collect();
    let label = if album_folder_ids.contains(&folder.id) && !direct_track_ids.is_empty() {
        let indicator = if group_is_fully_marked(&direct_track_ids, marked) {
            "[x]"
        } else {
            "[ ]"
        };
        format!("{indicator} {}", folder.name)
    } else {
        folder.name.clone()
    };
    TreeItem::new(DeviceItemId::Folder(folder.id), label, children)
        .expect("libmtp folder IDs are unique within each parent")
}

fn artist_sort_key(name: &str) -> String {
    let lower = name.to_lowercase();
    lower.strip_prefix("the ").unwrap_or(&lower).to_owned()
}

fn draw_device_message(
    frame: &mut Frame,
    area: Rect,
    message: &str,
    focused: bool,
    is_error: bool,
) {
    let color = if is_error {
        palette().error
    } else {
        palette().primary
    };
    frame.render_widget(
        Paragraph::new(message)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(color).bg(palette().background))
            .block(themed_block(" ZUNE DEVICE ", focused)),
        area,
    );
}

fn connection_line(connection: &Connection) -> Line<'static> {
    match connection {
        Connection::Loading(message) => {
            Line::styled((*message).to_owned(), Style::default().fg(palette().muted))
        }
        Connection::NotFound => Line::styled(NOT_FOUND_SHORT, Style::default().fg(palette().error)),
        Connection::Connected(snapshot) => {
            let info = &snapshot.info;
            let device_name = if info.friendly_name.is_empty() {
                &info.model
            } else {
                &info.friendly_name
            };
            Line::styled(
                format!("✓ Connected to Zune — {device_name}"),
                Style::default().fg(palette().success),
            )
        }
    }
}

fn status_line(app: &App) -> Line<'static> {
    app.transfer_status.as_ref().map_or_else(
        || connection_line(&app.connection),
        |message| Line::styled(message.clone(), Style::default().fg(palette().accent)),
    )
}

fn themed_block(title: &'static str, focused: bool) -> Block<'static> {
    let border = if focused {
        palette().primary
    } else {
        palette().muted
    };
    Block::default()
        .title(title)
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border).add_modifier(if focused {
            Modifier::BOLD
        } else {
            Modifier::empty()
        }))
        .style(
            Style::default()
                .fg(palette().primary)
                .bg(palette().background),
        )
}

fn themed_owned_block(title: String, focused: bool) -> Block<'static> {
    themed_block("", focused).title(title)
}

fn centered(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.saturating_sub(2).min(max_width);
    let height = area.height.saturating_sub(2).min(max_height);
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    area
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_filename_cannot_escape_download_directory() {
        let track = Track {
            id: 42,
            parent_id: 1,
            storage_id: 2,
            name: "Unsafe".to_owned(),
            filename: "../../outside.wma".to_owned(),
        };
        assert_eq!(safe_download_name(&track), "outside.wma");
    }

    #[test]
    fn track_range_marks_only_tracks_inclusive() {
        let visible = [
            DeviceItemId::Folder(1),
            DeviceItemId::Track(2),
            DeviceItemId::Folder(3),
            DeviceItemId::Track(4),
        ];
        let mut marks = HashSet::new();
        mark_track_range(
            &mut marks,
            &visible,
            DeviceItemId::Track(2),
            DeviceItemId::Track(4),
            &[],
        );
        assert_eq!(marks, HashSet::from([2, 4]));
    }

    #[test]
    fn only_the_top_level_albums_object_folder_is_filtered() {
        let albums = Folder {
            id: 1,
            storage_id: 2,
            name: "Albums".to_owned(),
            children: Vec::new(),
        };
        let artist_named_albums = Folder {
            id: 3,
            storage_id: 2,
            name: "The Albums".to_owned(),
            children: Vec::new(),
        };
        assert!(is_album_object_folder(&albums));
        assert!(!is_album_object_folder(&artist_named_albums));
    }

    #[test]
    fn artist_sorting_ignores_the_without_changing_names() {
        let mut names = vec!["The Kingston Trio", "Beatles", "Andrew Lloyd Webber"];
        names.sort_by_key(|name| artist_sort_key(name));
        assert_eq!(
            names,
            vec!["Andrew Lloyd Webber", "Beatles", "The Kingston Trio"]
        );
    }

    #[test]
    fn fully_marked_only_album_cascades_through_artist_with_full_confirmation() {
        let snapshot = physical_delete_snapshot(false);
        let scope = delete_scope(&snapshot, &HashSet::from([20, 21]));
        assert_eq!(scope.track_count, 2);
        assert_eq!(scope.album_count, 1);
        assert_eq!(scope.artist_names, vec!["Beatles"]);
        assert_eq!(
            scope
                .items
                .iter()
                .map(|item| item.object_id)
                .collect::<Vec<_>>(),
            vec![100, 20, 21, 12, 11]
        );
        let confirmation = DeleteConfirmation {
            device_name: "PAUL".to_owned(),
            items: scope.items,
            track_count: scope.track_count,
            album_count: scope.album_count,
            artist_names: scope.artist_names,
        };
        assert_eq!(
            delete_confirmation_message(&confirmation),
            "Delete 2 tracks, 1 album, and the artist folder 'Beatles' from PAUL's Zune? [y/N]"
        );
    }

    #[test]
    fn artist_folder_is_preserved_when_another_album_remains() {
        let snapshot = physical_delete_snapshot(true);
        let scope = delete_scope(&snapshot, &HashSet::from([20, 21]));
        assert!(scope.artist_names.is_empty());
        assert!(!scope.items.iter().any(|item| item.object_id == 11));
    }

    fn physical_delete_snapshot(with_second_album: bool) -> DeviceSnapshot {
        let mut albums = vec![Folder {
            id: 12,
            storage_id: 2,
            name: "Abbey Road".to_owned(),
            children: Vec::new(),
        }];
        if with_second_album {
            albums.push(Folder {
                id: 13,
                storage_id: 2,
                name: "Revolver".to_owned(),
                children: Vec::new(),
            });
        }
        DeviceSnapshot {
            info: DeviceInfo {
                friendly_name: "PAUL".to_owned(),
                model: "Zune".to_owned(),
                serial: String::new(),
                version: String::new(),
                capacity_bytes: 0,
                free_bytes: 0,
            },
            folders: vec![Folder {
                id: 10,
                storage_id: 2,
                name: "Music".to_owned(),
                children: vec![Folder {
                    id: 11,
                    storage_id: 2,
                    name: "Beatles".to_owned(),
                    children: albums,
                }],
            }],
            tracks: vec![
                Track {
                    id: 20,
                    parent_id: 12,
                    storage_id: 2,
                    name: "One".to_owned(),
                    filename: "one.mp3".to_owned(),
                },
                Track {
                    id: 21,
                    parent_id: 12,
                    storage_id: 2,
                    name: "Two".to_owned(),
                    filename: "two.mp3".to_owned(),
                },
            ],
            albums: vec![Album {
                id: 100,
                name: "Abbey Road".to_owned(),
                track_ids: vec![20, 21],
            }],
            playlists: Vec::new(),
        }
    }

    #[test]
    fn delete_confirmation_uses_device_name_count_and_cancel_preserves_marks() {
        let mut app = App::new().expect("local tree");
        app.focused_pane = Pane::Device;
        app.device_marks = HashSet::from([20, 21]);
        app.connection = Connection::Connected(DeviceSnapshot {
            info: DeviceInfo {
                friendly_name: "TEST ZUNE".to_owned(),
                model: "Zune".to_owned(),
                serial: String::new(),
                version: String::new(),
                capacity_bytes: 0,
                free_bytes: 0,
            },
            folders: Vec::new(),
            tracks: vec![
                Track {
                    id: 20,
                    parent_id: 1,
                    storage_id: 2,
                    name: "One".to_owned(),
                    filename: "one.mp3".to_owned(),
                },
                Track {
                    id: 21,
                    parent_id: 1,
                    storage_id: 2,
                    name: "Two".to_owned(),
                    filename: "two.mp3".to_owned(),
                },
            ],
            albums: vec![Album {
                id: 10,
                name: "Album".to_owned(),
                track_ids: vec![20, 21],
            }],
            playlists: Vec::new(),
        });

        app.request_delete();
        let confirmation = app.delete_confirmation.as_ref().expect("confirmation");
        assert_eq!(confirmation.device_name, "TEST ZUNE");
        assert_eq!(confirmation.items.len(), 3);

        app.handle_delete_confirmation(KeyCode::Esc);
        assert!(app.delete_confirmation.is_none());
        assert_eq!(app.device_marks, HashSet::from([20, 21]));
        assert!(app.transfer_receiver.is_none());
    }

    #[test]
    fn delete_is_ignored_for_local_focus_or_no_device_marks() {
        let mut app = App::new().expect("local tree");
        app.request_delete();
        assert!(app.delete_confirmation.is_none());

        app.device_marks.insert(20);
        app.request_delete();
        assert!(app.delete_confirmation.is_none());
    }

    #[test]
    fn delete_confirmation_queues_worker_after_setting_immediate_status() {
        let mut app = App::new().expect("local tree");
        app.delete_confirmation = Some(DeleteConfirmation {
            device_name: "PAUL".to_owned(),
            items: vec![DeleteItem {
                object_id: 42,
                name: "Song".to_owned(),
            }],
            track_count: 1,
            album_count: 0,
            artist_names: Vec::new(),
        });

        app.handle_delete_confirmation(KeyCode::Char('y'));

        assert_eq!(
            app.transfer_status.as_deref(),
            Some("Deleting 1 item(s) from PAUL's Zune...")
        );
        assert!(app.pending_transfer.is_some());
        assert!(app.transfer_receiver.is_none());
    }

    fn playlist_test_app() -> App {
        let mut app = App::new().expect("local tree");
        let mut snapshot = physical_delete_snapshot(false);
        snapshot.playlists = vec![
            Playlist {
                id: 30,
                parent_id: 0,
                storage_id: 2,
                name: "Road Trip".to_owned(),
                track_ids: vec![20, 21],
            },
            Playlist {
                id: 31,
                parent_id: 0,
                storage_id: 2,
                name: "Favorites".to_owned(),
                track_ids: vec![21],
            },
        ];
        app.connection = Connection::Connected(snapshot);
        app.screen = Screen::Playlists;
        app.playlist_state.select(Some(1));
        app
    }

    #[test]
    fn active_playlist_moves_independently_of_delete_marks() {
        let mut app = playlist_test_app();
        app.handle_playlist_list_key(KeyCode::Char(' '));
        app.handle_playlist_list_key(KeyCode::Char('a'));
        assert_eq!(app.active_playlist, Some(30));
        assert_eq!(app.playlist_delete_marks, HashSet::from([30]));

        app.playlist_state.select(Some(2));
        app.handle_playlist_list_key(KeyCode::Char('a'));
        assert_eq!(app.active_playlist, Some(31));
        assert_eq!(app.playlist_delete_marks, HashSet::from([30]));

        app.handle_playlist_list_key(KeyCode::Delete);
        let PlaylistConfirmation::DeletePlaylists(playlists) =
            app.playlist_confirmation.as_ref().expect("confirmation")
        else {
            panic!("wrong confirmation type");
        };
        assert_eq!(
            playlists
                .iter()
                .map(|playlist| playlist.id)
                .collect::<Vec<_>>(),
            vec![30]
        );
    }

    #[test]
    fn right_pane_removal_targets_displayed_playlist() {
        let mut app = playlist_test_app();
        app.playlist_focus = PlaylistPane::Tracks;
        app.playlist_track_state.select(Some(1));
        app.handle_playlist_track_key(KeyCode::Char(' '));
        app.handle_playlist_track_key(KeyCode::Delete);
        let PlaylistConfirmation::RemoveTracks {
            playlist,
            track_ids,
        } = app.playlist_confirmation.as_ref().expect("confirmation")
        else {
            panic!("wrong confirmation type");
        };
        assert_eq!(playlist.id, 30);
        assert_eq!(track_ids, &vec![21]);
    }

    #[test]
    fn new_playlist_input_queues_empty_playlist_creation() {
        let mut app = playlist_test_app();
        app.playlist_state.select(Some(0));
        app.handle_playlist_list_key(KeyCode::Enter);
        for character in "Morning Mix".chars() {
            app.handle_playlist_name_input(KeyCode::Char(character));
        }
        app.handle_playlist_name_input(KeyCode::Enter);
        assert!(matches!(
            app.pending_transfer,
            Some(TransferJob::CreatePlaylist { ref name, storage_id: 2 }) if name == "Morning Mix"
        ));
    }

    #[test]
    fn appending_playlist_tracks_preserves_order_and_skips_duplicates() {
        let (updated, added) = append_unique_track_ids(&[20, 21], &[21, 22, 20, 23, 22]);
        assert_eq!(updated, vec![20, 21, 22, 23]);
        assert_eq!(added, 2);
    }

    #[test]
    fn add_to_playlist_requires_the_shared_active_playlist() {
        let mut app = playlist_test_app();
        app.active_playlist = None;
        app.add_to_marks.insert(20);
        app.add_marked_tracks_to_active_playlist();
        assert!(app.pending_transfer.is_none());
        assert_eq!(
            app.transfer_status.as_deref(),
            Some("No active playlist — set one with 'a' on the CRUD Playlist screen.")
        );
    }

    #[test]
    fn add_to_playlist_album_mark_queues_every_album_track() {
        let mut app = playlist_test_app();
        app.active_playlist = Some(31);
        app.add_to_device_state.select(vec![
            DeviceItemId::Folder(10),
            DeviceItemId::Folder(11),
            DeviceItemId::Folder(12),
        ]);
        app.toggle_add_to_mark();
        assert_eq!(app.add_to_marks, HashSet::from([20, 21]));

        app.add_marked_tracks_to_active_playlist();
        let Some(TransferJob::AddPlaylistTracks {
            playlist,
            track_ids,
        }) = app.pending_transfer
        else {
            panic!("add job was not queued");
        };
        assert_eq!(playlist.id, 31);
        assert_eq!(track_ids, vec![20, 21]);
    }

    #[test]
    fn add_to_right_pane_removal_uses_active_playlist() {
        let mut app = playlist_test_app();
        app.active_playlist = Some(31);
        app.playlist_track_state.select(Some(0));
        app.handle_playlist_tracks_for(KeyCode::Char(' '), app.active_playlist().cloned());
        app.handle_playlist_tracks_for(KeyCode::Delete, app.active_playlist().cloned());
        let PlaylistConfirmation::RemoveTracks {
            playlist,
            track_ids,
        } = app.playlist_confirmation.as_ref().expect("confirmation")
        else {
            panic!("wrong confirmation type");
        };
        assert_eq!(playlist.id, 31);
        assert_eq!(track_ids, &vec![21]);
    }

    #[test]
    fn persisted_active_playlist_restores_only_when_id_and_name_match() {
        let mut app = playlist_test_app();
        let snapshot = match &app.connection {
            Connection::Connected(snapshot) => snapshot.clone(),
            _ => unreachable!(),
        };
        app.active_playlist = None;
        app.persisted.active_playlist_id = Some(30);
        app.persisted.active_playlist_name = Some("Road Trip".to_owned());
        app.restore_active_playlist(&snapshot);
        assert_eq!(app.active_playlist, Some(30));

        app.persisted.active_playlist_name = Some("Playlist from another Zune".to_owned());
        app.restore_active_playlist(&snapshot);
        assert_eq!(app.active_playlist, None);
        assert_eq!(app.persisted.active_playlist_id, None);
        assert_eq!(app.persisted.active_playlist_name, None);
    }

    #[test]
    fn settings_selection_updates_shared_persisted_theme() {
        let mut app = playlist_test_app();
        app.settings_state.select(Some(0));
        app.handle_settings_key(KeyCode::Down);
        assert_eq!(app.persisted.theme, Theme::CherryRed);
        assert_eq!(app.settings_state.selected(), Some(1));
    }
}
