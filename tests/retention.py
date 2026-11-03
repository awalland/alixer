#!/usr/bin/env python3
"""Exercise the real one-minute cleanup timer against private PipeWire/state."""
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("ALIXER_TEST_BIN_DIR", ROOT / "target/debug")).resolve()
if "--session" not in sys.argv:
    raise SystemExit(subprocess.call(["dbus-run-session", "--", sys.executable, __file__, "--session"]))


def wait(predicate, label, seconds=12):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.1)
    raise AssertionError("Timed out: " + label)


with tempfile.TemporaryDirectory(prefix="alixer-retention-") as tmp:
    root = Path(tmp)
    env = os.environ.copy()
    env.update(XDG_RUNTIME_DIR=str(root / "run"), XDG_CONFIG_HOME=str(root / "config"),
               XDG_DATA_HOME=str(root / "data"), XDG_STATE_HOME=str(root / "state"),
               ALIXER_STATE_DIR=str(root / "state/alixer"), GIO_USE_VFS="local")
    (root / "run").mkdir(mode=0o700)
    children, logs = [], []

    def spawn(args):
        log = open(root / f"{len(logs)}.log", "w+")
        logs.append(log)
        child = subprocess.Popen(args, env=env, stdout=log, stderr=subprocess.STDOUT)
        children.append(child)
        return child

    def snapshot():
        try:
            with socket.socket(socket.AF_UNIX) as client:
                client.settimeout(3)
                client.connect(str(root / "run/alixer/control.sock"))
                client.sendall(b'{"version":7,"id":1,"command":{"type":"Snapshot"}}\n')
                return json.loads(client.makefile().readline())["result"]["Ok"]["value"]
        except (OSError, KeyError, ValueError):
            return None

    try:
        daemon = spawn([str(BIN / "alixerd"), "--offline"])
        wait(snapshot, "initial state")
        daemon.terminate()
        assert daemon.wait(timeout=5) == 0
        db = sqlite3.connect(root / "state/alixer/state.sqlite3")
        saved = json.loads(db.execute("SELECT json FROM state").fetchone()[0])
        ids = [str(uuid.uuid4()) for _ in range(3)]
        for id, name in zip(ids, ("expired", "recent", "customized")):
            saved["scene"]["strips"].append(dict(
                id=id, name=name, kind="Stream",
                identity=dict(application="test", name=name, role="", channels=["MONO"]),
                channels=["MONO"], gain=1.0, mute=False, solo=False,
                color=[91, 174, 170], inserts=[]))
            saved["scene"]["routes"].append(dict(
                id=str(uuid.uuid4()), source=id, destination=dict(kind="Output", id="test.speakers"),
                source_channel=0, target_channel=0, gain=1.0, tap="Post"))
        now = int(time.time())
        saved["retention"] = dict(absent_since={ids[0]: now - 7200, ids[1]: now, ids[2]: now - 7200},
                                  protected=[ids[2]], history_imported=True)
        db.execute("UPDATE state SET json=?", (json.dumps(saved),))
        db.commit()
        db.close()
        spawn(["pipewire"])
        wait(lambda: (root / "run/pipewire-0").exists(), "private PipeWire")
        conf = root / "config/wireplumber/wireplumber.conf.d"
        conf.mkdir(parents=True, exist_ok=True)
        (conf / "99-test.conf").write_text("wireplumber.profiles = { main = { monitor.alsa = disabled monitor.bluez = disabled monitor.v4l2 = disabled monitor.libcamera = disabled } }")
        spawn(["wireplumber"])
        daemon = spawn([str(BIN / "alixerd")])
        state = wait(lambda: (s if s["status"].startswith("Connected") else None)
                     if (s := snapshot()) else None, "connected service")
        assert ids[0] in [s["id"] for s in state["scene"]["strips"]], "cleanup must run on its schedule"
        revision = state["scene"]["revision"]
        state = wait(lambda: (s if ids[0] not in [c["id"] for c in s["scene"]["strips"]] else None)
                     if (s := snapshot()) else None, "scheduled expiration", seconds=70)
        remaining = {s["id"] for s in state["scene"]["strips"]}
        assert set(ids[1:]) <= remaining, "recent and customized channels survive"
        assert all(r["source"] != ids[0] for r in state["scene"]["routes"])
        assert state["scene"]["revision"] > revision
        daemon.terminate()
        assert daemon.wait(timeout=5) == 0
        daemon = spawn([str(BIN / "alixerd"), "--offline"])
        restored = wait(snapshot, "persisted cleanup")
        assert restored["scene"] == state["scene"]
        print("PASS: scheduled cleanup removes expired default playback and routes, preserves recent/customized channels, and survives restart")
    finally:
        for child in reversed(children):
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
        for log in logs:
            log.close()
