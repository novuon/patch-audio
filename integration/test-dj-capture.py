#!/usr/bin/env python3
"""Isolated stamped-program recorder test. No PipeWire or live output is used."""
import json, os, socket, struct, subprocess, sys, tempfile, threading, time
from pathlib import Path

binary = Path(sys.argv[1] if len(sys.argv)>1 else 'target/release/omarchy-patch').resolve()

def wait(predicate, timeout=8):
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        result=predicate()
        if result: return result
        time.sleep(.01)
    raise AssertionError('Timed out')

def call(path, text):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(1);s.connect(str(path));s.sendall((text+'\n').encode())
        return s.makefile().readline().strip()

with tempfile.TemporaryDirectory(prefix='patch-dj-capture-') as tmp:
    root=Path(tmp); runtime=root/'run'; state=root/'state'; runtime.mkdir(mode=0o700)
    env=dict(os.environ,XDG_RUNTIME_DIR=str(runtime),XDG_STATE_HOME=str(state),XDG_CONFIG_HOME=str(root/'config'),XDG_CACHE_HOME=str(root/'cache'),XDG_DATA_HOME=str(root/'data'))
    directory=runtime/'omarchy-patch';directory.mkdir(mode=0o700)
    engine=directory/'engine.sock';recorder=directory/'recorder.sock'
    listener=socket.socket(socket.AF_UNIX);listener.bind(str(engine));listener.listen()
    stop=threading.Event();discontinuous=threading.Event();counter=[1000]
    def client(s):
        try:
            reader=s.makefile();line=reader.readline();req=json.loads(line)
            if req.get('op')=='capture_program':
                sample=counter[0]
                while not stop.is_set():
                    if discontinuous.is_set():sample+=512;discontinuous.clear()
                    n=256
                    # Depend on engine sample so every kept sample can be verified.
                    data=b''.join(struct.pack('<ff', (sample+i)%1000/2000, -((sample+i)%1000/2000)) for i in range(n))
                    s.sendall(struct.pack('<QQII',0x504154434850434D,sample,48000,n)+data)
                    sample+=n;counter[0]=sample;time.sleep(n/48000)
            else:
                snap={'set':{'id':'test-session','name':'Capture test','collections':[1,2],'transitions':[{'id':'t1','from':'a.wav','to':'b.wav','technique':'blend','started_sample':1800,'handover_sample':2200}]},'decks':[]}
                while True:
                    s.sendall((json.dumps({'ok':snap})+'\n').encode())
                    if not reader.readline():break
        except (OSError,ValueError):pass
        finally:s.close()
    def accept():
        while not stop.is_set():
            try:s,_=listener.accept()
            except OSError:return
            threading.Thread(target=client,args=(s,),daemon=True).start()
    threading.Thread(target=accept,daemon=True).start()
    def status():
        try:return json.loads(call(recorder,'status'))
        except (OSError,ValueError):return None
    take=root/'take.wav'
    process=subprocess.Popen([str(binary),'record-daemon','program:mix','f32','48000',str(take),'Test'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
    try:
        wait(status);wait(lambda:status() and status()['frames']>=2048)
        assert call(recorder,'pause')=='ok';time.sleep(.08)
        assert call(recorder,'resume')=='ok';time.sleep(.08)
        assert call(recorder,'stop')=='ok';process.wait(timeout=8)
        assert process.returncode==0,process.stderr.read().decode()
        meta=json.loads(take.with_suffix('.dj.json').read_text())
        assert meta['complete'] and meta['session_id']=='test-session'
        assert any(s['paused'] for s in meta['segments'])
        pcm=take.read_bytes();data_at=pcm.index(b'data')+8
        actual=list(struct.iter_unpack('<ff',pcm[data_at:]))
        expected=[]
        for seg in meta['segments']:
            if not seg['paused']:
                expected.extend(((seg['engine_start']+i)%1000/2000,-((seg['engine_start']+i)%1000/2000)) for i in range(seg['frames']))
        assert len(actual)==len(expected)==meta['frames']
        assert all(struct.pack('<ff',*a)==struct.pack('<ff',*b) for a,b in zip(actual,expected))
        for transition in meta['transitions']:
            sample=transition['handover_sample']
            matching=next((s for s in meta['segments'] if s['engine_start']<=sample<s['engine_start']+s['frames'] and not s['paused']),None)
            assert transition['recording_handover']==(matching['recording_start']+sample-matching['engine_start'] if matching else None)
        # A graph discontinuity must retain a partial file and explicit failure.
        bad=root/'discontinuous.wav'
        process=subprocess.Popen([str(binary),'record-daemon','program:mix','f32','48000',str(bad),'Bad clock'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        wait(status);wait(lambda:status() and status()['frames']>=512);discontinuous.set()
        process.wait(timeout=8);assert process.returncode!=0
        outcome=json.loads(recorder.with_suffix('.result.json').read_text())
        assert 'discontinuity' in outcome['error'].lower()
        assert not json.loads(bad.with_suffix('.dj.json').read_text())['complete']
        print('PASS: exact float samples, pause mappings, chapters, finalization, clock-discontinuity recovery')
    finally:
        if process.poll() is None:process.terminate();process.wait(timeout=5)
        stop.set();listener.close()
