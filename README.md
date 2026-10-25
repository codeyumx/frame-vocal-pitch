# Vocal Pitch

A small vocal pitch trainer that runs **on the Steam Frame headset itself** and shows, as a
SteamVR overlay, whether your voice is inside your target pitch range.

Look up, speak, and see one of six states: in range, too low, too high, no voice, mic muted, or no
microphone. Settings live in a SteamVR dashboard tab, so you can change the target range, the
overlay position and the reminder tone from inside VR without a PC or a keyboard.

> **Unofficial.** This is a personal project. It is not affiliated with, endorsed by or supported
> by Valve Corporation. Steam, SteamVR and Steam Frame are trademarks of Valve Corporation.

## Requirements

- A Steam Frame headset (aarch64 SteamOS). There is no desktop/PC build: the app talks to the
  headset's OpenVR runtime and its own PipeWire session.
- SteamVR running on the headset.
- Nothing else. The binary is self-contained; PipeWire, WirePlumber, the OpenVR runtime and the
  Vulkan loader come from the system.

## Install

Tip: Open the desktop on Frame, navigate here with a web browser, copy this command, then use the paste button in the Konsole.
From the headset's terminal (Developer Mode → Konsole, or `ssh`):

```sh
curl -fsSL https://github.com/codeyumx/frame-vocal-pitch/releases/latest/download/install.sh | bash
```

The installer downloads the latest release, checks it against the release's `SHA256SUMS`, and
installs it into your home directory:

| What | Where |
|---|---|
| binary | `~/.local/bin/frame-vocal-pitch` |
| systemd user unit | `~/.config/systemd/user/frame-vocal-pitch.service` |
| dashboard entry | `~/.local/share/applications/frame-vocal-pitch.desktop` |
| icons | `~/.local/share/icons/hicolor/48x48/apps/frame-vocal-pitch.png` (also `128x128`, `256x256`) |
| updater (a copy of the installer) | `~/.local/share/frame-vocal-pitch/install.sh` |
| configuration | `~/.config/frame-vocal-pitch/config.toml` |

It refuses to run as root, refuses to run on a non-aarch64 machine, and warns when the system does
not look like SteamOS. Re-run the same command to update: the payload is replaced in place and the
service is restarted if it was running.

### Options

```sh
url=https://github.com/codeyumx/frame-vocal-pitch/releases/latest/download/install.sh

curl -fsSL "$url" | bash -s -- --autostart           # start with SteamVR
curl -fsSL "$url" | bash -s -- --no-autostart        # do not start with SteamVR
curl -fsSL "$url" | bash -s -- --version             # version that would be installed
curl -fsSL "$url" | bash -s -- --release v0.4.0      # install that release instead of the latest
curl -fsSL "$url" | bash -s -- --uninstall           # remove, keep the configuration
curl -fsSL "$url" | bash -s -- --uninstall --purge   # remove, including the configuration
```

The autostart choice is stored in `~/.config/frame-vocal-pitch/install-args`, so updates keep it;
you can also flip it later in the dashboard.

