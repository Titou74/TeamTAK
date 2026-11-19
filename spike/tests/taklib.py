"""Shared helpers for the phase 0 spike tests.

Python standard library only, plus the `openssl` and `docker compose` commands already
on the machine. Nothing gets installed.

- http()        HTTPS calls to TAK Server (8443 with a certificate, 8446 with a password)
- enroll()      client certificate enrollment (CSR on 8446), the way ATAK does it
- TakClient     simulated TAK client (mutual TLS on 8089, sends and receives CoT XML)
- ldap_modify() directory writes with the platform account (cn=tak-manager)
"""
from __future__ import annotations

import base64
import http.client
import json
import os
import re
import socket
import ssl
import subprocess
import sys
import threading
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

SPIKE = Path(__file__).resolve().parents[1]
WORK = SPIKE / "work"
CERTS = WORK / "tak" / "certs"
CLIENTS = WORK / "clients"

HOST = "127.0.0.1"         # ports published by docker compose
SERVER_NAME = "takserver"  # name carried by the server certificate (verified)


def load_env() -> dict[str, str]:
    env = {}
    for line in (WORK / ".env").read_text().splitlines():
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            env[k] = v
    return env


ENV = load_env()
BASE_DN = ENV["LDAP_BASE_DN"]
USER_PW = ENV["TAK_USER_PW"]
CERT_PW = ENV["TAK_CERT_PASSWORD"]
INT_CA = ENV["INT_CA_NAME"]

T0 = time.time()
RESULTS: list[tuple[bool, str]] = []


def log(msg: str) -> None:
    print(f"[{time.time() - T0:7.2f}s] {msg}", flush=True)


def check(ok: bool, label: str) -> bool:
    print(f"  {'OK  ' if ok else 'FAIL'} {label}", flush=True)
    RESULTS.append((ok, label))
    return ok


def finish() -> None:
    failed = [label for ok, label in RESULTS if not ok]
    print(f"\n{len(RESULTS) - len(failed)}/{len(RESULTS)} checks passed")
    sys.exit(1 if failed else 0)


def run(cmd: list[str], stdin: str | None = None, check_rc: bool = True) -> str:
    p = subprocess.run(cmd, input=stdin, capture_output=True, text=True)
    if check_rc and p.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd[:4])}… → {p.returncode}\n{p.stdout}\n{p.stderr}")
    return p.stdout + p.stderr


# --------------------------------------------------------------------------- TLS / HTTP

def trust_pem() -> Path:
    """Truststore handed to clients (root + intermediate), as PEM."""
    out = CLIENTS / "trust.pem"
    if not out.exists():
        CLIENTS.mkdir(parents=True, exist_ok=True)
        run(["openssl", "pkcs12", "-in", str(CERTS / f"truststore-{INT_CA}.p12"), "-nokeys",
             "-passin", f"pass:{CERT_PW}", "-out", str(out)])
    return out


def ssl_context(cert: Path | None = None, key: Path | None = None, key_password: str | None = None) -> ssl.SSLContext:
    ctx = ssl.create_default_context(cafile=str(trust_pem()))
    if cert:
        ctx.load_cert_chain(str(cert), str(key) if key else None, password=key_password)
    return ctx


class _Conn(http.client.HTTPSConnection):
    """Connects to 127.0.0.1 but verifies the certificate for the name "takserver"."""

    def connect(self) -> None:
        sock = socket.create_connection((HOST, self.port), self.timeout)
        self.sock = self._context.wrap_socket(sock, server_hostname=SERVER_NAME)


ADMIN = (CERTS / "admin.pem", CERTS / "admin.key", CERT_PW)


def http(method: str, port: int, path: str, *, cert: tuple | None = None, basic: tuple[str, str] | None = None,
         body: bytes | str | None = None, headers: dict | None = None, timeout: float = 30) -> tuple[int, bytes]:
    ctx = ssl_context(*cert) if cert else ssl_context()
    conn = _Conn(SERVER_NAME, port, context=ctx, timeout=timeout)
    h = dict(headers or {})
    if basic:
        h["Authorization"] = "Basic " + base64.b64encode(f"{basic[0]}:{basic[1]}".encode()).decode()
    if isinstance(body, str):
        body = body.encode()
    try:
        conn.request(method, path, body=body, headers=h)
        r = conn.getresponse()
        return r.status, r.read()
    finally:
        conn.close()


