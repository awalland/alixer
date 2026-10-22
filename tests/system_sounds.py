#!/usr/bin/env python3
"""System-event grouping and real captured audio in an isolated PipeWire session."""
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


def wait(predicate, label, seconds=12):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError("Timed out: " + label)


with tempfile.TemporaryDirectory(prefix="alixer-events-") as tmp:
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

    def request(value, revision=None):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(5)
            connection.connect(str(root / "run/alixer/control.sock"))
            connection.sendall((json.dumps(dict(version=7, id=1, revision=revision, command=value)) + "\n").encode())
            reply = json.loads(connection.makefile().readline())["result"]
            assert "Ok" in reply, reply
            return reply["Ok"]["value"] if "value" in reply["Ok"] else None

    def snapshot():
        try:
            return request(dict(type="Snapshot"))
        except (OSError, KeyError):
            return None

    def edit(kind, **fields):
        for _ in range(20):
            try:
                return request(dict(type="Edit", value=dict(type=kind, **fields)), snapshot()["scene"]["revision"])
            except AssertionError as error:
                if "Stale revision" not in str(error):
                    raise
                time.sleep(.05)
        raise AssertionError("Repeated stale revision")

    def tone(name, amplitude, seconds):
        path = root / name
        with wave.open(str(path), "wb") as wav:
            wav.setparams((2, 2, 48000, 0, "NONE", "not compressed"))
            samples = array.array("h", [int(amplitude * math.sin(n * 2 * math.pi * 440 / 48000))
                                        for n in range(int(48000 * min(1, seconds))) for _ in range(2)])
            for _ in range(max(1, int(seconds))):
                wav.writeframesraw(samples.tobytes())
        return path

    def player(name, target, path, properties):
        return spawn(["pw-play", "--target", target, "--properties",
                      f"{{ node.name = {name} {properties} }}", str(path)])

    def capture(target, count=24000):
        path = root / f"{uuid.uuid4()}.f32"
        recorder = spawn(["pw-record", "--target", target, "--properties",
                          "{ stream.capture.sink = true }", "--rate", "48000", "--channels", "2",
                          "--format", "f32", "--raw", "--sample-count", str(count), str(path)])
        return recorder, path

    def captured_peak(recording):
        recorder, path = recording
        assert recorder.wait(timeout=10) in (0, 1)
        samples = array.array("f")
        samples.frombytes(path.read_bytes())
        assert len(samples) >= 48000, "capture must contain samples"
        return max(abs(v) for v in samples)

    def short_sound_meter(play):
        # Read the same subscription frames as the GUI, including after the
        # sound has ended. Its peak must survive the slower telemetry timer.
        with socket.socket(socket.AF_UNIX) as ui:
            ui.settimeout(5)
            ui.connect(str(root / "run/alixer/control.sock"))
            ui.sendall((json.dumps(dict(version=7, id=1, revision=None,
                                       command=dict(type="Subscribe"))) + "\n").encode())
            with ui.makefile() as frames:
                def level():
                    state = json.loads(frames.readline())["result"]["Ok"]["value"]
                    return next((peak for id, peak in state["peaks"] if id == group_id), 0)
                wait(lambda: level() < .14, "previous event meter settles")
                sound = play()
                peak, deadline = 0, time.monotonic() + 1
                while time.monotonic() < deadline:
                    peak = max(peak, level())
                assert sound.wait(timeout=5) == 0
                assert peak > .16, f"GUI missed a 50 ms system sound: peak={peak}"

    def destinations(name):
        graph = json.loads(command(["pw-dump"]))
        names = {o["id"]: o.get("info", {}).get("props", {}).get("node.name") for o in graph}
        source = next(key for key, value in names.items() if value == name)
        edges = [(o["info"]["props"].get("link.output.node"), o["info"]["props"].get("link.input.node"))
                 for o in graph if o["type"] == "PipeWire:Interface:Link"]
        pending, visited, outputs = [source], set(), set()
        while pending:
            current = pending.pop()
            if current in visited:
                continue
            visited.add(current)
            for before, after in edges:
                if before == current:
                    if names.get(after) in ("test.left", "test.right"):
                        outputs.add(names[after])
                    elif names.get(after, "").startswith(("alixer.system.", "alixer.event.")):
                        pending.append(after)
        return outputs

    try:
        conf = root / "config/wireplumber/wireplumber.conf.d"
        conf.mkdir(parents=True)
        (conf / "99-test.conf").write_text('wireplumber.profiles = { main = { monitor.alsa = disabled monitor.bluez = disabled monitor.v4l2 = disabled monitor.libcamera = disabled } }')
        spawn(["pipewire"])
        wait(lambda: (root / "run/pipewire-0").exists(), "PipeWire")
        command([str(BIN / "alixerctl"), "enable"])
        wp = spawn(["wireplumber"])
        spawn(["pipewire-pulse"])
        time.sleep(.8)
        for name in ("test.left", "test.right"):
            command(["pw-cli", "create-node", "adapter", '{ factory.name = support.null-audio-sink node.name = "'+name+'" media.class = Audio/Sink audio.position = [ FL FR ] object.linger = true }'])
        daemon = spawn([str(BIN / "alixerd")])
        wait(snapshot, "service")
        group = wait(lambda: next((s for s in snapshot()["scene"]["strips"] if s["kind"] == "SystemSounds"), None), "system sounds controller")
        group_id = group["id"]
        assert group["color"] == [100, 149, 237]
        loud = tone("loud.wav", 8000, 180)
        quiet = tone("quiet.wav", 4000, 180)
        short = tone("short.wav", 12000, .05)
        silence = tone("silence.wav", 0, 180)
        music = player("test.music", "test.right", silence, "media.role = Music")
        a = player("test.event.a", "test.left", loud, 'media.role = event media.name = Feedback')
        b = player("test.event.b", "test.right", quiet, 'media.role = Notification media.name = Alert')
        wait(lambda: len(snapshot()["observation"]["streams"]) == 3, "simultaneous sounds and music")
        wait(lambda: any(id == group_id and peak > .15 for id, peak in snapshot()["peaks"]), "combined event meter")
        state = snapshot()
        assert len([s for s in state["scene"]["strips"] if s["kind"] == "Stream"]) == 1
        assert len(state["active_streams"]) == 2, "multiple events share one active controller"
        assert not any(r["source"] == group_id for r in state["scene"]["routes"])
        assert destinations("test.event.a") == {"test.left"}
        assert destinations("test.event.b") == {"test.right"}
        left, right = captured_peak(capture("test.left")), captured_peak(capture("test.right"))
        assert .22 < left < .26 and .10 < right < .14, (left, right)

        edit("Gain", strip=group_id, value=.5)
        time.sleep(.3)
        left, right = captured_peak(capture("test.left")), captured_peak(capture("test.right"))
        assert .10 < left < .14 and .05 < right < .075, (left, right)
        wait(lambda: any(id == group_id and .10 < peak < .14 for id, peak in snapshot()["peaks"]), "shared post-volume meter")
        assert destinations("test.event.a") == {"test.left"} and destinations("test.event.b") == {"test.right"}
        edit("Mute", strip=group_id, value=True)
        time.sleep(.3)
        assert captured_peak(capture("test.left")) < .00001
        assert captured_peak(capture("test.right")) < .00001
        wait(lambda: any(id == group_id and peak < .00001 for id, peak in snapshot()["peaks"]), "shared muted meter")

        # Admission must mute even a sound shorter than a daemon timer tick.
        recording = capture("test.right", 96000)
        time.sleep(.2)
        c = player("test.event.short", "test.right", short, 'event.id = custom-alert media.name = Tiny')
        c.wait(timeout=5)
        peak = captured_peak(recording)
        assert peak < .00001, f"short event leaks before daemon polling: peak={peak}"
        recording = capture("test.right", 96000)
        time.sleep(.2)
        pulse = spawn(["paplay", "--device=test.right", "--property=media.role=event",
                       "--property=event.id=audio-volume-change", str(short)])
        assert pulse.wait(timeout=5) == 0
        assert captured_peak(recording) < .00001, "PulseAudio volume feedback leaks before daemon polling"
        edit("Mute", strip=group_id, value=False)
        time.sleep(.3)
        recording = capture("test.right", 96000)
        time.sleep(.2)
        c = player("test.event.short", "test.right", short, 'event.id = custom-alert media.name = Tiny')
        c.wait(timeout=5)
        peak = captured_peak(recording)
        assert peak > .10, f"unmuted short event must be audible on its requested device: {peak}"
        recording = capture("test.right", 96000)
        time.sleep(.2)
        pulse = spawn(["paplay", "--device=test.right", "--property=media.role=event",
                       "--property=event.id=audio-volume-change", str(short)])
        assert pulse.wait(timeout=5) == 0
        peak = captured_peak(recording)
        assert peak > .10, f"PulseAudio feedback must keep its requested device and shared gain: {peak}"
        for _ in range(3):
            short_sound_meter(lambda: player("test.event.short", "test.right", short,
                                             'event.id = custom-alert media.name = Tiny'))
            short_sound_meter(lambda: spawn(["paplay", "--device=test.right", "--property=media.role=event",
                                             "--property=event.id=audio-volume-change", str(short)]))
        assert len([s for s in snapshot()["scene"]["strips"] if s["kind"] == "Stream"]) == 1

        daemon.terminate(); assert daemon.wait(timeout=5) == 0
        daemon = spawn([str(BIN / "alixerd")])
        wait(snapshot, "restarted service")
        assert next(s for s in snapshot()["scene"]["strips"] if s["kind"] == "SystemSounds")["id"] == group_id
        wait(lambda: destinations("test.event.a") == {"test.left"} and destinations("test.event.b") == {"test.right"}, "event paths restored after restart")
        wait(lambda: .10 < captured_peak(capture("test.left")) < .14, "saved event gain restored after restart")
        assert destinations("test.event.a") == {"test.left"} and destinations("test.event.b") == {"test.right"}
        for process in (a, b):
            process.terminate(); process.wait(timeout=5)
        wait(lambda: len(snapshot()["observation"]["streams"]) == 1, "events closed")
        wait(lambda: any(id == group_id and peak == 0 for id, peak in snapshot()["peaks"]), "event meter returns to silence")
        state = snapshot()
        assert len([s for s in state["scene"]["strips"] if s["kind"] == "SystemSounds"]) == 1
        assert next(s for s in state["scene"]["strips"] if s["id"] == group_id)["gain"] == .5
        graph = json.loads(command(["pw-dump"]))
        assert len([o for o in graph if o.get("info", {}).get("props", {}).get("node.name", "").startswith("alixer.input.")]) == 1, "events must not leave saved playback sinks"
        wait(lambda: not any(o.get("info", {}).get("props", {}).get("node.name", "").startswith("alixer.event.")
                             for o in json.loads(command(["pw-dump"]))), "event endpoints collected")
        music.terminate(); music.wait(timeout=5)
        music = player("test.stop.music", "test.right", loud, "media.role = Music")
        wait(lambda: len(snapshot()["active_streams"]) == 1, "managed music before shutdown")
        time.sleep(.4)
        music_id = snapshot()["active_streams"][0]
        edit("Gain", strip=music_id, value=.25)
        time.sleep(.2)
        assert .05 < captured_peak(capture("test.right")) < .075
        for crash in (False, True):
            if crash:
                daemon.kill()
            else:
                daemon.terminate()
            assert daemon.wait(timeout=5) == (-9 if crash else 0)
            wait(lambda: destinations("test.stop.music") == {"test.right"}, "desktop playback after stop/crash")
            wait(lambda: not any(o.get("info", {}).get("props", {}).get("node.name", "").startswith(("alixer.input.", "lixer.input.", "alixer.event."))
                                 for o in json.loads(command(["pw-dump"]))), "playback endpoints removed")
            assert .22 < captured_peak(capture("test.right")) < .26, "music must continue without Alixer"
            if not crash:
                daemon = spawn([str(BIN / "alixerd")])
                wait(snapshot, "service before crash")
                time.sleep(.5)
                assert .05 < captured_peak(capture("test.right")) < .075, "restart retains saved mixer gain"
        music.terminate(); music.wait(timeout=5)
        recording = capture("test.right", 96000)
        time.sleep(.2)
        pulse = spawn(["paplay", "--device=test.right", "--property=media.role=event", str(short)])
        assert pulse.wait(timeout=5) == 0
        assert captured_peak(recording) > .15, "system sounds must play normally with the daemon stopped"
        # Exercise undeploy without a desktop systemd instance. The real daemon
        # still has to be stopped through its authenticated control socket.
        tools = root / "tools"
        tools.mkdir()
        systemctl = tools / "systemctl"
        systemctl.write_text('#!/bin/sh\nif [ "$2" = show ]; then echo not-found; fi\nexit 0\n')
        systemctl.chmod(0o755)
        env["PATH"] = str(tools) + ":" + env["PATH"]
        daemon = spawn([str(BIN / "alixerd")])
        wait(snapshot, "service before undeploy")
        music = player("test.undeploy.music", "test.right", loud, "media.role = Music")
        wait(lambda: len(snapshot()["active_streams"]) == 1, "music before undeploy")
        time.sleep(.3)
        command([str(BIN / "alixerctl"), "undeploy"])
        assert daemon.wait(timeout=5) == 0
        wait(lambda: destinations("test.undeploy.music") == {"test.right"}, "undeploy returns music to desktop")
        assert .22 < captured_peak(capture("test.right")) < .26
        assert not (conf / "90-alixer.conf").exists()
        assert not (conf / "99-alixer-output-names.conf").exists()
        assert not (root / "config/systemd/user/alixer.service").exists()
        assert (root / "state/alixer").exists(), "undeploy retains mixer settings"
        # Model the real restart requested by undeploy, unloading the guard.
        wp.terminate(); wp.wait(timeout=5)
        wp = spawn(["wireplumber"])
        time.sleep(.8)
        wait(lambda: destinations("test.undeploy.music") == {"test.right"}, "desktop policy without integration")
        for name in ("lixer.input.orphan", "alixer.input.orphan", "alixer.microphone"):
            command(["pw-cli", "create-node", "adapter", '{ factory.name = support.null-audio-sink node.name = "'+name+'" media.class = Audio/Sink audio.position = [ FL FR ] object.linger = true }'])
        command(["pw-metadata", "-n", "default", "0", "alixer.saved-targets", "[]", "Spa:String:JSON"])
        command(["pw-metadata", "-n", "default", "0", "test.keep", "untouched", "Spa:String"])
        graph = json.loads(command(["pw-dump"]))
        orphan = next(o for o in graph if o.get("info", {}).get("props", {}).get("node.name") == "lixer.input.orphan")
        source = next(o["id"] for o in graph if o.get("info", {}).get("props", {}).get("node.name") == "test.undeploy.music")
        command(["pw-metadata", "-n", "default", str(source), "target.object", str(orphan["info"]["props"]["object.serial"]), "Spa:Id"])
        command(["pw-metadata", "-n", "default", "0", "default.configured.audio.sink", '{"name":"lixer.input.orphan"}', "Spa:String:JSON"])
        command([str(BIN / "alixerctl"), "undeploy"])
        wait(lambda: destinations("test.undeploy.music") == {"test.right"}, "legacy target references removed")
        graph = json.loads(command(["pw-dump"]))
        assert not any(o.get("info", {}).get("props", {}).get("node.name", "").startswith(("alixer.", "lixer.")) for o in graph)
        metadata = next(o for o in graph if o["type"] == "PipeWire:Interface:Metadata" and o.get("props", {}).get("metadata.name") == "default")["metadata"]
        assert not any(v["key"].startswith(("alixer.", "lixer.")) for v in metadata)
        assert any(v["key"] == "test.keep" and v["value"] == "untouched" for v in metadata)
        print("PASS: grouped event/Notification/event.id streams, independent destinations, shared gain/mute, short-sound admission, meters, persistence, stop/crash handoff and undeploy cleanup")
    except Exception:
        for index, log in enumerate(logs):
            log.flush(); log.seek(0)
            print(f"\n--- child {index} ---\n{log.read()[-8000:]}", file=sys.stderr)
        raise
    finally:
        for process in reversed(children):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill(); process.wait()
        for log in logs:
            log.close()
