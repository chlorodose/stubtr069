#!/usr/bin/env python3
"""Tiny mock ACS used to exercise stubtr069 end to end (stdlib only).

Requires HTTP Digest from the CPE, queues a fixed list of RPCs for every session
that starts with an Inform, serves /fw.bin for Download, and (with --connreq URL)
fires a Digest-authenticated Connection Request at the CPE after the first session.
"""
import argparse
import hashlib
import secrets
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

NS = "urn:dslforum-org:cwmp-1-2"
USER, PASS = "acs", "acspass"
REALM = "mockacs"
FILE = b"stub firmware payload\n" * 1000


def env(rpc_id, body):
    return (
        '<?xml version="1.0"?><soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" '
        'xmlns:soapenc="http://schemas.xmlsoap.org/soap/encoding/" '
        'xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" '
        f'xmlns:cwmp="{NS}"><soapenv:Header><cwmp:ID soapenv:mustUnderstand="1">{rpc_id}</cwmp:ID></soapenv:Header>'
        f"<soapenv:Body>{body}</soapenv:Body></soapenv:Envelope>"
    )


def rpc_queue(base):
    sv = lambda n, v, t="xsd:string": (
        f'<ParameterValueStruct><Name>{n}</Name><Value xsi:type="{t}">{v}</Value></ParameterValueStruct>'
    )
    return [
        ("GetRPCMethods", "<cwmp:GetRPCMethods/>"),
        ("GetParameterValues", '<cwmp:GetParameterValues><ParameterNames soapenc:arrayType="xsd:string[2]">'
         "<string>Device.DeviceInfo.</string><string>Device.ManagementServer.Password</string></ParameterNames>"
         "</cwmp:GetParameterValues>"),
        ("SetParameterValues", '<cwmp:SetParameterValues><ParameterList soapenc:arrayType="cwmp:ParameterValueStruct[3]">'
         + sv("Device.DeviceInfo.ProvisioningCode", "ABC123")
         + sv("Device.ManagementServer.Password", PASS)
         + sv("Device.ManagementServer.PeriodicInformEnable", "1", "xsd:boolean")
         + "</ParameterList><ParameterKey>key-1</ParameterKey></cwmp:SetParameterValues>"),
        ("SetParameterValues(lenient)", '<cwmp:SetParameterValues><ParameterList soapenc:arrayType="cwmp:ParameterValueStruct[2]">'
         + sv("Device.DeviceInfo.Manufacturer", "x") + sv("Device.Nope.Flag", "true", "xsd:boolean")
         + "</ParameterList><ParameterKey>key-2</ParameterKey></cwmp:SetParameterValues>"),
        ("GetParameterValues(unknown)", '<cwmp:GetParameterValues><ParameterNames soapenc:arrayType="xsd:string[1]"><string>Device.Ghost.Value</string></ParameterNames></cwmp:GetParameterValues>'),
        ("GetParameterNames", "<cwmp:GetParameterNames><ParameterPath>Device.</ParameterPath><NextLevel>1</NextLevel></cwmp:GetParameterNames>"),
        ("AddObject", "<cwmp:AddObject><ObjectName>Device.Hosts.Host.</ObjectName><ParameterKey>key-3</ParameterKey></cwmp:AddObject>"),
        ("GetParameterNames(all)", "<cwmp:GetParameterNames><ParameterPath>Device.Hosts.</ParameterPath><NextLevel>0</NextLevel></cwmp:GetParameterNames>"),
        ("SetParameterAttributes", '<cwmp:SetParameterAttributes><ParameterList soapenc:arrayType="cwmp:SetParameterAttributesStruct[1]">'
         "<SetParameterAttributesStruct><Name>Device.DeviceInfo.ProvisioningCode</Name><NotificationChange>1</NotificationChange>"
         "<Notification>2</Notification><AccessListChange>1</AccessListChange><AccessList><string>Subscriber</string></AccessList>"
         "</SetParameterAttributesStruct></ParameterList></cwmp:SetParameterAttributes>"),
        ("GetParameterAttributes", '<cwmp:GetParameterAttributes><ParameterNames><string>Device.DeviceInfo.ProvisioningCode</string></ParameterNames></cwmp:GetParameterAttributes>'),
        ("DeleteObject", "<cwmp:DeleteObject><ObjectName>Device.Hosts.Host.1.</ObjectName><ParameterKey>key-4</ParameterKey></cwmp:DeleteObject>"),
        ("Download", f"<cwmp:Download><CommandKey>dl-1</CommandKey><FileType>1 Firmware Upgrade Image</FileType>"
         f"<URL>{base}/fw.bin</URL><Username>dl</Username><Password>dlpass</Password><FileSize>{len(FILE)}</FileSize>"
         "<TargetFileName>fw.bin</TargetFileName><DelaySeconds>1</DelaySeconds><SuccessURL></SuccessURL><FailureURL></FailureURL></cwmp:Download>"),
        ("Download(https)", "<cwmp:Download><CommandKey>dl-2</CommandKey><FileType>3 Vendor Configuration File</FileType>"
         "<URL>https://example.invalid/c.cfg</URL><Username></Username><Password></Password><FileSize>0</FileSize>"
         "<TargetFileName></TargetFileName><DelaySeconds>0</DelaySeconds><SuccessURL></SuccessURL><FailureURL></FailureURL></cwmp:Download>"),
        ("Upload", "<cwmp:Upload><CommandKey>up</CommandKey><FileType>2 Vendor Log File</FileType><URL>http://x/</URL>"
         "<Username></Username><Password></Password><DelaySeconds>0</DelaySeconds></cwmp:Upload>"),
        ("ScheduleInform", "<cwmp:ScheduleInform><DelaySeconds>2</DelaySeconds><CommandKey>si-1</CommandKey></cwmp:ScheduleInform>"),
    ]


