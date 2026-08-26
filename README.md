# ZuneTUI

A terminal app for managing a classic Microsoft Zune (30/80/120) from
Linux — browse and transfer files, and create/edit playlists — built
for Arch and Omarchy, themed to match.

Built in Rust with [ratatui](https://ratatui.rs/), talking directly to
[libmtp](https://libmtp.sourceforge.io/) via `bindgen`-generated FFI
bindings (no hand-written struct layouts — they're generated straight
from the real `libmtp.h` at build time).

## Features

- **File Management** — dual-pane browser (your local filesystem next
  to the Zune's real folder tree). Mark files or whole album folders,
  copy in either direction, delete tracks/albums off the device with
  confirmation. Uploads auto-resolve into the correct
  `Music/Artist/Album` folder on the device from ID3 tags (or your
  local folder structure, if tags are missing), matching existing
  artist/album folders case-insensitively and ignoring a leading
  "The" when comparing names.
- **Playlist Management** — a CRUD screen for creating, deleting, and
  editing existing playlists, plus a dedicated screen for browsing
  the device's library and adding songs or whole albums into
  whichever playlist you've marked "active."
- **Theming** — four fixed color themes (Tron Blue, Cherry Red,
  Orange, Green) plus a "System" option that reads your active
  Omarchy theme's palette. Tron Blue is the default and the fallback
  on non-Omarchy systems.

## Requirements

On Arch or Omarchy:

```bash
sudo pacman -S --needed rust clang pkgconf libmtp
```

`libmtp` on Arch already includes the CLI tools (`mtp-detect` etc.) —
there's no separate `mtp-tools` package to install.

## Building

```bash
git clone https://github.com/gregoftheweb/zunetui.git
cd zunetui
cargo build --release
```

To install it as a regular command on your `$PATH`:

```bash
cargo install --path .
```

This installs to `~/.cargo/bin/zunetui`. If you installed Rust via
`pacman` rather than `rustup`, that directory may not be on your
`$PATH` automatically — add it to your shell config if needed:

```bash
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

## ⚠️ Important: writing to the device requires MTPZ authentication (you must solve this yourself)

**This is the single most important thing to understand before
installing ZuneTUI.**

Classic Zune devices require an encrypted authentication handshake,
called **MTPZ**, before they'll accept any new file or playlist
write. This is separate from ordinary MTP, and it's why:

- **Reading** from your Zune (browsing files, listing tracks, viewing
  storage info) works immediately, with no setup — you'll see this
  functionality succeed the moment you plug in your device.
- **Writing** to your Zune (uploading files, creating folders,
  creating or editing playlists, deleting anything) will fail with a
  `PTP AccessDenied` error until MTPZ authentication is set up on
  your machine.

### What's actually needed

`libmtp` (the library this app is built on) has MTPZ support
compiled in on Arch — it can perform the authentication handshake.
What it does **not** have, and will never ship, is the actual
cryptographic key material the handshake requires. This is a
deliberate decision by the libmtp project: that key material
originates from Microsoft's own Zune desktop software, and
distributing it isn't something the libmtp maintainers are willing
to do, for legal reasons.

Concretely: `libmtp` looks for this key material in a file at
`~/.mtpz-data`. Without that file present and valid, every write
operation to a classic Zune will fail with the exact same
`AccessDenied` error, regardless of anything ZuneTUI does.

### This is not something ZuneTUI can fix, and it's not a bug

If you install ZuneTUI and file transfer / playlist editing doesn't
work, this is almost certainly why — and it's expected, not a defect
in this project. **Sourcing that key material is entirely your
responsibility**, and explicitly outside the scope of what this
project provides, distributes, links to, or offers support for.
People who've owned this exact hardware problem have found their own
solutions over the years; searching the Linux/MTP community (forums,
old blog posts, GitHub issues about libmtp and Zune support) is the
place to start. This project deliberately does not include, link to,
or point toward any specific source for that material.

Once `~/.mtpz-data` exists and is valid on your system, every write
feature in ZuneTUI (upload, playlist editing, delete) works exactly
as documented above — there's no ZuneTUI-specific configuration
needed beyond that.

## Keybindings

**Global** (work from any screen):

| Key | Action |
|---|---|
| `1` | File Management |
| `2` | CRUD Playlist |
| `3` | ADD TO Playlist |
| `4` | Settings |
| `?` | Help |
| `` ` `` | Toggle debug log panel |
| `q` / `Esc` | Quit |

**File Management** — dual-pane (local filesystem left, Zune device
tree right):

| Key | Action |
|---|---|
| `Tab` | Swap pane focus |
| `Space` | Mark item (an album folder marks it and every track inside) |
| `Shift+Space` | Mark a range |
| `Enter` | Copy marked items toward the other pane |
| `c` | Clear all marks |
| `Del` | Delete marked device items, with confirmation (cascades to the artist folder if it becomes empty) |

**CRUD Playlist** — playlist list left, its tracks right:

| Key | Action |
|---|---|
| `Tab` | Swap pane focus |
| `Space` | Mark a playlist for deletion, or mark a track |
| `Del` | Delete marked (with confirmation) |
| `a` | Set the focused playlist as ACTIVE |
| `Enter` on "+ New Playlist" | Create a new playlist |

**ADD TO Playlist** — Zune device tree left, active playlist's tracks
right:

| Key | Action |
|---|---|
| `Tab` | Swap pane focus |
| `Space` | Mark a song or whole album |
| `Enter` | Add marked items to the active playlist |
| `Del` | Remove a marked track from the active playlist |

## License

See [LICENSE](./LICENSE).

## Acknowledgments

Built with [Codex](https://openai.com/index/openai-codex/) as
coder, [Claude](https://claude.ai) as design partner, for a real
Zune that belonged to a real person — hence the extensive real-device
debugging throughout the commit history.
