# Alixer - A Linux Mixer

A Rust mixing console for Linux, powered by PipeWire and WirePlumber with an
egui GUI.

![Alixer mixing console](img/screenshot.jpg)

- Channel strips for microphones, playback applications, groups, aux masters, and outputs.
- Per-channel routing, mute/solo, and pre/post-fader aux sends.
- Dedicated EQ, compressor and limiter editors, powered by LSP LV2 effects.
- A persistent audio service, independent of the GUI.
- A dark, customizable interface.

## Getting started

### Install an RPM

See the [RPM packaging guide](packaging/rpm/README.md) for builds, installation,
and per-user service activation. The first tested target is openSUSE Tumbleweed
x86_64.

### Run from source

```sh
cargo build --workspace
target/debug/alixer --dev-service
```

`--dev-service` starts the sibling `alixerd` binary and stops it when the GUI exits.
It refuses to take ownership of an already running service. Without this flag,
the GUI connects to the independently running service.

| Component | Executable / unit |
| --- | --- |
| Mixer GUI | `alixer` |
| Audio service | `alixerd` / `alixer.service` |
| Command-line client | `alixerctl` |

The service keeps audio running when the GUI closes. Its PipeWire graph sleeps
when no playback or connected, routed microphone inputs need processing. Metering
runs for GUI subscribers and explicit snapshot requests; unrouted microphone
preview connections are released when metering stops.

## Using the console

### Faders and layout

| Action | Control |
| --- | --- |
| Reset a fader to 0 dB | Double-click its handle |
| Adjust a fader at half speed | Hold Shift while dragging its handle |
| Zoom in / out / reset | `Ctrl` + `+` / `Ctrl` + `-` / `Ctrl` + `0` |
| Collapse a strip | Right-click its name or background → **Hide** |
| Restore a collapsed strip | Click **»** at its bottom |
| Rename a strip | Right-click its name or background → **Rename…** |
| Reorder strips | Drag a strip's name within its section |
| Restore default colors | **Changes** → **Reset channel colors** |

### Routing

The routing box at the top of each sending strip shows two destination pairs
per row (`□ □ | □ □`).

- Hover a checkbox to identify its destination and channel.
- Click to connect or disconnect that channel.
- **Show offline** reveals disconnected outputs.

Mono sources can feed both sides of a stereo destination. Output masters are
terminal: edit their incoming connections in the sending channel's routing box.

### Microphones and meetings

Microphones appear as amber **MIC / INPUT** strips. To use one in a meeting:

1. In its routing box, enable both channels of **Alixer Microphone** (hover to
   find them).
2. Select **Alixer Microphone** as the input device in your meeting application.
3. Use the microphone strip's fader, mute, and LV2 inserts to control your voice.

New microphones start unrouted. Route them to headphones only if you want to
monitor yourself; routing to speakers can cause feedback. The virtual microphone
works with the GUI closed and stays silent if the Alixer service stops.
Input faders control the Alixer signal; hardware microphone gain remains in your
system's input settings. Meeting applications using the hardware microphone
directly bypass Alixer's controls.

### Aux sends

The aux-send rack sits between routing and inserts.

1. Right-click empty space to create an aux master (`Aux1`, `Aux2`, …) and its
   send, or select an existing master. On a full rack, use **Add send…**.
2. Raise the send's horizontal fader. New sends start post-fader at −∞.
3. Route the aux master to the desired outputs. New masters have no outgoing
   routes selected.

Right-click a send to change between pre/post-fader mode or **Delete send**.
Deleting sends retains their master, even when none remain. To remove a master
and all its sends, right-click its bottom name → **Delete aux master**, or use
the same button in its inspector.

### LV2 inserts

Each strip has an insert rack above its buttons and fader.

- **Add:** right-click empty rack space → **Add effect…**, then choose Parametric EQ,
  Compressor or Limiter. Mono and stereo strips use the matching LSP implementation.
- **Edit:** click an insert to open its dedicated egui editor. Numeric values can
  be typed directly; double-click a slider to restore its default.
- **EQ:** select one of eight bands, drag its frequency/gain handle, and scroll
  over a handle to adjust Q. Filter type, algorithm, slope, width, solo and mute
  remain available. The plot shows band positions, not a simulated frequency response.
- **Dynamics:** processing, timing, detector and signal controls are grouped by
  function, with live input/output meters and gain-change history.
- **Manage:** right-click an insert to bypass, reorder, or remove it.

Install the LSP **LV2** package for source builds (`lv2-lsp-plugins` on openSUSE,
`lsp-plugins-lv2` on Fedora). RPM installations require it automatically. Alixer
supports LSP Parametric Equalizer x8, Compressor and Limiter in mono/stereo;
external sidechain inputs and LSP link routing are not exposed. Multichannel
strips retain mixing and routing but cannot add these mono/stereo effects.
Other effects in existing sessions remain playable, with a generic fallback editor.

## Outputs

### Master strips and meters

Each output has a master strip. Use groups and auxes to build reusable mixes
and feed several outputs.

The fader and mute control the same native device settings used by desktop volume
controls. Meters show audio routed through Alixer before device volume and mute.

### Renaming outputs

Rename an output in its inspector to set its display name across audio
applications, including pavucontrol.

1. Enter the new name.
2. Choose **Apply output names…**.
3. Confirm **Restart WirePlumber**. Audio may briefly stop while devices reconnect.

**Reset name** removes Alixer's override; apply again to restore the original
system description. Names remain effective with Alixer closed. This supports
ALSA and Bluetooth outputs; custom virtual sinks must be renamed by their owner.

## Saved state and updates

Mixer state and GUI preferences are saved under `~/.local/state/alixer` (or
`$XDG_STATE_HOME/alixer`). Use `ALIXER_STATE_DIR` for an isolated state directory.
Restart the service after updating the binaries.

## Development and verification

### Workspace and runtime tests

```sh
cargo test --workspace
cargo build --workspace
python3 tests/runtime.py --offline
python3 tests/runtime.py
python3 tests/capture.py
```

Runtime tests start private PipeWire, WirePlumber, and D-Bus instances with
temporary config, runtime, and state directories. They do not change desktop audio
configuration. Live tests require their executables, `pipewire-pulse`, `pactl`,
`wpctl`, `pw-cli`, `pw-dump`, `pw-play`, `pw-record`, and a C compiler (`cc`)
for the test-only LV2 fixture. The live test checks uninterrupted playback and
plugin lifetime while unrelated streams open and close, suspension before and
after playback, idle CPU use, and microphone preview cleanup after UI disconnects.

### Native LV2 tests

The native LV2 test requires `ALIXER_LILV_LIBRARY` to point to an
installed Lilv shared library. It builds and hosts a test plugin with controls,
a worker, and saved state. Without that variable, this optional test is skipped.

To test against real LSP binaries, set `ALIXER_LSP_LV2_PATH` to the directory
containing their `.lv2` bundle and `ALIXER_LILV_LIBRARY` to Lilv, then run:

```sh
cargo test -p alixer-lv2 --test lsp -- --nocapture
```
