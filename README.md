# **S**ync**t**hing **F**older **E**vent **D**aemon (stfed)

[![Build status](https://github.com/desbma/stfed/actions/workflows/ci.yml/badge.svg)](https://github.com/desbma/stfed/actions)
[![License](https://img.shields.io/github/license/desbma/stfed.svg?style=flat)](https://github.com/desbma/stfed/blob/master/LICENSE)

**S**ync**t**hing **F**older **E**vent **D**aemon, aka `stfed`, is a small companion daemon to run alongside [Syncthing](https://syncthing.net/), which allows running custom commands when certain folder events happen.

I wrote this to replace a bunch of inefficient and unreliable scripts that were using [inotifywait](https://man.archlinux.org/man/community/inotify-tools/inotifywait.1.en) to watch specific files/folders. Instead of watching at the file level, `stfed` uses the Syncthing API to be notified when files are synchronized.

It is very light on ressource usage and is therefore suitable for use in all contexts: desktop PC, headless servers, home automation setups, etc.

## Features

* can react to custom events
    * folder synchronisation finished
    * file synchronisation finished
    * synchronisation conflict

_TODO_

## Installation

### From source

You need a Rust build environment for example from [rustup](https://rustup.rs/).

```
cargo build --release
install -Dm 755 -t /usr/local/bin target/release/stfed
```

## Configuration

_TODO_

## License

[GPLv3](https://www.gnu.org/licenses/gpl-3.0-standalone.html)
