# KioskGameMenu

A lightweight, fullscreen, gamepad-controlled game launcher for Linux kiosks,
written in Rust. It runs directly on a Linux virtual terminal through
DRM/KMS, with no X11, Wayland or desktop environment, and hides RetroArch,
MAME, DOSBox and other emulators behind a simple library of cover art.

Players pick a game with a controller. The emulator starts directly, and when
it exits the menu comes back. This is a native program, not a web application.

- [How it works](#how-it-works)
- [Requirements](#requirements)
- [Installation on Arch Linux](#installation-on-arch-linux)
- [Compilation](#compilation)
- [Dependency inspection](#dependency-inspection)
- [Running](#running)
- [Controls](#controls)
- [Configuration](#configuration)
- [Music, sounds and artwork](#music-sounds-and-artwork)
- [Adding games](#adding-games)
- [Statistics](#statistics)
- [Kiosk operation](#kiosk-operation)
- [Testing without an emulator](#testing-without-an-emulator)
- [Troubleshooting](#troubleshooting)
- [Verification status](#verification-status)
- [Limitations](#limitations)
- [License](#license)

## How it works

Only one process can drive a DRM/KMS display at a time, because only one can
hold *DRM master*. Hiding or minimising a window does not release it, so the
launcher fully shuts down SDL before any game starts.

All SDL objects belong to a single function, `run_session`. When it returns,
they have all been dropped, `SDL_Quit` has run, the DRM file descriptor has been
closed, and the display is free. Nothing else in the program holds a graphics
resource. The sound device is released the same way, which matters on plain ALSA,
where only one program can use it at a time. As a safety check, the launcher then
inspects `/proc/self/fd` and logs a warning if any `/dev/dri`, `/dev/input` or
`/dev/snd` device is still open.

| Module | Responsibility | Needs SDL |
| --- | --- | --- |
| `main.rs` | Command line, launch flow for both modes, exit codes | No |
| `config.rs` | `config.toml` parsing, validation and path resolution | No |
| `game.rs` | `game.toml` parsing, validation and discovery | No |
| `statistics.rs` | SQLite storage and schema migrations | No |
| `menu.rs` | Selection, sorting, scrolling and grid geometry | No |
| `launcher.rs` | Building and starting the game process | No |
| `gamepad.rs` | Controllers, dead zones and key repeat | Thin layer |
| `audio.rs` | Music and sound effects | Yes |
| `graphics.rs` | The SDL session, event loop and drawing | Yes |

### The two launch modes

Both start the same way: the game is **validated**, its statistics are
**committed**, the **launch sound** plays, and **SDL is shut down**. They differ
in what happens next.

| | Mode A: `keepLauncherRunning = true` | Mode B: `keepLauncherRunning = false` |
| --- | --- | --- |
| Game runs as | A child of the launcher | The same process, replacing the launcher through `exec` |
| Launcher during the game | Waiting, holding no display or input devices | Gone |
| When the game exits | Launcher reinitialises SDL and restores the menu | `scripts/kiosk-loop.sh` starts a new launcher |
| Selection afterwards | Same game, same sort mode | Same game (the most recently launched), sort mode from `config.toml` |
| Failure to start the game | Shown in the menu | Logged, launcher exits nonzero, supervisor restarts it |
| If the display cannot be restored | Retries 5 times one second apart, then exits with status 4 | Not applicable: a fresh launcher starts |

A game that fails validation (missing executable, missing ROM) never reaches
either path. The menu stays up and shows the error.

Mode A is the default and shows errors more helpfully. Mode B leaves nothing of
the launcher in memory while the game runs, which suits small machines.

## Requirements

- Linux with a DRM/KMS graphics driver, on a local virtual terminal
- Rust stable, 1.85 or newer
- SDL2 with `kmsdrm` support, plus SDL2_image, SDL2_ttf and SDL2_mixer
- A working EGL/GBM stack (Mesa on most systems)
- A C compiler, used once at build time to compile the bundled SQLite

SQLite is compiled into the binary, so no system SQLite library is needed.

## Installation on Arch Linux

Arch no longer packages classic SDL2. The `sdl2-compat` package provides the
SDL2 API on top of SDL3, and it is what `sdl2` dependencies resolve to.

To build and run on the same machine:

```sh
sudo pacman -S --needed base-devel rust sdl2-compat sdl2_image sdl2_ttf sdl2_mixer libdrm mesa libglvnd
```

### Build-time and runtime dependencies

| Package | Build | Run | Purpose |
| --- | --- | --- | --- |
| `rust` | Yes | | `rustc` and `cargo` |
| `base-devel` | Yes | | C compiler and linker |
| `sdl2-compat` (pulls in `sdl3`) | Yes | Yes | Graphics and controllers |
| `sdl2_image` | Yes | Yes | PNG artwork |
| `sdl2_ttf` | Yes | Yes | Text |
| `sdl2_mixer` | Yes | Yes | Music and sound effects. Pulls in `opusfile`, `libvorbis`, `mpg123` and `flac`, so every format below works with no extra packages |
| `libdrm`, `mesa`, `libglvnd` | | Yes | DRM/KMS, GBM and EGL/GLES |

The last row deserves attention. SDL3 opens `libdrm.so.2`, `libgbm.so.1`,
`libEGL.so.1` and `libGLESv2.so.2` at run time with `dlopen`. They are not
declared dependencies of the `sdl3` package and **`ldd` will not list them**. On
a stripped-down kiosk they can be missing even though `ldd` looks perfect, and
the symptom is simply `kmsdrm not available`.

`procps-ng` (`pgrep`, `pkill`) and `util-linux` (`setterm`) are used by the
supervisor script and are normally already installed.

## Compilation

```sh
cargo build            # debug build:   target/debug/kioskgamemenu
cargo build --release  # release build: target/release/kioskgamemenu
```

Use the release build on a kiosk. Run the checks with:

```sh
cargo fmt --check
cargo check
cargo test
```

The tests need neither a display nor a controller, but the test binaries still
link SDL, so the SDL libraries must be installed.

## Dependency inspection

List the shared libraries the binary links against:

```sh
ldd target/release/kioskgamemenu
```

Expect `libSDL2-2.0.so.0`, `libSDL2_image-2.0.so.0`, `libSDL2_ttf-2.0.so.0` and
`libSDL2_mixer-2.0.so.0`.
Anything shown as `not found` must be installed. There should be no
`libsqlite3`, because SQLite is built in. As explained above, the DRM and GBM
libraries will not appear. To check those separately:

```sh
ls -l /usr/lib/libdrm.so.2 /usr/lib/libgbm.so.1 /usr/lib/libEGL.so.1 /usr/lib/libGLESv2.so.2
```

Inspect the binary itself with:

```sh
file target/release/kioskgamemenu
```

It should report a 64-bit, dynamically linked ELF executable.

## Running

Switch to a text console (for example with `Ctrl+Alt+F2`), log in, and run:

```sh
./target/release/kioskgamemenu --config config.toml
```

`display.backend` in `config.toml` already selects `kmsdrm`. To override it for
one run, set `SDL_VIDEODRIVER`, which takes priority over the configuration:

```sh
SDL_VIDEODRIVER=kmsdrm ./target/release/kioskgamemenu --config config.toml
```

Before starting the menu, you can check the whole setup from any terminal:

```sh
./target/release/kioskgamemenu --check --config config.toml
```

This reports every game that cannot start, and launches nothing.

Without `--config`, the launcher uses `./config.toml`, then `config.toml` beside
the binary.

Press `Ctrl+C` on the terminal to quit, or send `SIGTERM`. The menu deliberately
has no quit button, because ordinary players should not reach a console.

### Access requirements

**Foreground TTY.** KMSDRM needs DRM master, so the launcher must run on the
active virtual terminal, with no display server using the same GPU. It will not
work from SSH while a graphical session owns the display, or from a terminal
window inside a desktop.

**DRM device.** The user needs access to `/dev/dri/card*`, normally by being
logged in on the local seat, or through the `video` group:

```sh
sudo usermod -aG video kiosk
```

**Sound device.** ALSA devices are `/dev/snd/*`. A locally logged-in user normally
receives access automatically, but only while their session is on the active
virtual terminal. Otherwise add the user to the `audio` group.

**Input devices.** Controllers appear as `/dev/input/event*`. A locally logged-in
user normally receives access automatically. Otherwise add the `input` group:

```sh
sudo usermod -aG input kiosk
```

Log out and back in after changing groups. Check with `id` and:

```sh
ls -l /dev/dri/ /dev/input/
```

Do not run the launcher as root. Emulators would inherit root too.

## Controls

| Action | Controller | Keyboard |
| --- | --- | --- |
| Move | D-pad or left stick | Arrow keys |
| Previous / next page | LB / RB | Page Up / Page Down |
| Launch selected game | A (south) | Enter |
| Change sort mode | Y (north) or Back | Tab |

Holding a direction repeats after a short delay. Buttons are read through SDL's
semantic controller mapping, so A means the south face button on any mapped
controller, whatever it is labelled.

The keyboard support exists for setup and for when no controller is connected.

Controllers can be connected or removed at any time. The launcher does not
intercept input while a game runs, so exiting a game is up to the emulator's own
configuration, such as a RetroArch hotkey combination.

Launching is ignored for the first 0.75 seconds after the menu appears, so that
buttons still being pressed as an emulator closes do not start a game again.

## Configuration

`config.toml` is read at startup. Every setting is optional and unknown keys are
errors, so a typing mistake cannot be silently ignored. Relative paths are
resolved against the directory containing `config.toml`, never against the
current directory.

```toml
[launcher]
keepLauncherRunning = true

[display]
backend = "kmsdrm"
fullscreen = true
font = "assets/fonts/Lato-Regular.ttf"

[storage]
database = "data/stats.sqlite"

[games]
directory = "games"
platforms_directory = "platforms"

[menu]
sort = "name_asc"

[audio]
enabled = true
music = "assets/music.opus"
select_sound = "assets/select.opus"
launch_sound = "assets/launch.opus"
music_volume = 50
effects_volume = 75

[appearance]
background = "assets/background.jpg"
banner = "assets/banner.png"
background_dim = 40
```

| Setting | Default | Meaning |
| --- | --- | --- |
| `launcher.keepLauncherRunning` | `true` | `true` selects Mode A, `false` selects Mode B. See [the two launch modes](#the-two-launch-modes). The spelling is exact. |
| `display.backend` | `"kmsdrm"` | SDL video driver name, or `"auto"` to let SDL choose. `SDL_VIDEODRIVER` overrides it. |
| `display.fullscreen` | `true` | Use the display's current mode. `false` opens a 1280x720 resizable window, useful when developing on a desktop with `backend = "x11"` or `"wayland"`. |
| `display.font` | `"assets/fonts/Lato-Regular.ttf"` | TrueType font. If unreadable, common system fonts are tried, and failing that the menu runs without text. Use a font covering your titles' alphabets. |
| `storage.database` | `"data/stats.sqlite"` | SQLite file. Its directory is created when missing. |
| `games.directory` | `"games"` | Directory containing one subdirectory per game. |
| `games.platforms_directory` | `"platforms"` | Directory containing `<platform>.png` icons. |
| `menu.sort` | `"name_asc"` | Initial order: `name_asc`, `name_desc`, `most_played` or `recently_played`. |
| `audio.enabled` | `true` | `false` leaves the sound device completely alone. |
| `audio.music` | `"assets/music.opus"` | Background music. |
| `audio.select_sound` | `"assets/select.opus"` | Played when the selection moves. |
| `audio.launch_sound` | `"assets/launch.opus"` | Played when a game starts. |
| `audio.music_volume` | `50` | From 0 to 100. |
| `audio.effects_volume` | `75` | From 0 to 100, for both effects. |
| `appearance.background` | `"assets/background.jpg"` | Wallpaper. |
| `appearance.banner` | `"assets/banner.png"` | Title banner. |
| `appearance.background_dim` | `40` | Darkens the wallpaper, from 0 (none) to 100 (black). |

A percentage outside 0 to 100 is corrected to the nearest valid value with a
warning, rather than stopping the kiosk.

## Music, sounds and artwork

All of this is optional and read from disk at startup, so files can be replaced
without recompiling. Replace a file, or point `config.toml` at a new one, and
restart the launcher. In Mode A the audio files are also reloaded every time the
menu returns from a game. The project ships no music, sound or wallpaper files.

Leaving a file out only removes that one item. So does an unreadable file, or
an empty path such as `music = ""`, and an invalid file also logs a warning. If no
sound file exists at all, the sound device is never opened.

### Audio formats

All of these were tested on Arch Linux with `sdl2_mixer` 2.8.2, for both music and
effects, with no additional packages:

| Format | Extension | Notes |
| --- | --- | --- |
| Opus | `.opus` | **Recommended for music.** Small, and needs no resampling at the launcher's 48 kHz output. |
| Ogg Vorbis | `.ogg` | Good for music too. |
| MP3 | `.mp3` | Works through mpg123. |
| FLAC | `.flac` | Lossless, but large for music. |
| WAV | `.wav` | **A good choice for effects**: no decoding. |

Other SDL2_mixer builds may differ. The log reports what was used, for example
`audio ready, decoders loaded: CMD WAVE OPUS`, and a format that cannot be decoded
produces a `cannot load` warning naming the file. Note that the list only shows
decoders in use by the configured files, not everything available.

To convert a file with ffmpeg:

```sh
ffmpeg -i theme.flac -c:a libopus -b:a 96k assets/music.opus
```

### How audio behaves

- **Music** starts when the menu appears and loops. Navigating never restarts it.
- **The selection sound** plays only if the selected game really changed, so
  pushing against the edge of the list is silent. It has one channel of its own and
  each play restarts it, so fast scrolling cannot pile up. It never interrupts the
  music.
- **Launching** fades the music over 0.3 seconds while the launch sound plays. The
  launcher waits for that sound to finish, for at most 2 seconds, then closes the
  sound device. Keep it under 2 seconds, or it is cut off. Without a launch sound
  the music stops at once and there is no delay.
- **If a game fails validation** there is no launch sound and the music continues.
- **Mode A:** the game runs with no audio held by the launcher. When it exits, audio
  reopens and the music starts again from the beginning.
- **Mode B:** audio is fully closed before `exec`. The restarted launcher starts the
  music from the beginning.

If the sound device cannot be opened, the launcher logs a warning and runs silently.

### Wallpaper and banner

PNG and JPEG are both supported, through SDL2_image. Each image is loaded once per
menu session, and again only if the graphics driver loses its textures.

**The wallpaper** covers the whole screen. Its aspect ratio is preserved and any
excess is cropped equally from both sides, so it is never stretched. Use 1920x1080
or larger, at the display's aspect ratio. Keep important detail near the centre if
it may be shown on other screens. Images are limited to 3840 pixels on the long
side, and larger ones are scaled down.

When a wallpaper is showing, the header, the footer and each game's title get a dark
backing strip so that text stays readable. Raise `background_dim` for busy images.

**The banner** sits at the top left of the screen, scaled to fit while preserving
its aspect ratio, and transparency is honoured. Use a wide PNG with a transparent
background, around 4:1, such as 1200x300. The header grows to make room, which
leaves slightly less space for covers, and the selected game's title moves to the
right. It never overlaps the covers. Without a banner the normal header is used.

## Adding games

Create a subdirectory under `games/` containing a `game.toml`, then restart the
launcher. No recompilation is needed.

```text
games/
└── chrono-quest/
    ├── game.toml
    └── cover.png
```

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | Yes | Stable unique ID using letters, digits, `-`, `_`, `.`. Statistics are stored against it. Changing it starts fresh statistics. |
| `title` | Yes | Name shown in the menu. |
| `platform` | Yes | Selects `platforms/<platform>.png`. A text badge is shown if that is missing. |
| `executable` | Yes | Program to run. |
| `cover` | No | Cover image, default `cover.png`. A generated placeholder is shown if it is missing or unreadable. PNG is recommended. |
| `args` | No | List of arguments. Each entry is exactly one argument. |
| `working_directory` | No | Directory to run in, default the game directory. |
| `check_paths` | No | Default `true`. Set to `false` if an absolute-path argument may legitimately not exist yet, such as an output file. |
| `[env]` | No | Extra environment variables. This table must come after the fields above. |

Games are never run through a shell. Spaces need no quoting, and `$`, `*` and
quotes have no special meaning.

### Path rules

| Value | How it is resolved |
| --- | --- |
| `cover`, `working_directory` | Absolute, or relative to the game directory. |
| `executable` starting with `/` | Used as given. |
| `executable` containing a `/`, such as `bin/run` | Relative to the game directory. |
| `executable` with no `/`, such as `retroarch` | Looked up on `PATH`. |
| `args`, `[env]` values | Passed literally. `{game_dir}` expands to the absolute game directory. |

So a ROM kept beside its `game.toml` is written `"{game_dir}/game.sfc"`.

### What is checked, and when

At **startup**, each definition's structure is checked. An invalid definition is
logged and skipped, and all others still load.

At **launch**, the executable, the working directory, and every argument that is
an absolute path must exist. A failure is shown in the menu. The argument check
matters because an emulator given a missing ROM tends to open its own interface,
which is what a kiosk must hide. Relative arguments and arguments like
`--rom=/path` cannot be recognised as paths and are not checked.

### Examples

**All paths below are examples and depend on your system.** The executable and
core paths match the current Arch Linux packages (`retroarch`, `libretro-snes9x`,
`libretro-beetle-psx`, `mame`, `dosbox`). Your ROM locations will differ. Each
emulator must also already work on your TTY.

RetroArch, SNES:

```toml
id = "super-mario-world"
title = "Super Mario World"
platform = "snes"
cover = "cover.png"
executable = "/usr/bin/retroarch"
args = [
    "-f",
    "-L",
    "/usr/lib/libretro/snes9x_libretro.so",
    "/games/snes/Super Mario World.sfc"
]
```

RetroArch, PlayStation (`mednafen_psx_hw_libretro.so` from
`libretro-beetle-psx-hw` is the hardware-rendered alternative):

```toml
id = "tekken-2"
title = "Tekken 2"
platform = "ps1"
cover = "cover.png"
executable = "/usr/bin/retroarch"
args = [
    "-f",
    "-L",
    "/usr/lib/libretro/mednafen_psx_libretro.so",
    "/games/ps1/Tekken 2.cue"
]
```

MAME. The ROM is given by short name, so only `-rompath` is path-checked. The
directory must hold `sf2.zip`:

```toml
id = "street-fighter-2"
title = "Street Fighter II"
platform = "arcade"
executable = "/usr/bin/mame"
args = ["sf2", "-rompath", "/games/arcade"]
```

DOSBox, with its own working directory and environment:

```toml
id = "doom"
title = "Doom"
platform = "dos"
executable = "/usr/bin/dosbox"
args = ["/games/dos/doom/DOOM.EXE", "-fullscreen", "-exit"]
working_directory = "/games/dos/doom"

[env]
SDL_AUDIODRIVER = "alsa"
```

A script or any other native program works the same way. Give scripts a `#!`
line (see [Limitations](#limitations)).

## Statistics

Statistics are stored in the SQLite file at `storage.database`, created
automatically. Game definitions are never modified.

```sql
game_stats (
    game_id          TEXT PRIMARY KEY,
    launch_count     INTEGER,
    last_launched_at INTEGER  -- Unix time in seconds
)
```

The schema version is kept in `PRAGMA user_version`, and new versions are added
as migrations. A database written by a newer version is refused rather than
risk damaging it.

**What a launch means.** The count rises by exactly one, in a committed
transaction, after the game passes validation and before it starts. It therefore
records launch *attempts*: a game that later crashes or fails to start has still
been counted. A game failing validation is not counted.

**If the database is unavailable,** the game still launches. Statistics are
never a reason to refuse a game. A warning is logged, and the launch is lost from
disk. No connection stays open while a game runs, so the next launch simply tries
again.

Play time is not tracked.

### Sort orders

The Y button cycles through these, keeping the same game selected.

| Mode | Primary order | Ties broken by |
| --- | --- | --- |
| Name A-Z | Title, ignoring case | ID |
| Name Z-A | Title, ignoring case, reversed | ID |
| Most played | Launch count, highest first | Most recent, then title, then ID |
| Recently played | Latest launch first, unplayed last | Launch count, then title, then ID |

## Kiosk operation

`scripts/kiosk-loop.sh` keeps the launcher running. It finds its files from its
own location, so it works from any directory.

```text
              ┌───────────────────────────────────────┐
              ▼                                       │
  kiosk-loop.sh ──▶ kioskgamemenu                     │
                       │ Mode A: game runs as a child │ fatal error
                       │ and the menu returns, in a   ├──────────────
                       │ loop, within this process    │
                       │                              │
                       │ Mode B: exec replaces the    │ game exits
                       └──▶ launcher with the game ───┘
```

In Mode B the loop is essential: it is what brings the menu back. In Mode A it
recovers from fatal errors such as a display that could not be restored.

To install under `/opt/kioskgamemenu`:

```sh
sudo install -d -o kiosk /opt/kioskgamemenu
cp -r target/release/kioskgamemenu config.toml assets games platforms scripts /opt/kioskgamemenu/
mkdir -p /opt/kioskgamemenu/data
```

Run it from the kiosk's TTY:

```sh
/opt/kioskgamemenu/scripts/kiosk-loop.sh
```

Output from the launcher and games goes to `data/kiosk.log`, which is rotated to
`kiosk.log.old` past 5 MB. Watch it from another console or over SSH.

If the launcher fails within 5 seconds, the delay before the next start doubles,
up to 30 seconds, so a broken setup does not spin. The optional environment
variables are listed at the top of the script.

### Stopping the loop

From another shell, such as SSH:

```sh
/opt/kioskgamemenu/scripts/kiosk-loop.sh stop
```

This ends the running game and the menu, then waits for the loop to exit. On the
kiosk's own terminal, `Ctrl+C` does the same if a keyboard is attached. Do not
kill the launcher directly during a game in Mode A, as the game would be left
running with no launcher.

### Starting automatically (example)

Nothing in this project enables a service. To start the loop on `tty1` for a
dedicated `kiosk` user later, one approach is a getty autologin override:

```sh
sudo systemctl edit getty@tty1
```

```ini
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin kiosk --noclear %I $TERM
```

and at the end of the `kiosk` user's `~/.bash_profile`:

```sh
if [ "$(tty)" = /dev/tty1 ]; then
    /opt/kioskgamemenu/scripts/kiosk-loop.sh
fi
```

The script is deliberately not started with `exec`, so that after `stop` the login
shell remains. With `exec`, the session would end and autologin would start the
loop again. Test this with a keyboard and a second way in, such as SSH, before relying on it.

### Exit status

| Status | Meaning |
| --- | --- |
| 0 | Quit normally, on `SIGINT` or `SIGTERM` |
| 1 | `--check` found problems, or `--launch` failed validation |
| 2 | Bad command line, unknown game, or invalid configuration |
| 3 | SDL or the display could not be initialised at startup |
| 4 | The display could not be restored after a game (Mode A) |
| 126, 127 | The game could not be executed (127: file or interpreter not found) |

In Mode B a successful `exec` means the game *is* the process, so the game's own
exit status is what the supervisor sees, and a game killed by a signal appears as
128 plus the signal number. The script logs each status and restarts either way.

## Testing without an emulator

`testdata/fake/` holds harmless games built on `/usr/bin/sleep`. These commands
need no display, because `--launch` goes through the menu's launch path without
any graphics:

```sh
cargo build --release
BIN=target/release/kioskgamemenu

# Mode A: 5 seconds, launcher stays alive as the parent
$BIN --config testdata/fake/child.toml --launch sleep-5s
echo "status: $?"

# Mode B: same result, but the process becomes `sleep`.
# In another terminal, `pgrep -a sleep` shows the launcher is gone.
$BIN --config testdata/fake/exec.toml --launch sleep-5s
echo "status: $?"

# Validation failure: nothing runs, status 1
$BIN --config testdata/fake/child.toml --launch missing-rom

# Everything at once. The missing-* games are expected to fail.
$BIN --config testdata/fake/child.toml --check
```

Fake statistics are kept separately, in `data/fake-stats.sqlite`.

### Manual test on real hardware

Run these on a text console of the Arch machine.

**Mode A:**

1. Start the menu with `target/release/kioskgamemenu --config testdata/fake/child.toml`.
2. Confirm four games appear, and that the D-pad, the left stick and LB/RB move.
   Press Y and confirm the selection stays on the same game.
3. Select *Missing Emulator* and press A. A red message appears and the menu keeps
   working. Repeat with *Missing ROM*.
4. Select *Sleep Five Seconds* and press A. The menu closes and the console shows
   for 5 seconds. The menu then returns with the same game selected.
5. Select *Crashes With Status 1* and press A. The menu returns showing the status.
6. Repeat step 4 several times, unplugging and replugging the controller during
   the wait. Confirm the logs have no `still open after SDL shutdown` warnings.

**Mode B:**

1. Point the supervisor at the fake games and start it:
   `KIOSK_CONFIG=$PWD/testdata/fake/exec.toml scripts/kiosk-loop.sh`
2. Launch *Sleep Five Seconds*. From SSH, `pgrep -a kioskgamemenu` shows nothing and
   `pgrep -a sleep` shows the game.
3. After 5 seconds the loop restarts the menu with that game selected.
4. Stop with `scripts/kiosk-loop.sh stop`, and check `data/kiosk.log`.

Then repeat both with a real emulator, confirming it receives the display and that
your exit combination returns to the menu.

## Troubleshooting

Start with the log, which is the terminal output or `data/kiosk.log` under the
supervisor, and with `kioskgamemenu --check`.

### No display

- Make sure you are on the active text console and not in SSH or a desktop terminal.
- Stop any display manager on the same GPU, for example
  `sudo systemctl stop gdm`, or whichever one is installed.
- Check that KMS is active: `ls /sys/class/drm/` should list connectors like
  `card0-HDMI-A-1`, and `cat /sys/class/drm/card0-*/status` should show one
  `connected`.

### The menu runs but the console stays on screen

If the log shows `display ready` and the music plays, but the screen does not change,
the display is rejecting the framebuffer. The log says
`the display rejected the first frame: Could not set videomode on CRTC`.

SDL 3.4 always asks for an ARGB framebuffer, and some display planes do not accept
one. `virtio-gpu`, as used by QEMU and libvirt virtual machines, accepts only XRGB.
To check what a plane accepts:

```sh
modetest -M virtio_gpu -p | grep formats
```

The launcher cannot change this through SDL. It needs a small `LD_PRELOAD` shim that
rewrites the format, and it must apply to the whole launcher. If your system has
such a wrapper, for example one named `sdl-kms-run`, start the launcher through it:

```sh
sdl-kms-run ./kioskgamemenu --config config.toml
sdl-kms-run scripts/kiosk-loop.sh
```

Emulators inherit the shim, which is harmless to those tested. No shim is included
in this project.

### Failed DRM/KMS initialization

`kmsdrm not available` (exit status 3) has several possible causes:

- Runtime libraries missing, which `ldd` cannot reveal. See
  [Dependency inspection](#dependency-inspection), and install `libdrm mesa libglvnd`.
- Another process holds DRM master, usually a display server, or a game that has
  not exited. Find it with `sudo fuser -v /dev/dri/card*`.
- No permission on `/dev/dri/card*`. See [Access requirements](#access-requirements).
- The system has more than one GPU. Try `SDL_KMSDRM_DEVICE_INDEX=1`.
- With the proprietary NVIDIA driver, kernel mode setting must be enabled
  (`nvidia_drm.modeset=1`) and `egl-gbm` installed. This configuration has not
  been tested with this project.

To see SDL's own diagnostics, set `SDL_LOGGING=video=debug` (SDL3, as used through
`sdl2-compat` on Arch).

### No gamepad detected

The menu shows `No controller connected` when none is usable.

- Confirm the kernel sees it: `ls -l /dev/input/by-id/`.
- Confirm read access to its `/dev/input/event*` device.
- A log line saying `has no SDL controller mapping` means SDL does not know the
  device. Supply a mapping through `SDL_GAMECONTROLLERCONFIG` or
  `SDL_GAMECONTROLLERCONFIG_FILE`, for example a file from the community
  `gamecontrollerdb.txt` project.

### No sound

- Find the `audio ready` line in the log, or a warning explaining why not. Remember
  that a missing file is only logged as information.
- Test the device outside the launcher: `speaker-test -c 2 -t sine -l 1`.
- Check `amixer` for a muted or low output. The launcher does not change system
  volume.
- `audio unavailable` or `cannot open sound device` means the device is missing or
  busy. Check permissions, see [Access requirements](#access-requirements), and
  find what is using it with `fuser -v /dev/snd/*`.
- To select the driver, set `SDL_AUDIODRIVER`, for example to `alsa`.

### Emulator has no sound

On plain ALSA the emulator needs the device the launcher has just released. A log
line `still open after SDL shutdown` naming `/dev/snd` would be a launcher bug.
Otherwise check the emulator's own audio driver setting.

### Missing shared libraries

An error like `libSDL2_ttf-2.0.so.0: cannot open shared object file` means a
library named by `ldd` as `not found` is absent:

```sh
ldd target/release/kioskgamemenu | grep 'not found'
```

Install the matching package from the [dependency table](#build-time-and-runtime-dependencies).
A `Failed loading SDL3 library` message means `sdl3` is missing or incompatible.

### Emulator fails to start

- Run `kioskgamemenu --check` to see what is wrong with each game.
- Run the emulator by hand from the same TTY, using the same arguments and as the
  same user. This separates launcher problems from emulator problems.
- If the emulator's own interface appears, its ROM or core is probably wrong.
- Read the emulator's output in `data/kiosk.log`.

### Menu does not return

- **Mode A:** the launcher waits for the process it started to exit. If that
  process has started another and exited, the menu may return too early. If
  the emulator is still running but invisible, its exit combination is not
  configured, or it needs another way to exit.
- **Mode A, exit status 4:** the display could not be taken back after 5 attempts.
  The log says why, and the supervisor restarts the launcher.
- **Mode B:** the loop must be running. Without it, nothing restarts the menu.
- If `still open after SDL shutdown` appears in the log, please treat it as a bug.

### SQLite errors

Statistics problems never stop a game, and show as log warnings.

- `unable to open database file`: check that the user can write to the directory
  containing the database. SQLite needs this for its journal file.
- `database is locked`: another program has it open. The launcher waits 2 seconds.
- `schema version N is newer`: an older build is reading a newer database. Upgrade.
- To start over, delete the database file. Only statistics are lost.

### Permission errors

- Check `id` and `ls -l /dev/dri/ /dev/input/`. See [Access requirements](#access-requirements).
- Group changes take effect only after logging in again.
- `Not an executable file` means the file needs `chmod +x`.
- The application directory must be writable for `data/`.

## Verification status

**Tested on Arch Linux** in a libvirt virtual machine: `virtio-gpu` KMS, an emulated
HDA sound card on plain ALSA with no PipeWire, `sdl2-compat` 2.32.74 on SDL 3.4.18,
and `sdl2_mixer` 2.8.2, run on a text console through the XRGB wrapper described in
[Troubleshooting](#the-menu-runs-but-the-console-stays-on-screen). The binary was
built on Debian 13 with Rust 1.85. Verified by reading DRM and ALSA state from the
kernel, by screenshots of the virtual display, and by analysing the mixed audio
written to a file:

- The menu, wallpaper, banner and fallbacks on real KMSDRM
- Launching real RetroArch with the snes9x core in both modes, with the display and
  sound device passing to RetroArch and back, and no devices left open
- Mode B and restarting through `scripts/kiosk-loop.sh`
- Selection sounds, launch sound, music fade and music restart in the mixed output
- All listed audio formats, and every missing or invalid asset case
- Navigation and hotplug through a virtual controller

**Not yet tested:**

- Physical GPUs. Only `virtio-gpu` has been used, and it needs the wrapper above.
- Audio heard through real speakers. Output was verified up to the sound device.
- Pressing buttons on a physical controller. A real one was detected, but
  input came from a virtual device.
- Exiting RetroArch with a controller combination or keyboard. The tests ended it
  with `SIGTERM`.
- MAME, DOSBox, and PipeWire or PulseAudio setups, launched through the menu.

## Limitations

Current behaviour worth knowing:

- Games are discovered only at startup, so adding one requires a restart.
- Mode B forgets a sort mode chosen with the controller when the menu restarts.
- In Mode A only the directly started process is waited for. A wrapper that starts
  an emulator in the background and exits returns to the menu too early, and the
  launcher will then fight the emulator for the display.
- In Mode B, an `exec` failure cannot be shown on screen, only logged, because the
  menu has already shut down. Use `--check` to find these before going live.
- Text files marked executable but lacking a `#!` line fail to start in Mode A, but
  in Mode B are run by `/bin/sh`, which is how `execvp` behaves. Always use `#!`.
- The launcher may briefly show the text console while switching to a game.
- The screen does not update during the launch sound, for at most 2 seconds.
- Music restarts from the beginning after a game. It does not resume.
- Artwork is decoded on the main thread, limited to two images per frame. Covers
  appear progressively when scrolling fast through a large library.
- `{game_dir}` expansion assumes the game directory path is valid UTF-8.
- Only the first display is used.

Deferred to future versions:

- Play-time tracking
- Reloading the library without a restart
- Search, favourites, and filtering by platform
- Remembering the sort mode across restarts
- Background artwork decoding, and animations
- Configurable controls
- A systemd service

## License

KioskGameMenu is licensed under the GNU General Public License version 3. See
[LICENSE](LICENSE).

`assets/fonts/Lato-Regular.ttf` is Lato 2.015 by Łukasz Dziedzic, under the SIL
Open Font License 1.1. See [assets/fonts/OFL.txt](assets/fonts/OFL.txt).

The cover images and platform icons in this repository are plain generated
placeholders made for this project, not original game artwork. Game and platform
names are the property of their respective owners, and no games or ROMs are
included.