class Auth:
    nonces = set()

    @staticmethod
    def challenge():
        n = secrets.token_hex(16)
        Auth.nonces.add(n)
        return f'Digest realm="{REALM}", qop="auth", nonce="{n}", opaque="{secrets.token_hex(8)}", algorithm=MD5'

    @staticmethod
    def ok(header, method):
        if not header or not header.startswith("Digest "):
            return False
        p = {}
        for part in header[7:].split(", "):
            k, _, v = part.partition("=")
            p[k.strip()] = v.strip().strip('"')
        if p.get("username") != USER or p.get("nonce") not in Auth.nonces:
            return False
        h = lambda s: hashlib.md5(s.encode()).hexdigest()
        ha1 = h(f"{USER}:{REALM}:{PASS}")
        ha2 = h(f"{method}:{p['uri']}")
        exp = h(f"{ha1}:{p['nonce']}:{p['nc']}:{p['cnonce']}:auth:{ha2}")
        return exp == p.get("response")


sessions = {}  # cookie -> {"queue": [...], "i": 0, "pending": name}
STATE = {"informs": 0}


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "mockacs"

    def log_message(self, *a):
        pass

    def reply(self, code, body=b"", headers=()):
        self.send_response(code)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path != "/fw.bin":
            return self.reply(404)
        auth = self.headers.get("Authorization", "")
        import base64
        if auth != "Basic " + base64.b64encode(b"dl:dlpass").decode():
            return self.reply(401, headers=[("WWW-Authenticate", 'Basic realm="files"')])
        print("[ACS] file server: served /fw.bin", flush=True)
        self.reply(200, FILE, [("Content-Type", "application/octet-stream")])

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(n).decode() if n else ""
        if not Auth.ok(self.headers.get("Authorization"), "POST"):
            return self.reply(401, headers=[("WWW-Authenticate", Auth.challenge())])
        cookie = self.headers.get("Cookie", "")
        sid = cookie.split("sid=")[1].split(";")[0] if "sid=" in cookie else None
        hdrs = []
        if "Inform>" in body or "Inform " in body and "<cwmp:Inform" in body:
            sid = secrets.token_hex(8)
            hdrs = [("Set-Cookie", f"sid={sid}; Path=/")]
            events = [l.split("<")[0] for l in body.split("<EventCode>")[1:]]
            print(f"[ACS] Inform #{STATE['informs']} events={events}", flush=True)
            STATE["informs"] += 1
            base = f"http://127.0.0.1:{self.server.server_port}"
            q = rpc_queue(base) if STATE["informs"] == 1 else []
            sessions[sid] = {"queue": q, "i": 0}
            rid = body.split("<cwmp:ID")[1].split(">")[1].split("<")[0]
            return self.reply(200, env(rid, "<cwmp:InformResponse><MaxEnvelopes>1</MaxEnvelopes></cwmp:InformResponse>").encode(),
                              hdrs + [("Content-Type", "text/xml")])
        s = sessions.get(sid)
        if s is None:
            return self.reply(204)
        if body:
            tag = body.split("<soap-env:Body>")[1][:600] if "<soap-env:Body>" in body else body[:600]
            print(f"[ACS] <- reply to {s.get('last')}: {tag[:500]}", flush=True)
            if "<cwmp:TransferComplete>" in body:
                print("[ACS] got TransferComplete", flush=True)
                return self.reply(200, env("tc", "<cwmp:TransferCompleteResponse/>").encode(), [("Content-Type", "text/xml")])
        if s["i"] < len(s["queue"]):
            name, xml = s["queue"][s["i"]]
            s["i"] += 1
            s["last"] = name
            print(f"[ACS] -> {name}", flush=True)
            return self.reply(200, env(f"acs-{s['i']}", xml).encode(), [("Content-Type", "text/xml")])
        print("[ACS] session done", flush=True)
        self.reply(204)


