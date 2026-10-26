# Spotifast with Local Files

[Spotifast](https://github.com/crmne/spotifast) (a fast, native Spotify
client) with a few additions:

- A **Local Files** page, like Spotify's own. Add folders from your computer
  and play their MP3, FLAC, M4A, OGG and WAV files, with the regular player
  bar, shuffle, repeat and covers.
- **Local files in your playlists.** Right-click a song in Local Files and
  choose **Add to playlist**. It goes into your real Spotify playlist, the
  same way Spotify's own app adds local files. In playlists, local songs play
  in order with the rest, shuffle included, from the folder set in
  **Settings > Playback > Local files** (your first Local Files folder, unless
  you choose another).
- **Monthly listeners** on artist pages.
- **Song credits.** Right-click a song and choose **Show credits**.
- **SoundCloud.** Click **SoundCloud** in the sidebar to search SoundCloud or paste a track or playlist link, and play it in the player bar. This is unofficial: it reads SoundCloud the way its website does, so it can stop working when SoundCloud changes, and Go+ songs play only as previews.
- **Jam with friends who use this app.** Click **Jam** in the sidebar to start one and share its code, or enter a friend's code to join. Everyone hears the same song at the same time, each on their own Spotify Premium; anyone can play, pause, skip and add songs.

The last three use the same private Spotify requests as Spotify's own apps,
so they need playback turned on in Settings and may stop working if Spotify
changes them.

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
