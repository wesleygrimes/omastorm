#!/usr/bin/env python3
"""Isolated maximum-size ECCC replay; real 30-second texture retirement."""
import argparse
import datetime as dt
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('baseline', ROOT / 'scripts/bench-engine.py')
baseline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(baseline)
sys.path.insert(0, str(ROOT / 'scripts/tooling'))
from eccc_mock import Client, Provider, inputs  # noqa: E402


def sample(pid, runtime, phase):
    result = baseline.resources(pid,runtime)
    rollup = Path(f'/proc/{pid}/smaps_rollup').read_text()
    result['pss_kib']=int(next(l.split()[1] for l in rollup.splitlines() if l.startswith('Pss:')))
    result['phase']=phase
    eccc=list((runtime/'omastorm/tex').glob('mosaic-eccc-*'))
    result['eccc_bytes']=sum(p.stat().st_size for p in eccc)
    result['eccc_files']=len(eccc)
    if result['eccc_bytes']>64*1024**2: raise AssertionError(result)
    return result


def cycle(binary, provider, updates, output, index):
    with tempfile.TemporaryDirectory(prefix='ec-',dir=ROOT/'target') as directory:
        runtime=Path(directory)
        env={k:v for k,v in os.environ.items() if not k.startswith('OMASTORM_')}
        env.update(XDG_RUNTIME_DIR=str(runtime),XDG_CACHE_HOME=str(runtime/'cache'),
                   OMASTORM_ECCC_URL=f'http://127.0.0.1:{provider.server_port}/')
        log_path=output/f'run-{index}.log'
        def start():
            log=log_path.open('a')
            process=subprocess.Popen([str(binary),'serve'],env=env,stdout=subprocess.DEVNULL,stderr=log)
            log.close()
            try: client=Client(runtime/'omastorm/engine.sock')
            except BaseException:
                subprocess.run([str(binary),'stop'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
                process.wait(timeout=5);raise
            client.send({'type':'follow','enabled':False})
            client.next(lambda m:m['type']=='state' and not m['navigation']['follow'])
            return process,client
        def stop(process,client):
            client.close()
            subprocess.run([str(binary),'stop'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True)
            process.wait(timeout=5)
        process,client=start()
        samples=[sample(process.pid,runtime,'warm-baseline')]
        latencies=[]
        references=[]
        region_switches=0
        last_region=None
        try:
            for i in range(updates):
                provider.stamp += dt.timedelta(minutes=6)
                provider.failure='malformed' if i in (9,99,199) else None
                client.send({'type':'select_source','id':'fixture-mosaic'})
                client.next(lambda m:m['type']=='state' and m['selection']['sourceId']=='fixture-mosaic')
                # Three frames per settled region; every update switches source.
                lat,lon=(50.,-91.) if (i//3)%2 else (53.5,-113.5)
                client.send({'type':'view_center','lat':lat,'lon':lon,'viewId':f'view-{i}',
                             'bounds':{'west':lon-1,'east':lon+1,'south':lat-1,'north':lat+1}})
                before=time.perf_counter()
                client.send({'type':'select_source','id':'eccc'})
                if provider.failure:
                    client.next(lambda m:m['type']=='state' and m['connection']['status']=='offline')
                    provider.failure=None
                    client.send({'type':'select_source','id':'fixture-mosaic'})
                    client.next(lambda m:m['type']=='state' and m['selection']['sourceId']=='fixture-mosaic')
                    client.send({'type':'select_source','id':'eccc'})
                stamp=provider.stamp.isoformat().replace('+00:00','Z')
                state=client.next(lambda m:m['type']=='state' and m.get('selection',{}).get('sourceId')=='eccc' and m.get('frame',{}).get('scanTime')==stamp)
                texture=runtime/'omastorm'/state['frame']['texture']
                assert texture.is_file()
                assert state['frame']['width']==state['frame']['height']==1024
                assert len(state['timeline'])==1
                region=state['frame']['id'].rsplit('-',1)[0]
                if last_region is not None and region!=last_region: region_switches+=1
                last_region=region
                validity=client.next(lambda m:m['type']=='grid_validity' and m['frameId']==state['frame']['id'] and m['viewId']==f'view-{i}')
                assert validity['counts']['outside']==0
                latencies.append((time.perf_counter()-before)*1000)
                references.append(texture)
                samples.append(sample(process.pid,runtime,f'frame-{i}'))
                # Grace files are real: a recently replaced path must survive.
                assert texture.is_file()
                if i and latencies[-1]<1000: assert references[-2].is_file()
                if i==updates//2:
                    stop(process,client)
                    process,client=start()
                    samples.append(sample(process.pid,runtime,'restart'))
                if i%50==0: print(f'cycle {index+1}: {i}/{updates}',flush=True)
            provider.failure=None
            client.send({'type':'select_source','id':'fixture-mosaic'})
            client.next(lambda m:m['type']=='state' and m['selection']['sourceId']=='fixture-mosaic')
            # Stop only the owned daemon. Its restart's orphan paths also age
            # through the same cleanup ticks, never a shortened test grace.
            until=time.monotonic()+32
            while time.monotonic()<until:
                samples.append(sample(process.pid,runtime,'retirement'))
                time.sleep(1)
            samples.append(sample(process.pid,runtime,'settled'))
            assert samples[-1]['eccc_bytes']==0
        finally:
            stop(process,client)
        return {'resources':samples,'frame_latency_ms':latencies,'median_frame_latency_ms':statistics.median(latencies),
                'engine_peak_pss_kib':max(s['pss_kib'] for s in samples),
                'warm_baseline_pss_kib':samples[0]['pss_kib'],
                'added_engine_peak_pss_kib':max(s['pss_kib'] for s in samples)-samples[0]['pss_kib'],
                'runtime_peak_bytes':max(s['eccc_bytes'] for s in samples),
                'retired_eccc_bytes':samples[-1]['eccc_bytes'],'source_switches':updates,'region_switches':region_switches,
                'failures':sum(i<updates for i in (9,99,199)),'restarts':1}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,default=ROOT/'target/release/omastorm-engine')
    parser.add_argument('--runs',type=int,default=3)
    parser.add_argument('--updates',type=int,default=300)
    parser.add_argument('--output',type=Path,default=ROOT/'target/bench-eccc')
    args=parser.parse_args()
    if args.runs<1 or args.updates<1:parser.error('runs and updates must be positive')
    args.output.mkdir(parents=True,exist_ok=True)
    rain,mask=inputs()
    provider=Provider(rain,mask)
    thread=threading.Thread(target=provider.serve_forever,daemon=True);thread.start()
    def interrupted(signum, frame):raise KeyboardInterrupt
    signal.signal(signal.SIGTERM,interrupted)
    source_snapshot=subprocess.check_output(['git','diff','--binary','HEAD'],cwd=ROOT)
    for name in subprocess.check_output(['git','ls-files','--others','--exclude-standard'],cwd=ROOT,text=True).splitlines():
        source_snapshot+=name.encode()+hashlib.sha256((ROOT/name).read_bytes()).digest()
    report={'schema':1,'source_snapshot_sha256':hashlib.sha256(source_snapshot).hexdigest(),'status':'running','measured_at':dt.datetime.now(dt.timezone.utc).isoformat(),
        'commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
        'working_tree':subprocess.check_output(['git','status','--short'],cwd=ROOT,text=True),
        'binary':str(args.binary.resolve()),'binary_sha256':hashlib.sha256(args.binary.read_bytes()).hexdigest(),
        'harness_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        'host':platform.platform(),'cpu_count':os.cpu_count(),'profile':'release' if 'release' in str(args.binary) else 'debug',
        'rain_sha256':hashlib.sha256(rain).hexdigest(),'rain_bytes':len(rain),'mask_sha256':hashlib.sha256(mask).hexdigest(),
        'dimensions':[1024,1024],'sampling':'/proc status, smaps_rollup, fd, runtime stat after every frame and at one-second retirement ticks',
        'scope':'engine backend; Python mock client/provider and GPU excluded. Combined native client gate belongs to #132.',
        'retirement_seconds':30,'runs':[]}
    path=args.output/'eccc.json'
    try:
        for i in range(args.runs):
            report['runs'].append(cycle(args.binary.resolve(),provider,args.updates,args.output,i))
            path.write_text(json.dumps(report,indent=2)+'\n')
        report['status']='complete'
        report['provider_requests']=provider.requests
        report['provider_peak_active']=provider.peak_active
    except BaseException as error:
        report.update(status='failed',error=repr(error));raise
    finally:
        path.write_text(json.dumps(report,indent=2)+'\n')
        provider.shutdown();provider.server_close();thread.join(timeout=5)
    print(f'Report: {path}',flush=True)


if __name__=='__main__':main()