def api(method: str, path: str, *, cert: tuple = ADMIN, body=None, headers=None) -> tuple[int, object]:
    """Calls the 8443 API with a client certificate; returns (status, JSON or text)."""
    h = {"Accept": "application/json"}
    if body is not None and not isinstance(body, (bytes, str)):
        body = json.dumps(body)
        h["Content-Type"] = "application/json"
    h.update(headers or {})
    status, raw = http(method, 8443, path, cert=cert, body=body, headers=h)
    try:
        return status, json.loads(raw)
    except ValueError:
        return status, raw.decode(errors="replace")


# --------------------------------------------------------------------------- enrollment

def enroll(username: str, password: str = USER_PW, *, name: str | None = None, client_uid: str | None = None) -> tuple[Path, Path]:
    """Enrolls a client certificate the way ATAK does. Returns (cert.pem, key.pem).

    1. GET  /Marti/api/tls/config          (HTTP Basic) → RDNs required by the server (O, OU)
    2. local RSA key and CSR whose CN is the username
    3. POST /Marti/api/tls/signClient/v2   (HTTP Basic) → signed certificate + chain
    """
    name = name or username
    d = CLIENTS / name
    d.mkdir(parents=True, exist_ok=True)
    key, csr, cert = d / "client.key", d / "client.csr", d / "client.pem"

    status, cfg = http("GET", 8446, "/Marti/api/tls/config", basic=(username, password))
    if status != 200:
        raise RuntimeError(f"tls/config → HTTP {status}")
    subj = "".join(f"/{n}={v}" for n, v in re.findall(r'nameEntry name="([^"]+)" value="([^"]+)"', cfg.decode()))
    subj += f"/CN={username}"

    run(["openssl", "genrsa", "-out", str(key), "2048"])
    run(["openssl", "req", "-new", "-key", str(key), "-subj", subj, "-out", str(csr)])
    csr_b64 = "".join(l for l in csr.read_text().splitlines() if "-----" not in l)

    uid = client_uid or f"spike-{name}"
    status, raw = http("POST", 8446, f"/Marti/api/tls/signClient/v2?clientUid={uid}&version=spike",
                       basic=(username, password), body=csr_b64,
                       headers={"Content-Type": "text/plain", "Accept": "application/json"})
    if status != 200:
        raise RuntimeError(f"signClient/v2 → HTTP {status} {raw[:200]!r}")
    signed = json.loads(raw)
    pem = lambda b64: "-----BEGIN CERTIFICATE-----\n" + b64.strip() + "\n-----END CERTIFICATE-----\n"
    chain = [pem(signed["signedCert"])] + [pem(v) for k, v in sorted(signed.items()) if k.startswith("ca")]
    cert.write_text("".join(chain))
    os.chmod(key, 0o600)
    return cert, key


def cert_text(cert: Path) -> str:
    return run(["openssl", "x509", "-in", str(cert), "-noout", "-text"])


def cert_field(cert: Path, field: str) -> str:
    """field: serial, fingerprint, subject, enddate…"""
    out = run(["openssl", "x509", "-in", str(cert), "-noout", f"-{field}"] + (["-sha256"] if field == "fingerprint" else []))
    return out.strip().split("=", 1)[-1]


# --------------------------------------------------------------------------- simulated TAK client

def _iso(dt: datetime) -> str:
    return dt.strftime("%Y-%m-%dT%H:%M:%S.") + f"{dt.microsecond // 1000:03d}Z"


