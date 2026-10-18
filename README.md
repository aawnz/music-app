# Spotifast with Local Files

[Spotifast](https://github.com/crmne/spotifast) (a fast, native Spotify
client) with one addition: a **Local Files** page, like Spotify's own. Add
folders from your computer and play their MP3, FLAC, M4A, OGG and WAV files
in Spotifast, with the regular player bar, shuffle, repeat and covers.

This is an unofficial build. It is not made or supported by Spotifast's
author, so please don't send him reports about it.

## Install (Windows)

1. Open [Releases](../../releases/latest) and download the file ending in
   **`-setup.exe`**.
2. Run it. If Windows says it protected your PC, click **More info**, then
   **Run anyway** (the build isn't code-signed).
3. Open Spotifast and sign in with your Spotify account. Spotify playback
   needs Premium.

It installs like the regular Spotifast, and replaces it if you have it.

## Updates

Spotifast checks this page for updates and installs them itself, just as
the regular app does. A new version appears here a few hours after each
Spotifast release.

## Using Local Files

- Click **Local Files** in the sidebar, then **Add a folder** (or **Use my
  Music folder**). Folders inside it are included.
- Double-click a song to play it. The player bar switches to it; playing
  anything from Spotify switches it back.
- New files are found when Spotifast starts, or click **Rescan**.

## How it's made

A scheduled build takes each Spotifast release, adds the new files in
[`additions`](additions) and the small changes in
[`local-files.patch`](local-files.patch), and publishes it here. Spotifast is
MIT-licensed; see [LICENSE](LICENSE).
