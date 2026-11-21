# Tin

Double-tap Shift to capture what you're looking at. Pick a window, something inside it or an
area, say what it's for, and press Enter. Tin keeps it as a file an agent can pick up. An
open-source take on [Copper](https://shadcn.com/copper).

<a href="docs/media/capture-demo.mp4"><img src="docs/media/capture-demo.webp" width="960" alt="Double-tap Shift on a post on X, pick the post, say it's for the platformer and press Enter. Tin saves the post's link, the voice note, the post's text and its video. A recreation of Tin's interface with a made-up account. Play the demo in 4K."></a>

Each capture keeps a picture of what you picked, its text and links, the page it was on, and your
voice note from [Hex](https://github.com/anomalyco/hex). Tweets are kept whole, with their photos
and videos, in case they're deleted.

## Install

1. Download **[tin-linux-x86_64.tar.gz](https://github.com/AugusDogus/tin/releases/latest/download/tin-linux-x86_64.tar.gz)**
   and put `tin` and `tin-app` on your `PATH`.
2. Run `sudo usermod -aG input $USER` and log in again, so Tin can hear Shift.
3. Start `tin-app --background` at login, and run Hex for voice notes.

Tin runs on Linux with the COSMIC desktop for now. macOS support isn't done yet.

The browser extension (Helium for now) adds the page's selected text, whole tweets, and the
images and videos where you pick. Download
**[tin-extension.zip](https://github.com/AugusDogus/tin/releases/latest/download/tin-extension.zip)**,
unzip it, run `tin extension install`, then **Load unpacked** in `helium://extensions`.

## Routes

Say or type what a capture is for, and a route sends it to a project:

```toml
[[route]]
name = "blog"
aliases = ["for the blog"]
path = "~/dev/blog"
```

`tin init` writes a config to start from.

## For agents

Each capture is a JSON file in `~/.local/share/tin/captures`. `tin list --json` lists them,
`tin show last --json` shows one, and `tin done <id>` checks one off.

## Build

```sh
cargo build --release   # Linux needs libxkbcommon-dev and libwayland-dev
```

MIT
