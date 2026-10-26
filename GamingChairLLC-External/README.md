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

The first build downloads and compiles the dependencies, so it can take a few
minutes. Later builds are incremental.

### Development build

```powershell
cargo run
```

`DEBUG` in `src/config.rs` enables WebView2 devtools (right-click → **Inspect**)
and verbose logging. It is currently `true` for every profile, including
`--release`; set it to `false` and rebuild to ship a build without devtools.

### Tests

```powershell
cargo test
```

---

## Packaging / distributing

The executable is not self-contained: the `resources` folder (UI, icons,
assets) must be next to it. Everything the app *writes* (settings, profiles,
offsets cache, logs) goes to `%LOCALAPPDATA%\GamingChairLLC` — see
[Where your data lives](#where-your-data-lives) — so the program folder itself
can be read-only.

A ready-to-zip layout:

```powershell
cargo build --release

Remove-Item dist -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory dist | Out-Null
Copy-Item target\release\GamingChairLLC.exe dist\
Copy-Item resources dist\resources -Recurse

Compress-Archive -Path dist\* -DestinationPath GamingChairLLC.zip -Force
```

`dist` is recreated from scratch each time, so re-running the script cannot
nest a second `resources` folder inside the first.

`GamingChairLLC.zip` now contains:

```text
GamingChairLLC.exe
resources\
```

Unzip it anywhere and run `GamingChairLLC.exe`. The app looks for `resources`
in the working directory and next to the executable, so both "run from the
project folder" and "run from an extracted zip" work.

If Windows reports a missing `VCRUNTIME140.dll` on another PC, install the
[Microsoft Visual C++ Redistributable (x64)](https://aka.ms/vs/17/release/vc_redist.x64.exe).
WebView2 must be present there as well (see Requirements).

---

## Usage

1. Start Roblox and join a game.
2. Run `GamingChairLLC.exe`.
3. Click **Attach**. The app downloads offsets for the running game version
   (or reuses the cached copy in `%LOCALAPPDATA%\GamingChairLLC\data\offsets.json`)
   and waits until the game's DataModel is readable (up to 10 seconds).
4. Use the **Global** page to toggle cheats and the **Explorer** page to browse
   the object tree and inspect properties.
5. **Detach** (same button) restores any active cheat before closing the
   connection.

The status bar shows the app version/build and the attach state.

### Profiles

The **Profile** bar at the top of the Global page stores the *values* of your
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
    "walkspeed": { "speed": 16 },
    "noclip": {}
  }
}
```

You can back up the folder, copy it to another PC, or edit the JSON by hand
while the app is closed.

---

## Project layout

```text
src/
  main.rs            window + WebView + event loop wiring
  api.rs             JS-facing API dispatch
  backend/
    engine.rs        attach/detach, cheats, explorer, settings/profiles
    store.rs         settings.json, data.json and profiles (atomic writes)
    worker.rs        always-on backend loop (ticked cheats)
    explorer.rs      instance-tree/properties queries
    state.rs         shared state between the UI and the loop
  cheats/            cheat implementations (WalkSpeed, NoClip)
  sdk/
    memory.rs        process memory access + memory freezer
    hyperion.rs      Hyperion thread blinding
    offsets.rs       offsets download/cache
    roblox/          Roblox SDK (Instance, Humanoid, Part, Player, ...)
  web_server.rs      local HTTP server for resources/
  webview.rs         pywebview-compatible JS bridge
resources/
  web/               HTML/CSS/JS for the interface (loopback server, port 8765 by default)
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
changed. If no cache exists, check your connection to `offsets.imtheo.lol`.

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