class TakClient:
    """Minimal TAK client: mutual TLS on 8089, one position per second, listens for CoT.

    It does not negotiate the protobuf protocol, so the server keeps talking CoT XML (one
    <event> after another), as it does with ATAK until negotiation happens.
    """

    def __init__(self, name: str, cert: Path, key: Path, *, key_password: str | None = None,
                 uid: str | None = None, callsign: str | None = None, interval: float = 1.0):
        self.name = name
        self.uid = uid or f"SPIKE-{name}"
        self.callsign = callsign or name
        self.interval = interval
        self.cert, self.key, self.key_password = cert, key, key_password
        self.events: list[tuple[float, str, str, str]] = []  # (time, uid, type, xml)
        self.closed_at: float | None = None
        self.error: str | None = None
        self.sent = 0
        self._stop = threading.Event()
        self._lock = threading.Lock()
        self.sock: ssl.SSLSocket | None = None

    # -- connection
    def connect(self, timeout: float = 10) -> "TakClient":
        raw = socket.create_connection((HOST, 8089), timeout)
        ctx = ssl_context(self.cert, self.key, self.key_password)
        self.sock = ctx.wrap_socket(raw, server_hostname=SERVER_NAME)
        threading.Thread(target=self._loop, daemon=True).start()
        return self

    def sa_event(self) -> str:
        now = datetime.now(timezone.utc)
        return (f'<event version="2.0" uid="{self.uid}" type="a-f-G-U-C" how="m-g" time="{_iso(now)}" '
                f'start="{_iso(now)}" stale="{_iso(now + timedelta(seconds=30))}">'
                f'<point lat="45.9" lon="6.1" hae="450" ce="10" le="10"/>'
                f'<detail><contact callsign="{self.callsign}" endpoint="*:-1:stcp"/>'
                f'<__group name="Cyan" role="Team Member"/>'
                f'<takv device="spike" platform="spike-sim" os="0" version="0"/>'
                f'<uid Droid="{self.callsign}"/></detail></event>')

    def _loop(self) -> None:
        """One thread per client: OpenSSL does not allow a read and a write at the same
        time on the same connection."""
        buf = b""
        next_send = 0.0
        while not self._stop.is_set():
            if time.time() >= next_send:
                try:
                    self.sock.settimeout(5.0)
                    self.sock.sendall(self.sa_event().encode())
                    self.sent += 1
                except (OSError, ssl.SSLError) as e:
                    self._closed(f"write: {type(e).__name__} {e}")
                    return
                next_send = time.time() + self.interval
            try:
                self.sock.settimeout(0.2)
                chunk = self.sock.recv(65536)
            except (TimeoutError, socket.timeout, ssl.SSLWantReadError):
                continue
            except (OSError, ssl.SSLError) as e:
                if "timed out" in str(e):
                    continue
                self._closed(f"read: {type(e).__name__} {e}")
                return
            if not chunk:
                self._closed("connection closed by the server")
                return
            buf += chunk
            while b"</event>" in buf:
                one, buf = buf.split(b"</event>", 1)
                xml = one.decode(errors="replace") + "</event>"
                xml = xml[xml.find("<event"):] if "<event" in xml else xml
                m_uid = re.search(r'<event[^>]*\buid="([^"]*)"', xml)
                m_type = re.search(r'<event[^>]*\btype="([^"]*)"', xml)
                with self._lock:
                    self.events.append((time.time(), m_uid.group(1) if m_uid else "?",
                                        m_type.group(1) if m_type else "?", xml))

    def _closed(self, why: str) -> None:
        if self.closed_at is None:
            self.closed_at, self.error = time.time(), why

    @property
    def connected(self) -> bool:
        return self.closed_at is None and self.sock is not None

    def close(self) -> None:
        self._stop.set()
        try:
            if self.sock:
                self.sock.close()
        except OSError:
            pass

    # -- observation
    def received_from(self, other: "TakClient | str", since: float = 0.0) -> list[float]:
        uid = other if isinstance(other, str) else other.uid
        with self._lock:
            return [t for t, u, _, _ in self.events if u == uid and t >= since]

    def other_types(self, since: float = 0.0) -> list[tuple[float, str, str]]:
        """Messages received that are not positions of other simulated clients."""
        with self._lock:
            return [(t, ty, xml) for t, u, ty, xml in self.events if t >= since and not u.startswith("SPIKE-")]

    def wait_first_from(self, other: "TakClient", since: float, timeout: float) -> float | None:
        end = time.time() + timeout
        while time.time() < end:
            got = self.received_from(other, since)
            if got:
                return got[0]
            time.sleep(0.2)
        return None

    def wait_silence_from(self, other: "TakClient", since: float, timeout: float, quiet: float = 4.0) -> float | None:
        """Waits until positions of `other` stop arriving; returns the time of the last one."""
        end = time.time() + timeout
        while time.time() < end:
            got = self.received_from(other, since)
            last = got[-1] if got else since
            if time.time() - last >= quiet:
                return last
            time.sleep(0.2)
        return None


