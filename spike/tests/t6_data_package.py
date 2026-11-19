#!/usr/bin/env python3
"""Question 6 — minimal content of a connection zip (data package) and QR formats.

The zips are produced by the official script of the ZIP, tak/certs/make-connection-package.sh,
run unmodified from the extracted copy. Two variants:
  - enrollment : truststore only, the client enrolls with username + password;
  - certificate: truststore + client certificate (.p12), nothing to type.
The script checks what the zips contain and that the embedded certificate really connects.
Importing them into ATAK and iTAK still has to be done on real phones (see the report).
"""
import re
import shutil
import subprocess
import time
import zipfile
from urllib.parse import quote

from taklib import *

SERVER = "tak.example.org"          # public name of the server (example)
SCRIPT = WORK / "takserver-docker-hardened-5.8-RELEASE-103" / "tak" / "certs" / "make-connection-package.sh"
OUT = WORK / "packages"
shutil.rmtree(OUT, ignore_errors=True)
OUT.mkdir(parents=True)
TRUST = OUT / "truststore.p12"
shutil.copy(CERTS / f"truststore-{INT_CA}.p12", TRUST)
FORBIDDEN = ["locationCallsign", "locationTeam", "atakRoleType", "callsign", "team", "role", "group"]


def make(args: list[str]) -> str:
    return subprocess.run(["bash", str(SCRIPT)] + args, cwd=OUT, check=True, capture_output=True, text=True).stdout


def describe(path) -> dict[str, bytes]:
    with zipfile.ZipFile(path) as z:
        files = {i.filename: z.read(i.filename) for i in z.infolist() if not i.is_dir()}
    print(f"   {path.name} ({path.stat().st_size} bytes):")
    for name, data in files.items():
        print(f"      {name:28s} {len(data):6d} bytes")
    return files


def show_text(files: dict[str, bytes], name: str) -> str:
    txt = files[name].decode()
    print(f"   --- {name}")
    print("      " + re.sub(r'(Password\d?" class="class java.lang.String">)[^<]*', r"\1***", txt).strip().replace("\n", "\n      "))
    return txt


print("== 1. Enrollment zip (truststore only)")
out = make(["-s", SERVER, "-p", "8089", "-t", TRUST.name, "-T", CERT_PW, "-n", "TAK Manager", "-o", "enrollment.zip"])
print("   " + re.sub(r"\(pass: [^)]*\)", "(pass: ***)", out).strip().replace("\n", "\n   "))
f1 = describe(OUT / "enrollment.zip")
pref1 = show_text(f1, "config.pref")
show_text(f1, "MANIFEST/manifest.xml")
check(set(f1) == {"config.pref", "MANIFEST/manifest.xml", TRUST.name}, "three files only: config.pref, manifest.xml, truststore")
check(f"{SERVER}:8089:ssl" in pref1 and "enrollForCertificateWithTrust0" in pref1, "server address and enrollment request present")
keys1 = re.findall(r'entry key="([^"]+)"', pref1)
print("   config.pref keys:", keys1)
check(not [k for k in keys1 if any(w.lower() in k.lower() for w in FORBIDDEN)],
      "no callsign, team colour, role or group key")

print("\n== 2. Zip with a client certificate")
cert, key = enroll("charlie")
p12 = OUT / "charlie.p12"
run(["openssl", "pkcs12", "-export", "-in", str(cert), "-inkey", str(key), "-name", "charlie",
     "-out", str(p12), "-passout", f"pass:{CERT_PW}"])
make(["-s", SERVER, "-p", "8089", "-t", TRUST.name, "-T", CERT_PW, "-c", p12.name, "-C", CERT_PW, "-n", "TAK Manager", "-o", "certificate.zip"])
f2 = describe(OUT / "certificate.zip")
pref2 = show_text(f2, "config.pref")
keys2 = re.findall(r'entry key="([^"]+)"', pref2)
check(set(f2) == {"config.pref", "MANIFEST/manifest.xml", TRUST.name, p12.name}, "four files: the previous three + the client certificate")
check("certificateLocation0" in pref2 and "enrollForCertificateWithTrust0" not in pref2, "certificate referenced, no enrollment requested")
check(not [k for k in keys2 if any(w.lower() in k.lower() for w in FORBIDDEN)],
      "no callsign, team colour, role or group key")

print("\n== 3. Is the content of the zip enough to connect?")
tmp = OUT / "verify"
tmp.mkdir()
(tmp / "client.p12").write_bytes(f2[p12.name])
(tmp / "trust.p12").write_bytes(f2[TRUST.name])
run(["openssl", "pkcs12", "-in", str(tmp / "client.p12"), "-passin", f"pass:{CERT_PW}", "-nodes", "-out", str(tmp / "client.pem")])
run(["openssl", "pkcs12", "-in", str(tmp / "trust.p12"), "-passin", f"pass:{CERT_PW}", "-nokeys", "-out", str(tmp / "trust.pem")])
out = run(["openssl", "verify", "-CAfile", str(tmp / "trust.pem"), str(CERTS / "takserver.pem")], check_rc=False)
check(": OK" in out, "the truststore of the zip validates the server certificate")
alice = connect_user("alice")
c = TakClient("charlie", tmp / "client.pem", tmp / "client.pem").connect()
time.sleep(6)
check(c.connected and bool(c.received_from(alice, time.time() - 4)),
      "the certificate taken from the zip opens an 8089 connection and receives the team (groups come from the directory, not from the zip)")
c.close(); alice.close()
info = run(["openssl", "pkcs12", "-in", str(tmp / "client.p12"), "-passin", f"pass:{CERT_PW}", "-info", "-noout"], check_rc=False)
print("   client .p12 format:", "; ".join(l.strip() for l in info.splitlines() if "PBES2" in l or "MAC:" in l or "PKCS7" in l)[:300])

print("\n== 4. Strings to encode in the QR codes")
user, pw = "alice", "Example-Password"
print("   iTAK  (Quick Connect, 4 fields):", f"TAK Manager,{SERVER},8089,ssl")
print("   ATAK  (enrollment)             :", f"tak://com.atakmap.app/enroll?host={quote(SERVER)}&username={quote(user)}&token={quote(pw)}")
print("   ATAK  (import a zip by URL)    :", "tak://com.atakmap.app/import?url=" + quote(f"https://{SERVER}/dp/abc123.zip", safe=""))
print(f"\nZips kept in {OUT} for trials on a phone.")
finish()
