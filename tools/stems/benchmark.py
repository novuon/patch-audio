#!/usr/bin/env python3
"""Reproducible local adapter benchmark; use licensed multitracks for quality metrics."""
import argparse,json,pathlib,subprocess,time,psutil,hashlib,resource
p=argparse.ArgumentParser();p.add_argument('config');p.add_argument('source');p.add_argument('output');args=p.parse_args()
cfg=json.loads(pathlib.Path(args.config).read_text());root=pathlib.Path(args.output);root.mkdir(parents=True,exist_ok=True)
rows=[]
for name,v in cfg.items():
 output=root/name;output.mkdir(exist_ok=True);request=dict(v,source=str(pathlib.Path(args.source).resolve()),output=str(output.resolve()))
 start=time.monotonic();log=open(output/'run.log','w');child=subprocess.Popen([v['model']['python'],v['model']['adapter']],stdin=subprocess.PIPE,stdout=log,stderr=log);child.stdin.write(json.dumps(request).encode());child.stdin.close();peak=0;gpu=0
 while child.poll() is None:
  try:peak=max(peak,psutil.Process(child.pid).memory_info().rss)
  except psutil.Error:pass
  try:
   result=subprocess.check_output(['nvidia-smi','--query-compute-apps=pid,used_gpu_memory','--format=csv,noheader,nounits'],text=True)
   for line in result.splitlines():
    a,b=line.split(',');
    if int(a)==child.pid:gpu=max(gpu,int(b.strip())*1024*1024)
  except (ValueError,subprocess.SubprocessError):pass
  time.sleep(.1)
 elapsed=time.monotonic()-start;row={'candidate':name,'exit':child.returncode,'elapsed_s':elapsed,'peak_rss_bytes':peak,'peak_vram_bytes':gpu,'source_sha256':hashlib.sha256(pathlib.Path(args.source).read_bytes()).hexdigest(),'config':v}
 if (output/'output.json').exists():
  desc=json.loads((output/'output.json').read_text());duration=desc['frames']/desc['sample_rate'];row.update(duration_s=duration,speed=duration/elapsed)
  import soundfile as sf,numpy as np
  stems=[sf.read(output/(s+'.wav'),dtype='float32')[0] for s in desc['available']];row['output_bytes']=sum((output/(s+'.wav')).stat().st_size for s in desc['available']);row['finite']=all(np.isfinite(s).all() for s in stems)
  row['frames_aligned']=len({len(s) for s in stems})==1
  raw=subprocess.check_output(['ffmpeg','-v','error','-i',args.source,'-f','f32le','-ar',str(desc['sample_rate']),'-ac','2','pipe:1']);x=np.frombuffer(raw,dtype='<f4').reshape(-1,2);diff=x-sum(stems);row['reconstruction_rms']=float(np.sqrt(np.mean(diff.astype('float64')**2)));row['quality_note']='Reconstruction only; no ground-truth SDR or blind listening score claimed.'
 rows.append(row);(root/'results.json').write_text(json.dumps(rows,indent=2));print(name,child.returncode,round(elapsed,2),flush=True)
