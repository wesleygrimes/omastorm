"""Loopback GeoMet WMS and engine socket client for ECCC checks; generated inputs only."""
import contextlib
import datetime as dt
import http.server
import json
import random
import socket
import struct
import time
import urllib.parse
import zlib

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
