#!/usr/bin/env python3
"""Isolated maximum-size ECCC replay; real 30-second texture retirement."""
import argparse
import contextlib
import datetime as dt
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import platform
import random
import signal
import socket
import statistics
import struct
import subprocess
import tempfile
import threading
import time
import urllib.parse
import zlib

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('baseline', ROOT / 'scripts/bench-engine.py')
baseline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(baseline)
COLORS = [bytes.fromhex(c) for c in ('99ccff','0099ff','00ff66','00cc00','009900','006600','ffff00','ffcc00','ff9900','ff6600','ff0000','ff0299','9933cc','660099')]


def png(raw, width=1024, height=1024):
    def chunk(kind, body):
        return struct.pack('>I', len(body)) + kind + body + struct.pack('>I', zlib.crc32(kind + body))
    rows = b''.join(b'\0' + raw[y*width*4:(y+1)*width*4] for y in range(height))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width,height,8,6,0,0,0)) + chunk(b'IDAT', zlib.compress(rows, 1)) + chunk(b'IEND', b'')


def inputs():
    rng = random.Random(131)
    cells = [c+b'\xff' for c in COLORS] + [b'\x00\x00\x00\x00']
    rain = png(b''.join(cells[rng.randrange(15)] for _ in range(1024**2)))
    # All measured cells remain covered; blanks have an ambiguous outer guard.
    mask = png(b'\0' * (1024**2*4))
    return rain, mask


class Provider(http.server.HTTPServer):
    def __init__(self, rain, mask):
        super().__init__(('127.0.0.1', 0), Handler)
        self.rain, self.mask = rain, mask
        self.stamp = dt.datetime(2026,10,1,12,tzinfo=dt.timezone.utc)
        self.failure = None
        self.delay = 0
        self.requests = 0
        self.active = 0
        self.peak_active = 0


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_GET(self):
        p = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
        self.server.requests += 1
        self.server.active += 1
        self.server.peak_active = max(self.server.active,self.server.peak_active)
        try:
            stamp = self.server.stamp.isoformat().replace('+00:00','Z')
            if p.get('REQUEST') == ['GetCapabilities']:
                layer = p['LAYERS'][0]
                body = f'<Layer><Name>{layer}</Name><Dimension name="time">{stamp}/{stamp}/PT6M</Dimension></Layer>'.encode()
            elif p.get('TIME') != [stamp]:
                body = b'<ServiceException>NoMatch</ServiceException>'
            elif self.server.failure == 'malformed':
                body = b'<ServiceException>NoMatch</ServiceException>'
            elif p['LAYERS'][0].endswith('.INV'):
                body = self.server.mask
            else:
                body = self.server.rain
                time.sleep(self.server.delay)
            self.send_response(200)
            self.send_header('Content-Length',str(len(body)))
            self.end_headers()
            with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                self.wfile.write(body)
        finally:
            self.server.active -= 1


class Client:
    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(65)
        until = time.monotonic()+30
        while True:
            try:
                self.socket.connect(str(path)); break
            except (FileNotFoundError,ConnectionRefusedError):
                if time.monotonic()>until: raise
                time.sleep(.002)
        self.wire = self.socket.makefile('rb')
        self.hello = self.next(lambda m:m['type']=='hello')
        self.next(lambda m:m['type']=='state')
    def send(self, message):
        self.socket.sendall(json.dumps(message).encode()+b'\n')
    def next(self, predicate):
        deadline=time.monotonic()+65
        while time.monotonic()<deadline:
            line=self.wire.readline()
            if not line: raise RuntimeError('engine disconnected')
            m=json.loads(line)
            if m['type']=='error': raise RuntimeError(m)
            if predicate(m): return m
        raise TimeoutError('expected replay response')
    def close(self):
        self.wire.close();self.socket.close()


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