def connect_user(username: str, *, name: str | None = None, uid: str | None = None, callsign: str | None = None,
                 reuse: bool = True) -> TakClient:
    """Enrolls `username` if needed, then opens a TAK connection with its certificate."""
    name = name or username
    d = CLIENTS / username
    cert, key = d / "client.pem", d / "client.key"
    if not (reuse and cert.exists() and key.exists()):
        cert, key = enroll(username)
    return TakClient(name, cert, key, uid=uid, callsign=callsign).connect()


# --------------------------------------------------------------------------- directory

MANAGER_DN = f"cn=tak-manager,ou=Services,{BASE_DN}"
COMPOSE = ["docker", "compose", "--project-directory", str(SPIKE)]


def ldap_modify(ldif: str, *, bind_dn: str = MANAGER_DN, password: str | None = None, check_rc: bool = True) -> str:
    """Applies an LDIF with ldapmodify, inside the openldap container, as the platform account."""
    password = password or ENV["LDAP_MANAGER_PW"]
    return run(COMPOSE + ["exec", "-T", "openldap", "ldapmodify", "-x", "-H", "ldap://localhost:389",
                          "-D", bind_dn, "-w", password], stdin=ldif, check_rc=check_rc)


def ldap_search(base: str, flt: str = "(objectClass=*)", attrs: list[str] | None = None, *,
                bind_dn: str = MANAGER_DN, password: str | None = None, scope: str = "sub") -> str:
    password = password or ENV["LDAP_MANAGER_PW"]
    return run(COMPOSE + ["exec", "-T", "openldap", "ldapsearch", "-LLL", "-x", "-o", "ldif-wrap=no",
                          "-H", "ldap://localhost:389", "-D", bind_dn, "-w", password,
                          "-b", base, "-s", scope, flt] + (attrs or []), check_rc=False)


def user_dn(uid: str) -> str:
    return f"uid={uid},ou=People,{BASE_DN}"


def group_dn(cn: str) -> str:
    return f"cn={cn},ou=Group,{BASE_DN}"


def ldap_set_group(cn: str, members: list[str]) -> None:
    """Creates, replaces or (empty list) deletes a TAK group. Idempotent."""
    ldap_modify(f"dn: {group_dn(cn)}\nchangetype: delete\n", check_rc=False)
    if members:
        ldap_modify(f"dn: {group_dn(cn)}\nchangetype: add\nobjectClass: top\nobjectClass: groupOfNames\ncn: {cn}\n"
                    + "".join(f"member: {user_dn(m)}\n" for m in members))


def ldap_member(cn: str, uid: str, op: str) -> None:
    """op = add | delete: adds a member to, or removes one from, an existing group."""
    ldap_modify(f"dn: {group_dn(cn)}\nchangetype: modify\n{op}: member\nmember: {user_dn(uid)}\n")


def ldap_ensure_user(uid: str, password: str = USER_PW) -> None:
    ldap_modify(f"dn: {user_dn(uid)}\nchangetype: add\nobjectClass: top\nobjectClass: person\n"
                f"objectClass: organizationalPerson\nobjectClass: inetOrgPerson\nuid: {uid}\ncn: {uid}\nsn: {uid}\n"
                f"userPassword: {password}\n", check_rc=False)


def subscriptions() -> list[dict]:
    """Connected clients as seen by TAK Server (admin API)."""
    status, data = api("GET", "/Marti/api/subscriptions/all")
    return data.get("data", []) if isinstance(data, dict) else []
