#!/usr/bin/env python3
"""Integration tests use private audio, D-Bus, config, state, and runtime directories."""
import array, json, math, os, pathlib, shutil, socket, subprocess, sys, tempfile, time, wave

ROOT=pathlib.Path(__file__).resolve().parents[1]
BIN=pathlib.Path(os.environ.get("ALIXER_TEST_BIN_DIR",ROOT/"target/debug")).resolve()
if "--session" not in sys.argv and "--offline" not in sys.argv:
    raise SystemExit(subprocess.call(["dbus-run-session","--",sys.executable,__file__,"--session"]))

def wait(predicate, label, seconds=12):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        value=predicate()
        if value:return value
        time.sleep(.1)
    raise AssertionError("Timed out: "+label)

with tempfile.TemporaryDirectory(prefix="alixer-test-") as tmp:
    root=pathlib.Path(tmp)
    env=os.environ.copy()
    env.update(XDG_RUNTIME_DIR=str(root/"run"),XDG_CONFIG_HOME=str(root/"config"),XDG_DATA_HOME=str(root/"data"),GIO_USE_VFS="local",XDG_STATE_HOME=str(root/"state"),ALIXER_STATE_DIR=str(root/"state/alixer"))
    if "ALIXER_TEST_BIN_DIR" in os.environ:
        env["XDG_DATA_DIRS"]=str(BIN.parent/"share")+":"+env.get("XDG_DATA_DIRS","/usr/local/share:/usr/share")
    for name in ("run","config","state"): (root/name).mkdir(mode=0o700)
    children=[]; logs=[]
    def spawn(args):
        log=open(root/(str(len(logs))+".log"),"w+");logs.append(log)
        p=subprocess.Popen(args,env=env,cwd=ROOT,stdout=log,stderr=subprocess.STDOUT);children.append(p);return p
    def request(command,revision=None):
        with socket.socket(socket.AF_UNIX) as s:
            s.settimeout(5);s.connect(str(root/"run/alixer/control.sock"))
            s.sendall((json.dumps(dict(version=7,id=1,revision=revision,command=command))+"\n").encode())
            return json.loads(s.makefile().readline())
    def snapshot():
        try:return request({"type":"Snapshot"})["result"]["Ok"]["value"]
        except (OSError,KeyError,ValueError):return None
    def edit(value):
        for _ in range(20):
            reply=request({"type":"Edit","value":value},snapshot()["scene"]["revision"])
            if reply["result"].get("Err") == "Stale revision; refresh before editing":
                time.sleep(.05)
                continue
            assert "Ok" in reply["result"],reply
            return reply
        raise AssertionError("Repeated concurrent scene changes during edit")
    def command(args):
        return subprocess.check_output(args,env=env,text=True,stderr=subprocess.PIPE)
    try:
        if "--offline" in sys.argv:
            daemon=spawn([str(BIN/"alixerd"),"--offline"])
            wait(snapshot,"offline service")
            assert (root/"run/alixer/control.sock").stat().st_mode & 0o777 == 0o600
            edit(dict(type="AddBus",name="Music",aux=False,channels=["FL","FR"]))
            state=snapshot();strip=state["scene"]["strips"][0]["id"]
            edit(dict(type="Gain",strip=strip,value=.4))
            edit(dict(type="AddAuxSend",source=strip,aux=None))
            aux=next(s["id"] for s in snapshot()["scene"]["strips"] if s["kind"]=="Aux")
            edit(dict(type="SetAuxSend",source=strip,aux=aux,gain=.7,tap="Pre"))
            stale=request({"type":"Edit","value":dict(type="Mute",strip=strip,value=True)},0)
            assert "Err" in stale["result"]
            order=[s["id"] for s in reversed(snapshot()["scene"]["strips"])]
            edit(dict(type="Reorder",strips=order))
            daemon.terminate();assert daemon.wait(timeout=5)==0
            daemon=spawn([str(BIN/"alixerd"),"--offline"])
            state=wait(snapshot,"restarted service")
            assert next(s for s in state["scene"]["strips"] if s["id"]==strip)["gain"]==.4,state
            assert [s["id"] for s in state["scene"]["strips"]]==order,"strip order survives service restart"
            assert all(r["gain"]==.7 and r["tap"]=="Pre" for r in state["scene"]["routes"])
            edit(dict(type="RemoveAuxSend",source=strip,aux=aux))
            daemon.terminate();assert daemon.wait(timeout=5)==0
            daemon=spawn([str(BIN/"alixerd"),"--offline"])
            state=wait(snapshot,"service after final-send deletion")
            assert any(s["id"]==aux for s in state["scene"]["strips"]) and not state["scene"]["routes"]
            daemon.terminate();assert daemon.wait(timeout=5)==0
            saved_scene=state["scene"]
            current=pathlib.Path(env.pop("ALIXER_STATE_DIR"))
            legacy=root/"state/lixer"
            current.rename(legacy)
            (legacy/"ui.json").write_text(json.dumps(dict(zoom=1.5,hidden_strips=[aux])))
            daemon=spawn([str(BIN/"alixerd"),"--offline"])
            state=wait(snapshot,"renamed service migrates old state")
            assert state["scene"]==saved_scene
            assert json.loads((current/"ui.json").read_text())["hidden_strips"]==[aux]
            assert legacy.exists(),"retain old plugin assets during migration"
            print("PASS: private IPC, durable intent, aux persistence, and Lixer-to-Alixer state/UI migration")
        else:
            bundle=root/"plugins/continuity.lv2";bundle.mkdir(parents=True)
            subprocess.check_call(["cc","-shared","-fPIC","-O2",str(ROOT/"tests/continuity.c"),"-o",str(bundle/"continuity.so")])
            (bundle/"manifest.ttl").write_text('@prefix lv2: <http://lv2plug.in/ns/lv2core#> .\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n<urn:alixer:test:continuity> a lv2:Plugin ; lv2:binary <continuity.so> ; rdfs:seeAlso <continuity.ttl> .')
            ports=[]
            for i in range(4):
                direction="InputPort" if i<2 else "OutputPort"
                ports.append(f'[ a lv2:{direction}, lv2:AudioPort ; lv2:index {i} ; lv2:symbol "port{i}" ; lv2:name "Port {i}" ]')
            (bundle/"continuity.ttl").write_text('@prefix lv2: <http://lv2plug.in/ns/lv2core#> .\n@prefix doap: <http://usefulinc.com/ns/doap#> .\n<urn:alixer:test:continuity> a lv2:Plugin ; doap:name "Continuity fixture" ; lv2:port '+', '.join(ports)+' .')
            events=root/"plugin-events"
            env.update(LV2_PATH=str(root/"plugins"),ALIXER_TEST_PLUGIN_EVENTS=str(events))
            conf=root/"config/wireplumber/wireplumber.conf.d";conf.mkdir(parents=True)
            (conf/"99-test.conf").write_text('wireplumber.profiles = { main = { monitor.alsa = disabled monitor.bluez = disabled monitor.v4l2 = disabled monitor.libcamera = disabled } }')
            spawn(["pipewire"])
            wait(lambda:(root/"run/pipewire-0").exists(),"private PipeWire")
            legacy_integration=[root/"data/wireplumber/scripts/lixer-guard.lua",conf/"90-lixer.conf",root/"config/systemd/user/lixer.service"]
            for path in legacy_integration:
                path.parent.mkdir(parents=True,exist_ok=True);path.write_text("old integration")
            command([str(BIN/"alixerctl"),"enable"])
            if "ALIXER_TEST_BIN_DIR" in os.environ:
                assert not (root/"config/systemd/user/alixer.service").exists(),"use package-owned service"
                assert not (root/"data/wireplumber/scripts/alixer-guard.lua").exists(),"use package-owned Lua script"
            assert not any(path.exists() for path in legacy_integration),"new installation replaces legacy integration"
            wp=spawn(["wireplumber"])
            time.sleep(1)
            for name in ("test.speakers","test.headphones"):
                layout="FL FR" if name=="test.speakers" else "FL FR FC LFE RL RR SL SR"
                command(["pw-cli","create-node","adapter",'{ factory.name = support.null-audio-sink node.name = "'+name+'" node.description = "'+name+'" media.class = Audio/Sink audio.position = [ '+layout+' ] object.linger = true }'])
            # Simulate unused lingering endpoints from both current and old builds.
            for name in ("alixer.input.orphan", "lixer.input.orphan"):
                command(["pw-cli","create-node","adapter",'{ factory.name = support.null-audio-sink node.name = "'+name+'" media.class = Audio/Sink audio.position = [ FL FR ] object.linger = true }'])
            def playback_sinks():
                return [o for o in json.loads(command(["pw-dump"]))
                        if o.get("info",{}).get("props",{}).get("node.name", "").startswith(("alixer.input.", "lixer.input."))]
            daemon=spawn([str(BIN/"alixerd")])
            wait(snapshot,"audio service")
            wait(lambda:not playback_sinks(),"startup collects orphaned current and legacy sinks")
            wait(lambda:len(snapshot()["observation"]["outputs"])>=2,"virtual outputs")
            def masters():return [s for s in snapshot()["scene"]["strips"] if s["kind"]=="Output"]
            wait(lambda:len(masters())==2,"one master per endpoint")
            assert len(next(s for s in masters() if s["output_key"]=="test.headphones")["channels"])==8
            speaker=next(s for s in masters() if s["output_key"]=="test.speakers")
            speaker_id=speaker["id"]
            def master():return next(s for s in masters() if s["id"]==speaker_id)
            def output_node():
                return next(o["id"] for o in json.loads(command(["pw-dump"])) if o.get("info",{}).get("props",{}).get("node.name")=="test.speakers")
            node=str(output_node())
            command(["wpctl","set-volume",node,"0.5"])
            wait(lambda:abs(master()["gain"]-.125)<.005,"external device volume adopted")
            edit(dict(type="Gain",strip=speaker_id,value=.216))
            wait(lambda:abs(float(command(["wpctl","get-volume",node]).split()[1])-.6)<.015,"GUI gain reaches device")
            edit(dict(type="Mute",strip=speaker_id,value=True))
            wait(lambda:"MUTED" in command(["wpctl","get-volume",node]),"GUI mute reaches device")
            command(["wpctl","set-mute",node,"0"])
            wait(lambda:not master()["mute"],"external device unmute adopted")
            edit(dict(type="Rename",strip=speaker_id,name="Desk speakers"))
            names_conf=conf/"99-alixer-output-names.conf"
            assert json.loads(names_conf.read_text())["monitor.alsa.rules"][0]["actions"]["update-props"]["node.description"]=="Desk speakers"
            command(["pw-cli","set-param",node,"Props","{ channelVolumes = [ 0.125 0.0625 ] }"])
            wait(lambda:master()["output_balance"]==[1.0,.5],"external channel balance adopted")
            edit(dict(type="Gain",strip=speaker_id,value=0))
            wait(lambda:next(o["volume"] for o in snapshot()["observation"]["outputs"] if o["key"]=="test.speakers")==0,"master reaches zero")
            edit(dict(type="Gain",strip=speaker_id,value=.216))
            wait(lambda:(volumes:=next(o["channel_volumes"] for o in snapshot()["observation"]["outputs"] if o["key"]=="test.speakers")) and abs(volumes[0]-.216)<.005 and abs(volumes[1]-.108)<.005,"channel balance survives zero master gain")
            def engine_info():
                return [o["info"] for o in json.loads(command(["pw-dump"]))
                        if o.get("info",{}).get("props",{}).get("node.name", "").startswith("alixer.engine.")]
            def engine_idle():
                engines = engine_info()
                return engines and all(e["state"] in ("idle", "suspended") for e in engines)
            wait(engine_idle, "unused mixer suspends with output links attached")
            # Successive edits must not wait forever for an idle audio callback.
            for name in ("Idle bus A", "Idle bus B", "Idle bus C"):
                edit(dict(type="AddBus", name=name, aux=False, channels=["FL", "FR"]))
                time.sleep(.3)
            wait(engine_idle, "mixer remains idle after successive graph edits")
            def cpu_ticks():
                fields = pathlib.Path(f"/proc/{daemon.pid}/stat").read_text().split()
                return int(fields[13]) + int(fields[14])
            time.sleep(.5)
            ticks, started_idle = cpu_ticks(), time.monotonic()
            time.sleep(2)
            idle_cpu = 100 * (cpu_ticks() - ticks) / os.sysconf("SC_CLK_TCK") / (time.monotonic() - started_idle)
            assert idle_cpu < 2, f"idle daemon consumed {idle_cpu:.2f}% CPU"
            print(f"Idle daemon CPU (debug build, private PipeWire): {idle_cpu:.2f}%")
            path=root/"tone.wav"
            with wave.open(str(path),"wb") as wav:
                wav.setparams((2,2,48000,0,"NONE","not compressed"))
                block=array.array("h",[v for n in range(48000) for v in [int(8000*math.sin(n*2*math.pi*440/48000))]*2]).tobytes()
                for _ in range(90):wav.writeframesraw(block)
            player=spawn(["pw-play","--target","test.speakers",str(path)])
            state=wait(lambda:(s if any(strip["kind"]=="Stream" for strip in s["scene"]["strips"]) else None) if (s:=snapshot()) else None,"playback strip")
            strip=next(s["id"] for s in state["scene"]["strips"] if s["kind"]=="Stream")
            state=wait(lambda:(s if any(p[1]>.1 for p in s["peaks"]) else None) if (s:=snapshot()) else None,"audio rendered through mixer")
            assert strip in snapshot()["active_streams"],"publish exact active strip bindings"
            edit(dict(type="AddInsert",strip=strip,uri="urn:alixer:test:continuity"))
            wait(lambda:events.exists() and events.read_text()=="I","native LV2 fixture instantiated")
            wait(lambda:any(p[1]>.1 for p in snapshot()["peaks"]),"audio restored after initial plugin insertion")
            # Opening/closing unrelated browser streams must not interrupt music.
            engine_names=lambda:sorted(o["info"]["props"]["node.name"] for o in json.loads(command(["pw-dump"])) if o.get("info",{}).get("props",{}).get("node.name","").startswith("alixer.engine."))
            engine=engine_names()
            silent=root/"silence.wav"
            with wave.open(str(silent),"wb") as wav:
                wav.setparams((2,2,48000,0,"NONE","not compressed"))
                wav.writeframes(b"\0"*(48000*4*10))
            continuity=root/"continuity.f32"
            started=time.monotonic()
            recorder=spawn(["pw-record","--target","test.speakers","--properties","{ stream.capture.sink = true }","--rate","48000","--channels","2","--format","f32","--raw","--sample-count","384000",str(continuity)])
            time.sleep(.5)
            for n in range(5):
                browser=spawn(["pw-play","--target","test.speakers","--properties",f"{{ application.name = Browser{n} media.name = Browser{n} }}",str(silent)])
                wait(lambda:any(s["identity"]["application"]==f"Browser{n}" and s["managed"] for s in snapshot()["observation"]["streams"]),"new browser stream managed")
                assert engine_names()==engine,"stream additions must retain the engine and existing links"
                time.sleep(.2)
                browser.terminate();browser.wait(timeout=5)
            recorder.wait(timeout=12)
            assert time.monotonic()-started<9.5,"capture clock must not stall during stream changes"
            samples=array.array("f");samples.frombytes(continuity.read_bytes())
            assert len(samples)>=384000*2,"continuous capture must produce all samples"
            mono=samples[::2]
            windows=[mono[i:i+256] for i in range(24000,len(mono)-24000-256,256)]
            assert windows and min(math.sqrt(sum(v*v for v in block)/len(block)) for block in windows)>.001,"new streams must not produce silent gaps in existing music"
            assert events.read_text()=="I","unrelated streams must not recreate or destroy a running LV2 instance"
            wait(lambda:len(snapshot()["observation"]["streams"])==1,"temporary browser streams closed")
            assert snapshot()["active_streams"]==[strip],"saved offline browser strips must not be marked active"
            wait(lambda:len(playback_sinks())==1,"closed browser sinks collected without interrupting music")
            # Reproduce an upgrade with a surviving stream on its old Lixer
            # ingress. The logical strip ID stays the same across the rename.
            daemon.terminate();assert daemon.wait(timeout=5)==0
            wait(lambda:not any(v["key"]=="alixer.saved-targets" for o in json.loads(command(["pw-dump"])) for v in o.get("metadata",[])),"desktop handoff completed before legacy upgrade simulation")
            legacy_ingress="lixer.input."+strip
            command(["pw-cli","create-node","adapter",'{ factory.name = support.null-audio-sink node.name = "'+legacy_ingress+'" media.class = Audio/Sink audio.position = [ FL FR ] object.linger = true lixer.strip = "'+strip+'" }'])
            graph=json.loads(command(["pw-dump"]))
            stream_node=next(o["id"] for o in graph if o.get("info",{}).get("props",{}).get("media.class")=="Stream/Output/Audio")
            def stream_at(target):
                graph=json.loads(command(["pw-dump"]))
                target_id=next((o["id"] for o in graph if o.get("info",{}).get("props",{}).get("node.name")==target),None)
                return any(o.get("info",{}).get("props",{}).get("link.output.node")==stream_node and o.get("info",{}).get("props",{}).get("link.input.node")==target_id for o in graph)
            command(["pw-metadata",str(stream_node),"target.object",legacy_ingress,"Spa:Id"])
            wait(lambda:stream_at(legacy_ingress),"stream parked on old Lixer ingress")
            daemon=spawn([str(BIN/"alixerd")])
            wait(snapshot,"Alixer service after legacy ingress parking")
            wait(lambda:stream_at("alixer.input."+strip),"surviving stream handed over to Alixer ingress")
            wait(lambda:len(playback_sinks())==1,"legacy sink collected after handover")
            wait(lambda:any(p[1]>.1 for p in snapshot()["peaks"]),"audio restored after Lixer-to-Alixer handover")
            edit(dict(type="Mute",strip=strip,value=True))
            wait(lambda:all(p[1]<.0001 for p in snapshot()["peaks"]),"muted meter")
            edit(dict(type="Mute",strip=strip,value=False))
            wait(lambda:any(p[1]>.1 for p in snapshot()["peaks"]),"unmuted meter")
            edit(dict(type="AddAuxSend",source=strip,aux=None))
            aux=next(s["id"] for s in snapshot()["scene"]["strips"] if s["kind"]=="Aux")
            assert not any(r["source"]==aux for r in snapshot()["scene"]["routes"]),"new aux master must be unrouted"
            wait(lambda:any(id==aux for id,values in snapshot()["channel_peaks"]),"aux master compiled")
            wait(lambda:any(p[1]>.1 for p in snapshot()["peaks"]),"audio restored after aux graph insertion")
            def engines():return sorted(o["info"]["props"]["node.name"] for o in json.loads(command(["pw-dump"])) if o.get("info",{}).get("props",{}).get("node.name","").startswith("alixer.engine."))
            engine=engines()
            for channel in (strip,aux,speaker_id):
                for color in ([35,24,151],[119,24,151],[91,174,170]):
                    edit(dict(type="Color",strip=channel,color=color))
                    time.sleep(.12)
                    assert engines()==engine,"color edits must not rebuild DSP"
                    assert any(p[1]>.1 for p in snapshot()["peaks"]),"audio must continue during color edits"
            edit(dict(type="ResetChannelColors"))
            defaults={"SystemSounds":[100,149,237],"Stream":[91,174,170],"Group":[91,174,170],"Aux":[119,24,151],"Output":[35,24,151]}
            assert all(s["color"]==defaults[s["kind"]] for s in snapshot()["scene"]["strips"])
            time.sleep(.2)
            assert engines()==engine,"resetting colors must not rebuild DSP"
            edit(dict(type="Rename",strip=speaker_id,name="Desk monitors"))
            wait(lambda:master()["name"]=="Desk monitors","output renamed without changing identity")
            time.sleep(.2)
            assert engines()==engine,"renaming an output must not rebuild DSP"
            edit(dict(type="Rename",strip=speaker_id,name="Desk speakers"))
            def aux_peak():return max((p for id,p in snapshot()["peaks"] if id==aux),default=0)
            edit(dict(type="SetAuxSend",source=strip,aux=aux,gain=.8,tap=None))
            wait(lambda:aux_peak()>.15,"aux send gain applied")
            edit(dict(type="Gain",strip=strip,value=.5))
            wait(lambda:.08<aux_peak()<.11,"post-fader aux follows source fader")
            edit(dict(type="SetAuxSend",source=strip,aux=aux,gain=None,tap="Pre"))
            wait(lambda:aux_peak()>.15,"pre-fader aux ignores source fader")
            assert engines()==engine,"send level/tap changes must not recreate the audio engine"
            edit(dict(type="RemoveAuxSend",source=strip,aux=aux))
            assert any(s["id"]==aux for s in snapshot()["scene"]["strips"]),"last send deletion keeps aux master"
            edit(dict(type="AddAuxSend",source=strip,aux=aux))
            edit(dict(type="RemoveStrip",strip=aux))
            assert not any(r["destination"]==dict(kind="Bus",id=aux) for r in snapshot()["scene"]["routes"]),"manual master deletion removes associated sends"
            edit(dict(type="Gain",strip=strip,value=1))
            wait(lambda:any(id==speaker_id and len(values)==2 and max(values)>.1 for id,values in snapshot()["channel_peaks"]),"output channel meters")
            edit(dict(type="Mute",strip=speaker_id,value=True))
            wait(lambda:"MUTED" in command(["wpctl","get-volume",node]),"output muted")
            wait(lambda:any(id==speaker_id and max(values)>.1 for id,values in snapshot()["channel_peaks"]),"output meters remain pre-device-mute after topology edits settle")
            daemon.terminate();assert daemon.wait(timeout=5)==0
            command(["wpctl","set-volume",node,"0.4"])
            names_conf.unlink()
            command(["wpctl","set-mute",node,"0"])
            wait(lambda:not playback_sinks(),"stopping the service removes managed playback endpoints")
            daemon=spawn([str(BIN/"alixerd")])
            wait(snapshot,"service socket after restart")
            assert json.loads(names_conf.read_text())["monitor.alsa.rules"][0]["actions"]["update-props"]["node.description"]=="Desk speakers","service regenerates global rules from durable intent"
            wait(lambda:any(p[1]>.1 for p in snapshot()["peaks"]),"audio restored after restart")
            wait(lambda:abs(master()["gain"]-.064)<.005 and not master()["mute"],"device controls changed during downtime adopted")
            assert master()["name"]=="Desk speakers" and len(masters())==2
            command(["pw-cli","destroy",node])
            wait(lambda:not any(o["key"]=="test.speakers" for o in snapshot()["observation"]["outputs"]),"output unplugged")
            assert master()["name"]=="Desk speakers","offline output intent retained"
            command(["pw-cli","create-node","adapter",'{ factory.name = support.null-audio-sink node.name = "test.speakers" node.description = "Returned speakers" media.class = Audio/Sink audio.position = [ FL FR ] object.linger = true }'])
            wait(lambda:any(o["key"]=="test.speakers" for o in snapshot()["observation"]["outputs"]),"output returned")
            wait(lambda:abs(master()["gain"]-next(o["volume"] for o in snapshot()["observation"]["outputs"] if o["key"]=="test.speakers"))<.005,"returning output's current volume adopted")
            wait(lambda:any(id==speaker_id and max(values)>.1 for id,values in snapshot()["channel_peaks"]),"audio reaches retained output master")
            assert master()["id"]==speaker_id and master()["name"]=="Desk speakers" and len(masters())==2
            recording=root/"returned.f32"
            recorder=spawn(["pw-record","--target","test.speakers","--properties","{ stream.capture.sink = true }","--rate","48000","--channels","2","--format","f32","--raw","--sample-count","48000",str(recording)])
            recorder_status=recorder.wait(timeout=10)
            samples=array.array("f");samples.frombytes(recording.read_bytes())
            assert samples and max(abs(v) for v in samples)>.0001, f"signal must reach returned endpoint, not just its meter (recorder status {recorder_status}, samples {len(samples)}, output {next(o for o in snapshot()['observation']['outputs'] if o['key']=='test.speakers')})"
            # Exercise the real WirePlumber rule loader/matcher and PulseAudio
            # compatibility layer without requiring ALSA hardware in the test.
            scripts=root/"data/wireplumber/scripts"
            scripts.mkdir(parents=True,exist_ok=True)
            (scripts/"alixer-output-name-test.lua").write_text('''
local props = JsonUtils.match_rules_update_properties (
  Conf.get_section_as_json ("monitor.alsa.rules", Json.Array {}), {
    ["factory.name"] = "support.null-audio-sink",
    ["node.name"] = "test.speakers", ["node.description"] = "test.speakers",
    ["media.class"] = "Audio/Sink", ["audio.position"] = "[ FL FR ]",
  })
props["node.name"] = "test.rulecheck"
test_node = Node ("adapter", props)
test_node:activate (Features.ALL)
''')
            (conf/"99-output-name-test.conf").write_text('''
wireplumber.components = [ { name = alixer-output-name-test.lua type = script/lua provides = custom.alixer-output-name-test } ]
wireplumber.profiles = { main = { custom.alixer-output-name-test = required } }
''')
            wp.terminate();assert wp.wait(timeout=5)==0
            wp=spawn(["wireplumber"])
            spawn(["pipewire-pulse"])
            wait(lambda:(root/"run/pulse/native").exists(),"private PulseAudio socket")
            def pulse_name():
                sinks=json.loads(command(["pactl","--format=json","list","sinks"]))
                return next((s["description"] for s in sinks if s["name"]=="test.rulecheck"),None)
            wait(lambda:pulse_name()=="Desk speakers","WirePlumber rename visible to PulseAudio clients")
            edit(dict(type="ResetOutputName",strip=speaker_id))
            assert json.loads(names_conf.read_text())["monitor.alsa.rules"]==[]
            wp.terminate();assert wp.wait(timeout=5)==0
            wp=spawn(["wireplumber"])
            wait(lambda:pulse_name()=="test.speakers","reset removes global name override")
            if "ALIXER_TEST_BIN_DIR" in os.environ:
                # Simulate package removal: the user fragment survives, but the
                # package-owned script is no longer on WirePlumber's data path.
                wp.terminate();assert wp.wait(timeout=5)==0
                env["XDG_DATA_DIRS"]="/usr/local/share:/usr/share"
                wp=spawn(["wireplumber"])
                time.sleep(1)
                assert wp.poll() is None,"leftover optional activation must not break WirePlumber"
                command(["wpctl","status"])
            player.terminate(); player.wait(timeout=5)
            wait(engine_idle, "mixer suspends after playback ends, including saved LV2 strips")
            wait(lambda:not playback_sinks(),"last playback sink collected")
            edit(dict(type="Gain",strip=strip,value=.6))
            daemon.terminate();assert daemon.wait(timeout=5)==0
            daemon=spawn([str(BIN/"alixerd")])
            wait(snapshot,"service restarted with saved offline strips")
            time.sleep(.5)
            assert not playback_sinks(),"saved settings must not recreate offline playback sinks"
            assert next(s for s in snapshot()["scene"]["strips"] if s["id"]==strip)["gain"]==.6
            player=spawn(["pw-play","--target","test.speakers",str(path)])
            wait(lambda:strip in snapshot()["active_streams"],"returning app reuses saved strip")
            wait(lambda:len(playback_sinks())==1 and any(p[1]>.01 for p in snapshot()["peaks"]),"returning app gets a working sink")
            sink=playback_sinks()[0]
            command(["pw-cli","destroy",str(sink["id"])])
            wait(lambda:len(playback_sinks())==1 and playback_sinks()[0]["info"]["props"]["object.serial"]!=sink["info"]["props"]["object.serial"],"externally removed live sink recreated")
            wait(lambda:any(p[1]>.01 for p in snapshot()["peaks"]),"audio restored after external sink removal")
            player.terminate();player.wait(timeout=5)
            wait(lambda:not playback_sinks(),"recreated sink collected after app exits")
            print("PASS: native discovery/DSP, uninterrupted stream changes/LV2 reuse, exact active strips, legacy Lixer ingress handover, live aux controls, retained/deleted masters, device controls, downtime/hotplug reconciliation, and global output names/reset through WirePlumber and PulseAudio")
    except Exception:
        for i,log in enumerate(logs):
            log.flush();log.seek(0);print("\n--- child",i,"---\n",log.read()[-12000:],file=sys.stderr)
        raise
    finally:
        for p in reversed(children):
            if p.poll() is None:
                p.terminate()
                try:p.wait(timeout=3)
                except subprocess.TimeoutExpired:p.kill();p.wait()
        for log in logs:log.close()
