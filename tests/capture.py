#!/usr/bin/env python3
"""Microphone capture checks in a private PipeWire / WirePlumber session."""
import array
import json
import math
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import time
import uuid
import wave

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = pathlib.Path(os.environ.get("ALIXER_TEST_BIN_DIR", ROOT / "target/debug")).resolve()
if "--session" not in sys.argv:
    raise SystemExit(subprocess.call(["dbus-run-session", "--", sys.executable, __file__, "--session"]))


def wait(predicate, label, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(.1)
    raise AssertionError("Timed out: " + label)


with tempfile.TemporaryDirectory(prefix="alixer-capture-") as tmp:
    root = pathlib.Path(tmp)
    env = os.environ.copy()
    env.update(XDG_RUNTIME_DIR=str(root / "run"), XDG_CONFIG_HOME=str(root / "config"),
               XDG_DATA_HOME=str(root / "data"), XDG_STATE_HOME=str(root / "state"),
               ALIXER_STATE_DIR=str(root / "state/alixer"), GIO_USE_VFS="local")
    if "ALIXER_TEST_BIN_DIR" in os.environ:
        env["XDG_DATA_DIRS"] = str(BIN.parent / "share") + ":/usr/local/share:/usr/share"
    for name in ("run", "config", "state"):
        (root / name).mkdir(mode=0o700)
    children, logs = [], []

    def spawn(args):
        log = open(root / f"{len(logs)}.log", "w+")
        logs.append(log)
        child = subprocess.Popen(args, env=env, stdout=log, stderr=subprocess.STDOUT)
        children.append(child)
        return child

    def command(args):
        return subprocess.check_output(args, env=env, text=True, stderr=subprocess.PIPE)

    def request(command_value, revision=None):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(5)
            connection.connect(str(root / "run/alixer/control.sock"))
            connection.sendall((json.dumps(dict(version=6, id=1, revision=revision, command=command_value)) + "\n").encode())
            return json.loads(connection.makefile().readline())["result"]

    def snapshot():
        try:
            return request(dict(type="Snapshot"))["Ok"]["value"]
        except (OSError, KeyError):
            return None

    def edit(value):
        result = request(dict(type="Edit", value=value), snapshot()["scene"]["revision"])
        assert "Ok" in result, result

    def record_peak():
        path = root / f"{uuid.uuid4()}.f32"
        recorder = spawn(["pw-record", "--target", "alixer.microphone", "--rate", "48000",
                          "--channels", "2", "--format", "f32", "--raw", "--sample-count", "24000", str(path)])
        status = recorder.wait(timeout=12)
        # Some pw-record versions return 1 at the sample limit; check the data.
        assert status in (0, 1), f"recorder exited unexpectedly: {status}"
        samples = array.array("f")
        samples.frombytes(path.read_bytes())
        assert len(samples) >= 48000, f"capture must produce samples, even when silent (status {status}, samples {len(samples)})"
        return max(abs(v) for v in samples)

    try:
        conf = root / "config/wireplumber/wireplumber.conf.d"
        conf.mkdir(parents=True)
        (conf / "99-test.conf").write_text('wireplumber.profiles = { main = { monitor.alsa = disabled monitor.bluez = disabled monitor.v4l2 = disabled monitor.libcamera = disabled } }')
        spawn(["pipewire"])
        wait(lambda: (root / "run/pipewire-0").exists(), "private PipeWire")
        command([str(BIN / "alixerctl"), "enable"])
        spawn(["wireplumber"])
        spawn(["pipewire-pulse"])
        daemon = spawn([str(BIN / "alixerd")])
        wait(snapshot, "service")

        tone = root / "mic.wav"
        with wave.open(str(tone), "wb") as wav:
            wav.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
            block = array.array("h", [int(8000 * math.sin(n * 2 * math.pi * 440 / 48000)) for n in range(48000)]).tobytes()
            for _ in range(180):
                wav.writeframesraw(block)

        def microphone():
            return spawn(["pw-play", "--target", "0", "--properties",
                          "{ media.class = Audio/Source node.name = test.microphone node.description = TestMicrophone }", str(tone)])

        player = microphone()
        wait(lambda: any(s["kind"] == "Input" for s in snapshot()["scene"]["strips"]), "microphone discovery")
        scene = snapshot()["scene"]
        mic = next(s for s in scene["strips"] if s["kind"] == "Input")
        master = next(s for s in scene["strips"] if s["kind"] == "VirtualMic")
        assert mic["color"] == [224, 164, 62]
        assert not scene["routes"], "microphones must start without speaker or virtual-mic routes"
        wait(lambda: any(p[0] == mic["id"] and p[1] > .1 for p in snapshot()["peaks"]), "input audio meter")
        def engine_idle():
            engines = [o["info"] for o in json.loads(command(["pw-dump"]))
                       if o.get("info", {}).get("props", {}).get("node.name", "").startswith("alixer.engine.")]
            return engines and all(e["state"] in ("idle", "suspended") for e in engines)

        wait(engine_idle, "unrouted microphone sleeps after one-shot meter request")
        with socket.socket(socket.AF_UNIX) as ui:
            ui.settimeout(5)
            ui.connect(str(root / "run/alixer/control.sock"))
            ui.sendall((json.dumps(dict(version=6, id=1, revision=None,
                                       command=dict(type="Subscribe"))) + "\n").encode())
            with ui.makefile() as frames:
                def input_meter():
                    value = json.loads(frames.readline())["result"]["Ok"]["value"]
                    return any(id == mic["id"] and peak > .1 for id, peak in value["peaks"])
                wait(input_meter, "UI subscription enables unrouted input meters")
        wait(engine_idle, "closing UI releases microphone preview links")
        wait(lambda: any(s["name"] == "alixer.microphone" for s in json.loads(command(["pactl", "--format=json", "list", "sources"]))), "virtual microphone exposed to PulseAudio clients")
        assert record_peak() < .0001, "unrouted virtual microphone must be silent"
        for channel in range(2):
            edit(dict(type="PutRoute", route=dict(id=str(uuid.uuid4()), source=mic["id"],
                 destination=dict(kind="Output", id="alixer.microphone"), source_channel=0,
                 target_channel=channel, gain=1.0, tap="Post")))
        wait(lambda: any(p[0] == master["id"] and p[1] > .1 for p in snapshot()["peaks"]), "virtual microphone audio")
        assert record_peak() > .15, "meeting app must receive real samples"
        edit(dict(type="Gain", strip=mic["id"], value=.5))
        time.sleep(.4)
        assert .09 < record_peak() < .15, "microphone fader affects capture"
        edit(dict(type="Mute", strip=mic["id"], value=True))
        time.sleep(.4)
        assert record_peak() < .0001, "microphone mute affects capture"
        edit(dict(type="Mute", strip=mic["id"], value=False))
        edit(dict(type="Mute", strip=master["id"], value=True))
        time.sleep(.4)
        assert record_peak() < .0001, "virtual master mute affects capture"
        edit(dict(type="Mute", strip=master["id"], value=False))

        player.terminate()
        player.wait(timeout=5)
        wait(lambda: not snapshot()["observation"]["inputs"], "microphone unplug")
        assert record_peak() < .0001, "unplugged input must be silent"
        player = microphone()
        wait(lambda: bool(snapshot()["observation"]["inputs"]), "microphone replug")
        wait(lambda: any(p[0] == master["id"] and p[1] > .09 for p in snapshot()["peaks"]), "microphone routing restored")
        assert len([s for s in snapshot()["scene"]["strips"] if s["kind"] == "Input"]) == 1

        # A meeting that selected the virtual mic must not silently switch to
        # the raw hardware source when the service stops.
        meeting = spawn(["pw-record", "--target", "alixer.microphone", "--properties",
                         "{ node.name = test.meeting }", "--format", "f32", "--raw", str(root / "meeting.f32")])

        def meeting_sources():
            graph = json.loads(command(["pw-dump"]))
            names = {o["id"]: o.get("info", {}).get("props", {}).get("node.name") for o in graph}
            meeting_ids = {key for key, name in names.items() if name == "test.meeting"}
            return {names.get(o.get("info", {}).get("props", {}).get("link.output.node"))
                    for o in graph if o.get("info", {}).get("props", {}).get("link.input.node") in meeting_ids}

        wait(lambda: meeting_sources() == {"alixer.microphone"}, "meeting connected to virtual microphone")
        daemon.terminate()
        assert daemon.wait(timeout=5) == 0
        time.sleep(.6)
        assert meeting_sources() == {"alixer.microphone"}, "keep the selected source instead of falling back to the raw microphone"
        assert record_peak() < .0001, "retained virtual source must be silent without the service"
        daemon = spawn([str(BIN / "alixerd")])
        wait(snapshot, "service restarted")
        wait(lambda: any(p[0] == master["id"] and p[1] > .09 for p in snapshot()["peaks"]), "saved capture routes restored")
        wait(lambda: meeting_sources() == {"alixer.microphone"}, "meeting reconnected after service restart")
        assert .09 < record_peak() < .15
        meeting.terminate()
        meeting.wait(timeout=5)
        for route in snapshot()["scene"]["routes"]:
            edit(dict(type="RemoveRoute", route=route["id"]))
        wait(engine_idle, "removing the final microphone route releases its hardware links")
        print("PASS: input discovery, amber strips, silent defaults, PulseAudio source, captured samples, gain/mute, hotplug and persistence")
    except Exception:
        for index, log in enumerate(logs):
            log.flush()
            log.seek(0)
            print(f"\n--- child {index} ---\n{log.read()[-8000:]}", file=sys.stderr)
        raise
    finally:
        for child in reversed(children):
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
        for log in logs:
            log.close()
