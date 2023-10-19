# **S**ync**t**hing **F**older **E**vent **D**aemon (stfed)

[![Build status](https://github.com/desbma/stfed/actions/workflows/ci.yml/badge.svg)](https://github.com/desbma/stfed/actions)
[![AUR version](https://img.shields.io/aur/version/stfed.svg?style=flat)](https://aur.archlinux.org/packages/stfed/)
[![License](https://img.shields.io/github/license/desbma/stfed.svg?style=flat)](https://github.com/desbma/stfed/blob/master/LICENSE)

**S**ync**t**hing **F**older **E**vent **D**aemon, aka `stfed`, is a small companion daemon to run alongside [Syncthing](https://syncthing.net/), which allows running custom commands when certain folder events happen.

I wrote this to replace a bunch of inefficient and unreliable scripts that were using [inotifywait](https://man.archlinux.org/man/community/inotify-tools/inotifywait.1.en) to watch specific files/folders. Instead of watching at the file level, `stfed` uses the Syncthing API to be notified when files are synchronized.

It is very light on ressource usage and is therefore suitable for use in all contexts: desktop PC, headless servers, home automation setups, etc.

## Features

- can react to custom events
  - folder synchronisation finished
  - file synchronisation finished
  - synchronisation conflict
- light on system ressources
- no runtime dependency outside of Syncthing

## Installation

### From source

You need a Rust build environment for example from [rustup](https://rustup.rs/).

```
cargo build --release
install -Dm 755 -t /usr/local/bin target/release/stfed
```

A systemd service is provided for convenience, to use it:

```
install -D -t ~/.config/systemd/user ./systemd/stfed.service
systemctl --user daemon-reload
systemctl --user enable --now stfed.service
```

### From the AUR

Arch Linux users can install the [stfed AUR package](https://aur.archlinux.org/packages/stfed/).

## Configuration

Configuration file are stored in a directory following the [XDG specification](https://specifications.freedesktop.org/basedir-spec/basedir-spec-latest.html), so typically `~/.config/stfed`.

### Main configuration

`config.toml`

**This file is optional, if absent, `stfed` will read Syncthing configuration and build its own configuration from it. You typically only need to use this file if using a non local Syncthing instance.**

Sample content:

```
url = "http://127.0.0.1:8384/"  # Syncthing URL
api_key = "xyz"  # Syncthing API key
```

### Hooks

`hooks.toml`

This file defines hooks, ie. events you want to react to, and what commands to run when they occur.

Sample section for a single hook:

```
[[hooks]]

# Syncthing folder path
folder = "/absolute/path/of/the/folder"

# Event type, one of:
# file_down_sync_done: triggers when a file has been fully synchronized locally (see filter to match for a specific file)
# folder_down_sync_done: triggers when a folder has been fully synchronized locally
# file_conflict: triggers when Syncthing creates a .stconflict file due to a synchronization conflict
event = "file_down_sync_done"

# glob rule for specific file matching for file_down_sync_done events
filter = "shopping-list.txt"

# command to run when event triggers
command = "notify-send 'stfef event triggered!'"

# Whether to allow several commands for the same hook to run simultaneously
# if false, and a burst of events comes, the commands will be skipped while the previous one is still running
# optional, defaults to false
allow_concurrent = false
```

## License

[GPLv3](https://www.gnu.org/licenses/gpl-3.0-standalone.html)
