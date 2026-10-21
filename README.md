# Alixer - A Linux Mixer

A Rust audio mixer for Linux using PipeWire and WirePlumber, with an egui GUI.

The application is `alixer`, its audio service is `alixerd` (`alixer.service`),
and the command-line client is `alixerctl`.

Upgrading from Lixer: close the old GUI and stop/disable `lixer.service` before
starting Alixer. On first start, existing `~/.local/state/lixer` settings are
copied to `~/.local/state/alixer` (or their XDG equivalents), including the mixer
database, plugins, zoom and collapsed strips. The old directory is retained for
LV2 assets that use absolute paths. Existing Alixer state is never overwritten.
`LIXER_STATE_DIR` and `LIXER_LILV_LIBRARY` remain accepted as legacy fallbacks;
new setups should use `ALIXER_STATE_DIR` and `ALIXER_LILV_LIBRARY`.

For installed-service users, run `alixerctl enable` to replace the old integration
files, then `systemctl --user daemon-reload`, restart WirePlumber, and run
`systemctl --user enable --now alixer.service`. Development sessions use
`target/debug/alixer --dev-service`.

Build and run a development session:

```sh
cargo build --workspace
target/debug/alixer --dev-service
```

`--dev-service` starts the sibling `alixerd` binary and stops it when the GUI exits.
It refuses to take ownership of an already running service. Without this flag,
the GUI connects to the independently running service.

## UI scale

Use egui's zoom shortcuts (`Ctrl` + `+`, `Ctrl` + `-`, `Ctrl` + `0`). The GUI
remembers its zoom independently of desktop display scaling and service state.
Preferences are saved in `$XDG_STATE_HOME/alixer/ui.json` (normally
`~/.local/state/alixer/ui.json`). `ALIXER_STATE_DIR` overrides that directory for
development and isolated tests. Invalid preferences fall back to 100% zoom.

Double-click a channel fader handle to reset it to 0 dB. Hold Shift while dragging
the handle for half-speed adjustment; Shift can be changed during the drag.
Console strips have a common height that fills the available window space, with
names at the bottom in fixed-height scroll areas. Long names do not resize the
strips; short windows can scroll the console vertically.
Right-click a strip's name or background and choose **Hide** to collapse it to
a narrow strip in the same position. Its vertical name identifies it; the **»**
button at the bottom restores it. Collapsed strips keep playing with their
existing settings, and the layout is remembered across GUI restarts.
The same menu offers **Rename…**; renaming an aux master updates all send labels
that reference it.
The **Changes** page offers **Reset channel colors** to restore every strip's
default color, including offline or collapsed strips: teal for playback/groups,
blue (`#231897`) for outputs, and purple (`#771897`) for aux masters.

Each strip has an insert rack above its buttons and fader. Right-click the empty
rack space and choose **Add plugin…** to search LV2 effects matching the strip's
channel count. Click an insert to edit its generic controls in the inspector.
Right-click an insert to bypass it, move it up or down, or remove it. Long insert
chains scroll inside the rack without changing strip height.
The host supports audio/control ports and leaves unsupported optional ports
explicitly disconnected, as LV2 requires. This includes the optional messaging
ports on LSP audio effects; their numeric controls remain available in the
inspector. Plugins requiring unsupported port types are excluded from the picker.
For an optional test against real LSP binaries, set `ALIXER_LSP_LV2_PATH` to the
directory containing their `.lv2` bundle and `ALIXER_LILV_LIBRARY` to Lilv, then run
`cargo test -p alixer-lv2 --test lsp -- --nocapture`.

The routing box above the insert rack shows two destination pairs per row
(`□ □ | □ □`). Hover a checkbox to identify its destination and channel; click it
to connect or disconnect that channel. Destinations follow the visible output
masters, filling down the left column before continuing down the right column.
**Show offline** includes disconnected outputs in both the bank and the selector.
Groups follow the outputs. Additional destination channels form further
pairs, and long lists scroll.
Mono sources can feed both sides of a stereo destination. Output masters remain
terminal; edit their incoming connections in the sending channel's routing box.

Rename an output in its inspector to set its system-wide display name. Alixer
saves ALSA and Bluetooth rules in
`$XDG_CONFIG_HOME/wireplumber/wireplumber.conf.d/99-alixer-output-names.conf`
(normally under `~/.config`). The output's internal identity and routing stay
unchanged. Choose **Apply output names…** and confirm **Restart WirePlumber**
to apply pending names in other audio applications, including pavucontrol;
audio may briefly stop while devices reconnect. **Reset name** removes Alixer's
override; apply again to restore the original system description. Rules remain
effective with Alixer closed. Custom virtual sinks created by other applications
are outside the ALSA/Bluetooth monitor rules and must be renamed by their owner.

The aux-send rack sits between routing and inserts. Right-click empty space to
create an aux master (`Aux1`, `Aux2`, …) and its send, or select an existing master.
New sends start post-fader at −∞; raise the horizontal fader to send audio. New
masters have no outgoing routes. Masters appear after playback/groups and before
outputs. Mono sources feed both sides of a stereo aux; new masters otherwise
match the source layout. Existing auxes with incompatible channel counts or
feedback paths cannot be selected.

Right-click a send to select pre/post-fader mode or **Delete send**. On a full
rack, **Add send…** offers further destinations. Send deletion retains the master,
even when no sends remain. To delete a master and its associated sends, right-click
its bottom name and choose **Delete aux master**, or select it and use the same
button in the inspector. Aux send levels/taps, routing, masters, and inserts are
saved by the service. Level changes ramp smoothly without recreating the audio
engine. Output masters remain terminal and cannot originate aux sends.

## Output masters

Each discovered output endpoint gets one master strip, with a meter for each
channel. Internal Alixer ingress sinks are excluded. Output masters receive routes
but cannot send audio onward, be deleted, or be soloed. Use groups and auxes to
build reusable mixes and feed several outputs.

The output signal path is:

```text
Incoming routes → sum → LV2 inserts → device volume/mute → endpoint
```

The fader and mute control the same native device settings used by desktop volume
controls. External changes are adopted, including changes made while Alixer was
stopped. Device gain is not applied a second time in the mixer. Meters show
processed audio before device volume and mute; inserts and meters cover audio
routed through Alixer. Device controls affect all audio reaching the endpoint.

Disconnected outputs retain their names, routing, and inserts. Enable **Show
offline** to view them. Reconnection uses the endpoint's PipeWire `node.name` and
current device controls, retaining Alixer settings. Saved channel names prevent
reconnection from silently swapping channels when registry IDs are reused.

The GUI, service, and CLI currently use protocol version 5. Restart the service
after updating the binaries. Older saved mixer scenes remain readable.

## Verification

```sh
cargo test --workspace
cargo build --workspace
python3 tests/runtime.py --offline
python3 tests/runtime.py
```

Runtime tests start private PipeWire, WirePlumber, and D-Bus instances with
temporary config, runtime, and state directories. They do not change desktop audio
configuration. The live test requires their executables, `wpctl`, `pw-cli`,
`pw-dump`, `pw-play`, and `pw-record`.

The native LV2 test additionally requires `ALIXER_LILV_LIBRARY` to point to an
installed Lilv shared library. It builds and hosts a test plugin with controls,
a worker, and saved state. Without that variable, this optional test is skipped.
