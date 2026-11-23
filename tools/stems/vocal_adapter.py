#!/usr/bin/env python3
"""Patch's vocal meter: where a track sings. One process measures many
tracks (the model loads once). Reads a JSON line {"model","backend"} first,
then one {"source": path} per track; answers one JSON line each:
{"ok": {"seconds": [[vocal-to-mix dB, audible], ...]}} or {"error": "..."}."""
import json, sys, subprocess, pathlib, hashlib
import numpy as np
import torch
from demucs.pretrained import get_model
from demucs.apply import apply_model
req=json.loads(sys.stdin.readline()); m=req['model']; b=req['backend']
# The librarian's background work: few threads, the GPU when there is one.
torch.set_num_threads(max(1,min(2,b['threads'])))
weight=pathlib.Path(m['weights'])
if hashlib.sha256(weight.read_bytes()).hexdigest()!=m['weights_digest']:raise RuntimeError('Model weights checksum mismatch')
model=get_model(weight.stem.split('-')[0],repo=weight.parent).eval().to(b['device'])
vocals=model.sources.index('vocals'); sr=model.samplerate
def measure(source):
    raw=subprocess.check_output(['ffmpeg','-v','error','-nostdin','-threads','1','-i',source,'-f','f32le','-ar',str(sr),'-ac','2','pipe:1'])
    x=torch.from_numpy(np.frombuffer(raw,dtype='<f4').copy().reshape(-1,2).T)
    ref=x.mean(0); mean=ref.mean(); std=ref.std().clamp_min(1e-8)
    with torch.no_grad():
        y=apply_model(model,((x-mean)/std)[None],device=b['device'],shifts=0,split=True,overlap=.25)[0]
        v=(y[vocals].cpu()*std+mean).mean(0).numpy()
    mix=x.mean(0).numpy(); n=len(mix)//sr
    if n==0:return []
    rms=lambda a:np.sqrt((a[:n*sr].reshape(n,sr).astype(np.float64)**2).mean(1)+1e-12)
    vr,mr=rms(v),rms(mix)
    # Audible: within 30 dB of the track's loudest second (not a fade or a gap).
    audible=mr>mr.max()*10**(-30/20)
    db=20*np.log10(vr/mr)
    return [[round(float(d),1),bool(a)] for d,a in zip(db,audible)]
for line in sys.stdin:
    try:
        out={'ok':{'seconds':measure(json.loads(line)['source'])}}
    except Exception as e:
        out={'error':str(e)[-500:]}
    if torch.cuda.is_available():torch.cuda.empty_cache()
    sys.stdout.write(json.dumps(out)+'\n'); sys.stdout.flush()
