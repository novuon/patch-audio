#!/usr/bin/env python3
"""Model-neutral process protocol adapter for MSST BS-RoFormer/SCNet artifacts."""
import json, sys, os, subprocess, pathlib, hashlib
req=json.load(sys.stdin); m=req['model']; b=req['backend']; out=pathlib.Path(req['output'])
# Profile is a JSON object encoded as text: repository/config are explicit adapter settings.
settings=json.loads(m['profile']);sys.path.insert(0,settings['repository'])
import torch, numpy as np, soundfile as sf, yaml
if hashlib.sha256(pathlib.Path(m['weights']).read_bytes()).hexdigest()!=m['weights_digest']:raise RuntimeError('Weights checksum mismatch')
cfg=yaml.load(pathlib.Path(settings['config']).read_text(),Loader=yaml.FullLoader)
if settings['architecture']=='bs_roformer':
 from models.bs_roformer.bs_roformer import BSRoformer
 model=BSRoformer(**cfg['model'])
elif settings['architecture']=='scnet':
 from models.scnet.scnet import SCNet
 model=SCNet(**cfg['model'])
else:raise RuntimeError('Unsupported adapter architecture')
torch.set_num_threads(max(1,min(8,b['threads'])))
state=torch.load(m['weights'],map_location='cpu',weights_only=False)
if 'state_dict' in state:state=state['state_dict']
if 'model' in state:state=state['model']
model.load_state_dict(state);model.to(b['device']).eval()
rate=cfg['audio']['sample_rate'];raw=subprocess.check_output(['ffmpeg','-v','error','-nostdin','-i',req['source'],'-f','f32le','-ar',str(rate),'-ac','2','pipe:1'])
x=np.frombuffer(raw,dtype='<f4').copy().reshape(-1,2).T;n=x.shape[1];chunk=cfg['audio']['chunk_size'];overlap=2;hop=chunk//overlap
acc=np.zeros((4,2,n+chunk),np.float32);weights=np.zeros(n+chunk,np.float32);window=np.hanning(chunk).astype(np.float32);window[:hop]=np.maximum(window[:hop],.001)
with torch.inference_mode():
 for start in range(0,n,hop):
  piece=np.pad(x[:,start:start+chunk],((0,0),(0,max(0,start+chunk-n))))
  y=model(torch.from_numpy(piece)[None].to(b['device']))[0].cpu().numpy()
  acc[:,:,start:start+chunk]+=y*window;weights[start:start+chunk]+=window
  (out/'progress.json').write_text(json.dumps({'progress':int(min(80,80*(start+hop)/n))}))
y=acc[:,:,:n]/np.maximum(weights[:n],1e-8)
sources=cfg['training']['instruments']
for stem,audio in zip(sources,y):sf.write(out/(stem+'.wav'),audio.T,rate,subtype='FLOAT')
(out/'output.json').write_text(json.dumps({'sample_rate':rate,'frames':n,'available':sources}))
