#!/usr/bin/env python3
"""Patch's isolated reference adapter; model and backend selected by the request."""
import json, sys, subprocess, pathlib, hashlib
import numpy as np
import torch
import soundfile as sf
from demucs.pretrained import get_model
from demucs.apply import apply_model
req=json.load(sys.stdin); m=req['model']; b=req['backend']; out=pathlib.Path(req['output'])
torch.set_num_threads(max(1,min(8,b['threads'])))
weight=pathlib.Path(m['weights'])
if hashlib.sha256(weight.read_bytes()).hexdigest()!=m['weights_digest']:raise RuntimeError('Model weights checksum mismatch')
model=get_model(weight.stem.split('-')[0],repo=weight.parent);model.eval()
raw=subprocess.check_output(['ffmpeg','-v','error','-nostdin','-i',req['source'],'-f','f32le','-ar',str(model.samplerate),'-ac','2','pipe:1'])
x=torch.from_numpy(np.frombuffer(raw,dtype='<f4').copy().reshape(-1,2).T)
ref=x.mean(0); mean=ref.mean(); std=ref.std().clamp_min(1e-8)
def progress(d):
    offset=d.get('segment_offset',0)
    value=min(80,int(80*(offset+(model.segment*model.samplerate if d.get('state')=='end' else 0))/x.shape[-1]))
    (out/'progress.json').write_text(json.dumps({'progress':value}))
with torch.no_grad():
    y=apply_model(model.to(b['device']),((x-mean)/std)[None],device=b['device'],shifts=0,split=True,overlap=.25,progress=True,callback=progress)[0].cpu()*std+mean
for stem,audio in zip(model.sources,y):sf.write(out/(stem+'.wav'),audio.numpy().T,model.samplerate,subtype='FLOAT')
(out/'output.json').write_text(json.dumps({'sample_rate':model.samplerate,'frames':x.shape[-1],'available':model.sources}))
