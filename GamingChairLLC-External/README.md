# GamingChairLLC

External Roblox tool with a WebView2 interface: attach to a running
`RobloxPlayerBeta.exe`, browse the instance tree, and toggle cheats.

Written in Rust. No Python installation is required.

---

## Requirements

| Requirement | Notes |
| --- | --- |
| Windows 10/11 (64-bit) | The app uses Win32 process APIs and WebView2. |
| [Rust](https://rustup.rs) stable, `x86_64-pc-windows-msvc` | `rustup default stable-msvc` |
| Visual Studio Build Tools | Install the **Desktop development with C++** workload (MSVC compiler + Windows SDK). |
| WebView2 Runtime | Preinstalled on Windows 11 and most Windows 10 machines. If the window stays blank, install the [Evergreen Runtime](https://developer.microsoft.com/microsoft-edge/webview2/). |
| Internet (first attach) | Offsets are downloaded for the running Roblox version and cached in `%LOCALAPPDATA%\GamingChairLLC\data\`. |

Check your toolchain:

```powershell
rustc --version
cargo --version
```

---

## Build a release executable

From this folder (`GamingChairLLC-External`):

```powershell
cargo build --release
```

The executable is written to:

```text
target\release\GamingChairLLC.exe
```

The UI is baked into the executable, so this one file is the whole app — no
`resources` folder ships with it.

The first build downloads and compiles the dependencies, so it can take a few
minutes. Later builds are incremental.

### How the resources are bundled

`build.rs` runs before the crate is compiled:

1. Copies `resources\` to a temporary staging folder under `target\...\out\`.
2. Patches the staged `web\index.html`: `__BUILD_TYPE__` becomes `Developer`
   (debug) or `Release` (release), and `__APP_VERSION__` becomes the package
   version.
3. Packs the staged tree into a single `resources.bin`.
4. Deletes the staging folder.

`src/bundle.rs` embeds that blob with `include_bytes!`, and the local HTTP
server and the window icon read from it. Change anything under `resources\` and
the next build re-bundles automatically.

### Development build

```powershell
cargo run
```

Debug builds keep WebView2 devtools (F12, Ctrl+Shift+I) and browser shortcuts
such as Ctrl+R. The default right-click menu is disabled in every build; a
custom menu will replace it later. Release builds also disable devtools and
browser accelerator keys.

`DEBUG` in `src/config.rs` controls verbose (DEBUG) logging.

### Tests

```powershell
cargo test
```

---

## Packaging / distributing

A release build is a single self-contained executable:

```powershell
cargo build --release
Copy-Item target\release\GamingChairLLC.exe dist\
```

Nothing else needs to be next to it. Everything the app *writes* (settings,
profiles, offsets cache, WebView2 profile, logs) goes to
`%LOCALAPPDATA%\GamingChairLLC` — see
[Where your data lives](#where-your-data-lives) — so the program folder itself
can be read-only.

The status bar (bottom-left) shows `Developer` for debug builds and `Release`
for release builds.

If Windows reports a missing `VCRUNTIME140.dll` on another PC, install the
[Microsoft Visual C++ Redistributable (x64)](https://aka.ms/vs/17/release/vc_redist.x64.exe).
WebView2 must be present there as well (see Requirements).

---

## Usage

1. Start Roblox and join a game.
2. Run `GamingChairLLC.exe`.
3. Click **Attach**. The app downloads offsets for the running game version
   from [rbxoffsets.com](https://rbxoffsets.com) (or reuses the cached copy in
   `%LOCALAPPDATA%\GamingChairLLC\data\offsets.json`) and waits until the game's
   DataModel is readable (up to 10 seconds).
4. Use the **Global** page for WalkSpeed, NoClip, Fly, Fling and Sweep, the
   **Combat** page for Aim Assist, and the **Explorer** page to browse the
   object tree and inspect properties.
5. **Detach** (same button) restores any active cheat before closing the
   connection.

The status bar shows the app version/build and the attach state.

### Profiles

The **Profile** bar at the top of the Settings page stores the *values* of your
global cheats (WalkSpeed speed, NoClip settings, …). Toggle state is never
saved: cheats always start off, so nothing is written to the game until you
switch it on.

| Action | What happens |
| --- | --- |
| Select a profile | Loads its saved values into the controls. If a cheat is currently enabled, the new value is applied to the game immediately. |
| **New** | Creates a profile from the values you are using right now and selects it. |
| **Delete** | Deletes the selected profile. The last remaining profile cannot be deleted. |

The active profile is remembered in `data.json`, so the next launch restores
both the selection and the control values.

### Fly, Fling, Sweep and Aim Assist

**Global → Fly** pins the character in place and moves it with `WASD`, `Space`
and `Ctrl`. Hold `Q` to accelerate; the ramp is raised to `Boost Power`. Speed,
floor margin and camera-facing are adjustable, and every position Fly writes is
clamped above the place's kill plane. The default bind is `V`.

**Global → Fling** tumbles the character on random angular velocity. Turning it
on turns Fly on automatically, and while Fling is on the Fly switch and keybind
are locked (the backend refuses to turn Fly off too). Turning Fling off stops
the tumble, zeroes the limbs and stands the character back up at the yaw it was
facing, without releasing the Fly freeze until it is upright. The default bind
is `E`.

**Global → Fling → Sweep** teleports to every other player in turn, waits
`Dwell` seconds on each, then returns to where the sweep started. It also turns
Fly on and claims the position channel, so `WASD` cannot fight the teleport.
Sweep is one-shot: it switches itself off after the last player. The default
bind is `R`.

**Combat → Aim Assist** eases the camera onto the nearest other player instead
of snapping to them. `Smoothing` is a time constant in seconds (`0` is
instant), `Strength` scales how much of the eased aim is applied, and
`Stickiness` is the hysteresis that stops two equally close players from
flickering. With **Screen Check** on, only players projecting within
**Screen Radius** pixels of the crosshair are eligible; off ranks everyone by
world distance. **Aim Part** picks `Head` or `HumanoidRootPart`. The default
bind is `C`.

Sweep and Aim Assist are also one row each in the profile values, so their
settings and binds follow the active profile like every other cheat.

### Keybinds

Each cheat has a **Keybind** row on the Global page (Aim Assist's is on the
Combat page). Click the button and
press a key (letters, digits, `F1`–`F24`, arrows, `Space`, `Ctrl`, `Shift`,
`Alt`, and the punctuation keys are supported). `Backspace` clears the bind,
`Escape` cancels. The bind is stored in that cheat's values, so it belongs to
the active profile and switches with it.

Binds are handled entirely in the Rust backend. A key toggles the cheat only
when **both** of these are true:

1. `RobloxPlayerBeta.exe` owns the foreground window, and
2. the in-game chat input bar is not focused
   (`ChatInputBarConfiguration.IsFocused`).

A press that happens while either condition fails is ignored. Keys are
edge-triggered — holding one down toggles once. Toggling from a keybind updates
the switch in the UI immediately.

---

## Where your data lives

All writable data lives in one folder:

```text
%LOCALAPPDATA%\GamingChairLLC\
  settings.json          UI preferences (theme, menu keybind)
  data.json              active profile
  profiles\
    Default.json         cheat values per profile
  data\
    offsets.json         cached offsets for the detected Roblox version
    last_checked_offsets.txt
  logs\
    gamingchairllc.log   rotating log file (about 2 MB × 3)
  webview\               WebView2 browser profile (cache, local storage)
```

`settings.json` is written with defaults on the first launch and then updated
automatically whenever you change a setting in **Settings**. The
**Settings → Advanced → App Data Folder** row shows the full path, and **Open
Folder** opens it in Explorer.

A profile file looks like this:

```json
{
  "version": 1,
  "name": "Default",
  "cheats": {
    "walkspeed": { "speed": 16, "keybind": "F1" },
    "noclip": { "keybind": "F2" },
    "fly": { "keybind": "V", "speed": 1, "boost_power": 4, "floor_margin": 10, "face_camera": true },
    "fling": { "keybind": "E", "speed": 1000 },
    "sweep": { "keybind": "R", "dwell": 0.5 },
    "aim": { "keybind": "C", "smoothing": 0.08, "strength": 1, "stickiness": 0.15, "screen_mode": true, "radius": 150, "aim_part": "head" }
  }
}
```

You can back up the folder, copy it to another PC, or edit the JSON by hand
while the app is closed.

---

## Project layout

```text
build.rs             stages, patches and packs resources/ into the executable
src/
  main.rs            window + WebView + event loop wiring
  api.rs             JS-facing API dispatch
  bundle.rs          embedded resources (parser, lookups, window icon)
  backend/
    engine.rs        attach/detach, cheats, explorer, settings/profiles
    store.rs         settings.json, data.json and profiles (atomic writes)
    worker.rs        always-on backend loop (ticked cheats)
    keybinds.rs      keybind polling, foreground/chat gates
    explorer.rs      instance-tree/properties queries
    state.rs         shared state between the UI and the loop
  cheats/            cheat implementations (WalkSpeed, NoClip, Fly, Fling, Sweep, Aim Assist)
  sdk/
    memory.rs        process memory access + memory freezer
    hyperion.rs      Hyperion thread blinding
    offsets.rs       rbxoffsets download/cache
    metadata.rs      Roblox web metadata (users, avatars, places)
    roblox/          Roblox SDK (Instance, Humanoid, Part, Player, ...)
  web_server.rs      local HTTP server for the bundled UI
  webview.rs         pywebview-compatible JS bridge
resources/           source files baked into the executable at build time
  web/               HTML/CSS/JS for the interface (served over loopback, port 8765 by default)
  icons/             UI and Roblox object icons
```

User data is **not** stored in the project folder; see
[Where your data lives](#where-your-data-lives).

---

## Troubleshooting

**The window is blank or never appears**
Install the WebView2 Evergreen Runtime (link above). Check
`%LOCALAPPDATA%\GamingChairLLC\logs\gamingchairllc.log` for details.

**`Attach failed: could not detect the Roblox version`**
`RobloxPlayerBeta.exe` is not running, or it was started with an unusual
install layout. Start Roblox first, then attach.

**Offsets download fails**
The app falls back to the cached copy in
`%LOCALAPPDATA%\GamingChairLLC\data\offsets.json` when the version has not
changed. If no cache exists, check your connection to `rbxoffsets.com`. The
service keeps the recent Windows builds only, so a very old client may not have
a dump available.

**Port 8765 is already in use**
The app automatically picks a free local port; the window still loads
normally.

**Antivirus flags the executable**
The tool opens another process's memory (`OpenProcess` / `NtReadVirtualMemory`),
which heuristic scanners commonly flag for game tools. Add an exclusion for the
folder if you trust the build.

**Where are the logs?**
`%LOCALAPPDATA%\GamingChairLLC\logs\gamingchairllc.log`. The file rotates at
about 2 MB, keeping three backups. **Settings → Advanced → Open Folder** opens
the folder containing it.

**A setting or profile will not load**
The JSON files are validated on read: invalid values fall back to the defaults
and are rewritten on the next save. Close the app, fix or delete the file, and
start it again — a missing profile is recreated from the current values.
