#!/usr/bin/env python3
"""Separate live ECCC smoke. Owns one scratch daemon; no UI or shared daemon."""
import argparse
import contextlib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

ROOT=Path(__file__).resolve().parent.parent
spec=importlib.util.spec_from_file_location('replay',ROOT/'scripts/bench-eccc.py')
replay=importlib.util.module_from_spec(spec);spec.loader.exec_module(replay)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,default=ROOT/'target/release/omastorm-engine')
    parser.add_argument('--output',type=Path,default=ROOT/'target/eccc-live-smoke')
    args=parser.parse_args();args.output.mkdir(parents=True,exist_ok=True)
    def interrupt(signum,frame):raise KeyboardInterrupt
    signal.signal(signal.SIGTERM,interrupt)
    with tempfile.TemporaryDirectory(prefix='es-',dir=ROOT/'target') as directory:
        runtime=Path(directory)
        env={k:v for k,v in os.environ.items() if not k.startswith('OMASTORM_')}
        env.update(XDG_RUNTIME_DIR=str(runtime),XDG_CACHE_HOME=str(runtime/'cache'))
        with (args.output/'engine.log').open('w') as log:
            engine=subprocess.Popen([str(args.binary.resolve()),'serve'],env=env,stdout=subprocess.DEVNULL,stderr=log)
        client=None
        try:
            client=replay.Client(runtime/'omastorm/engine.sock')
            client.send({'type':'follow','enabled':False})
            client.next(lambda m:m['type']=='state' and not m['navigation']['follow'])
            view={'type':'view_center','lat':51.25,'lon':-91.9,'viewId':'live-131',
                  'bounds':{'west':-94.,'south':49.,'east':-89.,'north':53.}}
            client.send(view)
            started=time.perf_counter();client.send({'type':'select_source','id':'eccc'})
            state=client.next(lambda m:m['type']=='state' and m.get('selection',{}).get('sourceId')=='eccc' and bool(m.get('frame',{}).get('scanTime')))
            latency=(time.perf_counter()-started)*1000
            validity=client.next(lambda m:m['type']=='grid_validity' and m['frameId']==state['frame']['id'] and m['viewId']=='live-131')
            texture=runtime/'omastorm'/state['frame']['texture']
            report={'scope':'LIVE provider smoke, separate from synthetic resource measurements',
                    'binary_sha256':hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                    'first_frame_ms':latency,'texture_bytes':texture.stat().st_size,
                    'texture_sha256':hashlib.sha256(texture.read_bytes()).hexdigest(),
                    'view':view,'state':state,'validity':validity,
                    'resources':replay.sample(engine.pid,runtime,'live-first-frame')}
            (args.output/'live.json').write_text(json.dumps(report,indent=2)+'\n')
            (args.output/'wire.json').write_text(json.dumps({'hello':client.hello,'view':view,'state':state,'validity':validity},indent=2)+'\n')
            print(json.dumps({k:report[k] for k in ('scope','first_frame_ms','texture_bytes')}))
            print(state['frame']['scanTime'],state['frame']['geotransform'],validity['counts'])
        finally:
            if client:client.close()
            with contextlib.suppress(Exception):subprocess.run([str(args.binary.resolve()),'stop'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            try:engine.wait(timeout=5)
            except subprocess.TimeoutExpired:engine.terminate();engine.wait(timeout=5)


if __name__=='__main__':main()
