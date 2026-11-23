#!/usr/bin/env python3
"""Install Patch's optional isolated separator runtime, never into system Python."""
import argparse, hashlib, json, os, pathlib, shutil, subprocess, tempfile, urllib.request
p=argparse.ArgumentParser();p.add_argument('--device',choices=['cpu','cuda','auto'],default='cpu');p.add_argument('--reuse-runtime',type=pathlib.Path);a=p.parse_args()
home=pathlib.Path.home();data=pathlib.Path(os.environ.get('XDG_DATA_HOME',home/'.local/share'))/'omarchy-patch/stems';config=pathlib.Path(os.environ.get('XDG_CONFIG_HOME',home/'.config'))/'omarchy-patch';state=pathlib.Path(os.environ.get('XDG_CACHE_HOME',home/'.cache'))/'omarchy-patch/stems';state.mkdir(parents=True,exist_ok=True);data.mkdir(parents=True,exist_ok=True)
status=pathlib.Path(os.environ.get('PATCH_STEM_SETUP_STATUS',state/'setup.json'));status.parent.mkdir(parents=True,exist_ok=True)
def update(state,progress,error=None):
 temp=status.with_suffix('.tmp');temp.write_text(json.dumps(dict(state=state,progress=progress,error=error)));temp.replace(status)
def run(cmd):subprocess.run([str(x) for x in cmd],check=True)
try:
 update('preparing',1)
 device=a.device
 if device=='auto':
  try:device='cuda' if subprocess.run(['nvidia-smi'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0 else 'cpu'
  except FileNotFoundError:device='cpu'
 # CUDA toolkit wheels require several GiB. Reserve room before installing.
 minimum=9*1024**3 if device=='cuda' else 3*1024**3
 if not a.reuse_runtime and shutil.disk_usage(data).free<minimum:raise RuntimeError('Insufficient free space for the optional separator runtime')
 runtime=a.reuse_runtime.resolve() if a.reuse_runtime else data/'runtime'
 if not a.reuse_runtime:
  if not shutil.which('uv'):raise RuntimeError('Install uv to prepare the optional isolated runtime')
  if not (runtime/'bin/python').exists():run(['uv','venv','--python','3.11',runtime])
  index='https://download.pytorch.org/whl/cu128' if device=='cuda' else 'https://download.pytorch.org/whl/cpu'
  run(['uv','pip','install','--python',runtime/'bin/python','--index-url',index,'torch==2.10.0','torchaudio==2.10.0'])
  update('preparing',50)
  run(['uv','pip','install','--python',runtime/'bin/python','demucs==4.1.0','numpy==2.3.5','soundfile==0.13.1'])
 weight=data/'955717e8-8726e21a.th';digest='8726e21a993978c7ba086d3872e7608d7d5bfca646ca4aca459ffda844faa8b4'
 if not weight.exists() or hashlib.sha256(weight.read_bytes()).hexdigest()!=digest:
  temporary=weight.with_suffix('.download');urllib.request.urlretrieve('https://dl.fbaipublicfiles.com/demucs/hybrid_transformer/955717e8-8726e21a.th',temporary)
  if hashlib.sha256(temporary.read_bytes()).hexdigest()!=digest:temporary.unlink();raise RuntimeError('Separator weight checksum mismatch')
  temporary.replace(weight)
 update('preparing',90)
 adapter=data/'demucs_adapter.py';source=pathlib.Path(__file__).with_name('demucs_adapter.py')
 # Installed, this script sits beside the adapter it would copy.
 if source.resolve()!=adapter.resolve():shutil.copyfile(source,adapter)
 run([runtime/'bin/python','-c','import torch,torchaudio,demucs,soundfile,numpy'])
 config.mkdir(parents=True,exist_ok=True)
 value=dict(model=dict(id='htdemucs',version='955717e8',weights_digest=digest,layout=['vocals','drums','bass','other'],profile='balanced-v1',adapter=str(adapter),python=str(runtime/'bin/python'),weights=str(weight)),backend=dict(id='pytorch',device=device,precision='float32',threads=4))
 target=config/'stems.json';temporary=target.with_suffix('.tmp');temporary.write_text(json.dumps(value,indent=2));temporary.replace(target)
 update('ready',100)
except Exception as e:
 update('failed',0,str(e));raise
