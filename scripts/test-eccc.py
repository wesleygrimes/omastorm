#!/usr/bin/env python3
"""Local WMS/daemon contract checks; generated inputs only."""
import contextlib
import datetime as dt
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time

ROOT=Path(__file__).resolve().parent.parent
spec=importlib.util.spec_from_file_location('replay',ROOT/'scripts/bench-eccc.py')
replay=importlib.util.module_from_spec(spec);spec.loader.exec_module(replay)


def main():
    binary=ROOT/'target/debug/omastorm-engine'
    rain,mask=replay.inputs()
    provider=replay.Provider(rain,mask)
    thread=threading.Thread(target=provider.serve_forever,daemon=True);thread.start()
    def interrupt(signum,frame):raise KeyboardInterrupt
    signal.signal(signal.SIGTERM,interrupt)
    try:
        with tempfile.TemporaryDirectory(prefix='et-',dir=ROOT/'target') as directory:
            runtime=Path(directory)
            env={k:v for k,v in os.environ.items() if not k.startswith('OMASTORM_')}
            env.update(XDG_RUNTIME_DIR=str(runtime),XDG_CACHE_HOME=str(runtime/'cache'),OMASTORM_ECCC_URL=f'http://127.0.0.1:{provider.server_port}/')
            log_path=runtime/'engine.log'
            with log_path.open('w') as log:
                engine=subprocess.Popen([str(binary),'serve'],env=env,stdout=subprocess.DEVNULL,stderr=log)
            client=None
            try:
                client=replay.Client(runtime/'omastorm/engine.sock')
                assert client.hello['gridView']
                client.send({'type':'follow','enabled':False})
                client.next(lambda m:m['type']=='state' and not m['navigation']['follow'])
                def view(lon,identity):
                    client.send({'type':'view_center','lat':50.,'lon':lon,'viewId':identity,'bounds':{'west':lon-1,'east':lon+1,'south':49.,'north':51.}})
                def fixture():
                    client.send({'type':'select_source','id':'fixture-mosaic'})
                    client.next(lambda m:m['type']=='state' and m.get('selection',{}).get('sourceId')=='fixture-mosaic')
                def select():client.send({'type':'select_source','id':'eccc'})
                def frame():
                    stamp=provider.stamp.isoformat().replace('+00:00','Z')
                    return client.next(lambda m:m['type']=='state' and m.get('selection',{}).get('sourceId')=='eccc' and m.get('frame',{}).get('scanTime')==stamp)
                def validity(state,identity):
                    return client.next(lambda m:m['type']=='grid_validity' and m['frameId']==state['frame']['id'] and m['viewId']==identity)
                view(-91.,'known');select();known=frame();v=validity(known,'known')
                assert not v['unfetched'] and v['counts']['measured']>0 and v['counts']['noEcho']>0
                # Missing or mismatched mask never establishes outside/dry.
                fixture();provider.mask=b'<ServiceException>NoMatch</ServiceException>'
                provider.stamp+=dt.timedelta(minutes=6);select();unknown=frame();u=validity(unknown,'known')
                assert u['counts']['measured']==v['counts']['measured']
                assert u['counts']['unknown']>0 and u['counts']['noEcho']==u['counts']['outside']==0
                provider.mask=mask
                # Palette ambiguity and malformed TIME reject the rain frame.
                for body in [b'<ServiceException>NoMatch</ServiceException>',replay.png(bytes([1,2,3,255])*1024**2),replay.png(bytes([153,204,255,128])*1024**2)]:
                    fixture();provider.rain=body;provider.stamp+=dt.timedelta(minutes=6);select()
                    failed=client.next(lambda m:m['type']=='state' and m['connection']['status']=='offline')
                    assert not failed['frame']['scanTime']
                provider.rain=rain
                # Late old-source/old-region response, then the same source
                # reselected. The only dated frame must be the latest TIME.
                fixture();provider.delay=.3;provider.stamp+=dt.timedelta(minutes=6)
                before=provider.requests;select()
                deadline=time.monotonic()+3
                while provider.requests<before+3:
                    if time.monotonic()>deadline:raise TimeoutError('delayed rain did not start')
                    time.sleep(.002)
                fixture();provider.stamp+=dt.timedelta(minutes=6);view(-113.,'new-region');provider.delay=0;select()
                current=frame();c=validity(current,'new-region')
                assert current['frame']['id']!=known['frame']['id'] and len(current['timeline'])==1
                assert not c['unfetched']
                assert provider.peak_active==1
                # A stalled body exercises the actual 30-second HTTP timeout.
                fixture();provider.delay=32;provider.stamp+=dt.timedelta(minutes=6)
                began=time.monotonic();select()
                client.next(lambda m:m['type']=='state' and m['connection']['status']=='offline')
                timeout_elapsed=time.monotonic()-began
                assert 29<=timeout_elapsed<60
                provider.delay=0
                # Re-select after a source change to verify a failed newest
                # observation remains retryable under a new poller generation.
                fixture();provider.stamp+=dt.timedelta(minutes=6);select();retried=frame()
                assert retried['frame']['scanTime']==provider.stamp.isoformat().replace('+00:00','Z')
                print(f'ECCC body timeout: {timeout_elapsed:.2f}s; retry succeeded')
                # Source generation retains its selection footprint in hello.
                source=next(s for s in client.hello['sources'] if s['id']=='eccc')
                assert len(source['selectionFootprint']['coordinates'])==4
                print('ECCC mock provider: validity, mask failure, palette rejection, source/region cancellation passed')
            finally:
                if client:client.close()
                with contextlib.suppress(Exception):
                    subprocess.run([str(binary),'stop'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True)
                try:engine.wait(timeout=5)
                except subprocess.TimeoutExpired:engine.terminate();engine.wait(timeout=5)
                if engine.returncode not in (0,None):print(log_path.read_text())
    finally:
        provider.shutdown();provider.server_close();thread.join(timeout=5)


if __name__=='__main__':main()