If you prefer not to pipe into a shell, download the tarball from the
[releases page](https://github.com/codeyumx/frame-vocal-pitch/releases), extract it and run
`./install.sh` from inside the extracted folder. It installs the payload next to it (`--release`
only works when the installer is piped or run on its own).

## Updates

The app checks for updates itself and installs one only when you ask for it, from the dashboard's
**Updates** section. Nothing about updates is ever shown on the HUD.

- **When it checks:** with **Check automatically** on (the default; `update.check` in
  `config.toml`), at start and then every hour, but it asks GitHub at most once a day (an hour
  after a failed check). **Check now** asks right away and works even with automatic checks off.
  It asks only for the tag of the latest release
  (`api.github.com/repos/codeyumx/frame-vocal-pitch/releases/latest`); the result is cached in
  `~/.cache/frame-vocal-pitch/update-check.json`.
- **What it installs:** after **Update to X.Y.Z** and a second click on **Confirm update**, it runs
  `~/.local/share/frame-vocal-pitch/install.sh --release vX.Y.Z`. That downloads exactly that
  release's tarball and `SHA256SUMS`, checks the checksum, and runs the release's own installer with
  your saved options (`~/.config/frame-vocal-pitch/install-args`, so autostart stays as it was).
  It is the same install you get from the one-line command, pinned to the version you confirmed.
- **Where it runs:** in a separate systemd user unit, `frame-vocal-pitch-update`, so it survives the
  app being restarted. Its output goes to `~/.cache/frame-vocal-pitch/update.log`. When the app
  runs as a service, the update restarts it; if you started it by hand, the app closes and you
  start it again from the dashboard's **+** list.
- **When it fails:** nothing installed is touched until the download has been verified, so a
  missing release, a broken download or a checksum mismatch leaves the current version installed
  and running. The dashboard shows "Update failed:" followed by the last line of `update.log`.
- **Checksum, not signature:** `SHA256SUMS` comes from the same GitHub release as the tarball. It
  catches broken or truncated downloads; it does not protect against a release that was replaced
  by someone with access to the repository.
- **Versions up to 0.2.1** have no updater: re-run the install command once; later versions can
  update from the dashboard.

If an update fails or seems stuck:

```sh
cat ~/.cache/frame-vocal-pitch/update.log                       # what it did; reason on the last line
systemctl --user status frame-vocal-pitch-update                # is an update still running?
~/.local/share/frame-vocal-pitch/install.sh --release vX.Y.Z    # run the same update by hand
```

Re-running the one-line install command always works as well and installs the latest release.

## Using it

- Turn the overlay on in the dashboard tab **Vocal Pitch**: the HUD panel follows your head; nudge
  its position from the same tab.
- Pick a target preset (`feminine` 165–255 Hz, `masculine` 85–180 Hz) or a custom band. Green is
  in range; just below it is yellow (raise your voice), just above it orange (lower it), further
  out red. The width of the yellow/orange zone is adjustable (default ±10 Hz, 0 turns it off).
  Dark grey is silence.
- Adjust size, opacity, position nudges, fade time and, if you want it, a reminder tone that
  beeps after you stay out of range for a configurable delay.
- Vocal rest can remind you to stop after 5–50 minutes of voiced time in the last hour.
- **Quit** closes the app; because the service is configured with `RestartPreventExitStatus=3`,
  quitting from the dashboard keeps it off until SteamVR or you start it again.

Run it by hand with:

```sh
frame-vocal-pitch            # normal
frame-vocal-pitch --verbose  # more logging
frame-vocal-pitch --probe    # print the audio/SteamVR environment and exit
```

`--probe` is the first thing to try when the HUD says "no mic": it prints which PipeWire source
would be recorded from, its mute state, and whether SteamVR's OpenVR library was found.

Logs go to the journal when it runs as a service:

```sh
journalctl --user -u frame-vocal-pitch -f
```

## Pitch is not the whole voice

The main indicator measures only pitch (how high or low your voice is). How feminine or masculine
a voice sounds depends at least as much on **resonance** (how bright or dark it sounds), and also on
intonation, weight and speech patterns. A voice can sit in the green range and still read
differently from what you want, or sit below it and still read as intended.

### Resonance indicator (experimental, off by default)

On the dashboard's **Resonance (experimental)** page you can turn on a rough brightness indicator.
The page shows your current reading in Hz: speak in your relaxed voice and set **Relaxed** to that
number, then do the same in your target voice for **Target**. The HUD (Display: Detailed) then shows
a slow-moving marker between "relaxed" and "target". It compares you only with those two values.

Turn it off if it doesn't help you, in particular if:
- you speak a different language or accent in VR than when you set the values, or switch often
  (vowels differ between languages and move the estimate; set them again in the language you use);
- you whisper, sing, shout, speak above ~300 Hz, or are in a noisy room;
- your goal isn't about how bright or dark your voice sounds;
- watching it makes you tense, or it doesn't match what you hear or what people you trust tell you.

It cannot tell you how others hear your voice; a voice coach or a trusted listener can. Details:
[`SPEC.md` §5.1](SPEC.md#51-resonance-feedback-experimental-opt-in).

## Microphone

The app records the Frame's default microphone like any other app, so Valve's processing applies
(EQ and downmix, and while active, echo cancellation with automatic gain control and noise
suppression with a voice gate). Pitch tracking copes with that; the resonance indicator is rougher
for it. SteamOS hides the raw hardware mic node from applications, and the app does not try to get
around that. Details are in [`SPEC.md` §6.3](SPEC.md#63-mic-input).

The app never changes the system-wide audio configuration and never touches Valve's mic filters.

## Uninstall

```sh
curl -fsSL https://github.com/codeyumx/frame-vocal-pitch/releases/latest/download/install.sh \
  | bash -s -- --uninstall
```

That stops and disables the service, stops an instance you started by hand, and removes the
binary, unit, dashboard entry, icons, the updater (`~/.local/share/frame-vocal-pitch/`) and the
update cache and log (`~/.cache/frame-vocal-pitch/`). `--uninstall --purge` removes
`~/.config/frame-vocal-pitch` as well. Nothing outside your home directory is removed.

## Build from source

Requires Docker for the cross-build (the headset is aarch64; the host is assumed to be x86_64).

```sh
# host-side tests (no headset needed)
cargo test --workspace

# aarch64 release binary, built in the Ubuntu 24.04 cross container
build/build-aarch64.sh -p frame-vocal-pitch --release
# -> target/aarch64-unknown-linux-gnu/release/frame-vocal-pitch

# tarball + SHA256SUMS in dist/
scripts/package.sh 0.1.0 target/aarch64-unknown-linux-gnu/release/frame-vocal-pitch
```

The cross container pins glibc 2.39, the same baseline as the headset's SteamOS. Releases are
built by `.github/workflows/release.yml` on a `v*` tag; it runs the tests, cross-builds, packages
and attaches the tarball, `SHA256SUMS` and `install.sh` to the GitHub release.

## Things this app deliberately never does

The installer and the app were written around the Frame's known audio hazards:

- **Never restarts PipeWire or WirePlumber.** Restarting them drops audio for SteamVR and Steam
  Link. Restarting the *unit* (`systemctl --user restart frame-vocal-pitch`) is all an update does.
- **Never writes ALSA mixer controls.** Some of them default to 0 and would silence the mic.
- **Never writes WirePlumber/PipeWire configuration or the `filters` metadata.** Valve's smart
  filters are global; enabling or disabling them would change the microphone for every app.
- **Never changes the default source** and never remaps anything system-wide.

## License

Apache-2.0; see [`LICENSE`](LICENSE). Third-party notices, including the OpenVR SDK header (BSD-3-Clause),
the bundled Inter font (SIL OFL 1.1) and every Rust crate, are in
[`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md).