def connection_request(url):
    """Digest-authenticated GET against the CPE's connection request URL."""
    cpe_user, cpe_pass = "stub", "stub"
    try:
        urllib.request.urlopen(url, timeout=5)
    except urllib.error.HTTPError as e:
        ch = e.headers["WWW-Authenticate"]
        p = {k.strip(): v.strip().strip('"') for k, _, v in (x.partition("=") for x in ch[7:].split(", "))}
        h = lambda s: hashlib.md5(s.encode()).hexdigest()
        path = "/" + url.split("/", 3)[3] if url.count("/") >= 3 else "/"
        cn, nc = secrets.token_hex(4), "00000001"
        ha1 = h(f"{cpe_user}:{p['realm']}:{cpe_pass}")
        ha2 = h(f"GET:{path}")
        resp = h(f"{ha1}:{p['nonce']}:{nc}:{cn}:auth:{ha2}")
        auth = (f'Digest username="{cpe_user}", realm="{p["realm"]}", nonce="{p["nonce"]}", uri="{path}", '
                f'algorithm=MD5, response="{resp}", opaque="{p.get("opaque","")}", qop=auth, nc={nc}, cnonce="{cn}"')
        r = urllib.request.urlopen(urllib.request.Request(url, headers={"Authorization": auth}), timeout=5)
        print(f"[ACS] connection request -> HTTP {r.status}", flush=True)
        # A wrong password must be refused.
        bad = auth.replace(resp, "0" * 32)
        try:
            urllib.request.urlopen(urllib.request.Request(url, headers={"Authorization": bad}), timeout=5)
            print("[ACS] ERROR: bad credentials accepted", flush=True)
        except urllib.error.HTTPError as e2:
            print(f"[ACS] bad-credential connection request -> HTTP {e2.code}", flush=True)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=9090)
    ap.add_argument("--connreq", help="CPE ConnectionRequestURL to hit ~12s after start")
    a = ap.parse_args()
    if a.connreq:
        threading.Timer(12, connection_request, args=(a.connreq,)).start()
    ThreadingHTTPServer(("127.0.0.1", a.port), H).serve_forever()
