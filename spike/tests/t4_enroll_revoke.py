#!/usr/bin/env python3
"""Question 4 — programmatic enrollment, channels, revocation.

1. Enrollment: CSR sent to 8446 with the LDAP username and password.
2. Channels: can the resulting certificate switch a group on and off (groups/active API)?
   Compared with a certificate made by makeCert.sh (without the TAK-specific extended key
   usage), depending on x509useGroupCacheRequiresExtKeyUsage (REQUIRES_EKU in work/.env).
3. Revocation through the admin API, and its effect on the open connection and on later ones.
4. Deleting the LDAP account without revoking the certificate.
"""
import json
import ssl
import subprocess
import time

from taklib import *

REQUIRES_EKU = ENV["REQUIRES_EKU"] == "true"
print(f"x509useGroupCacheRequiresExtKeyUsage = {str(REQUIRES_EKU).lower()}\n")


def channels_work(client: TakClient, peer: TakClient, cert: tuple, group: str) -> tuple[bool, str]:
    """Switches channel `group` off for `client`, checks that traffic from `peer` stops, then switches it back on."""
    st, d = api("GET", "/Marti/api/groups/all?useCache=true", cert=cert)
    groups = d["data"] if st == 200 else []
    listed = sorted({g["name"] for g in groups})
    if group not in listed:
        return False, f"GET groups/all → {st}, groups listed {listed}"
    for g in groups:
        if g["name"] == group:
            g["active"] = False
    t = time.time()
    st_put, _ = api("PUT", f"/Marti/api/groups/active?clientUid={client.uid}", cert=cert, body=groups)
    time.sleep(6)
    still = len(client.received_from(peer, t + 2))
    for g in groups:
        g["active"] = True
    t2 = time.time()
    api("PUT", f"/Marti/api/groups/active?clientUid={client.uid}", cert=cert, body=groups)
    time.sleep(5)
    back = len(client.received_from(peer, t2 + 1))
    ok = st_put == 200 and still == 0 and back > 0
    return ok, f"PUT groups/active → {st_put}; positions of {peer.name} received with the channel off: {still}, back on: {back}"


# ------------------------------------------------------------------ directory setup
for u in ("dave", "erin"):
    ldap_ensure_user(u)
ldap_set_group("evt-spike", ["dave", "erin", "bob"])   # dave and erin: team-alpha + evt-spike
for u in ("dave", "erin"):
    if user_dn(u) not in ldap_search(group_dn("team-alpha"), attrs=["member"]):
        ldap_member("team-alpha", u, "add")

# ------------------------------------------------------------------ 1. enrollment
print("== 1. Programmatic enrollment (port 8446)")
cert, key = enroll("dave")
txt = cert_text(cert)
subject = cert_field(cert, "subject")
print("   subject :", subject)
print("   issuer  :", cert_field(cert, "issuer"))
print("   validity:", cert_field(cert, "startdate"), "→", cert_field(cert, "enddate"))
eku = txt[txt.find("Extended Key Usage"):].splitlines()[1].strip()
print("   extended key usage:", eku)
check("CN=dave" in subject.replace(" ", ""), "the certificate CN is the LDAP username")
check("challengePassword" in eku, "the certificate carries the TAK-specific extended key usage (challengePassword, OID 1.2.840.113549.1.9.7)")
out = run(["openssl", "verify", "-CAfile", str(trust_pem()), str(cert)], check_rc=False)
check(": OK" in out, "the certificate is signed by TAK Server's intermediate CA (chain verified)")
# A CSR whose CN is not the authenticated username: is it accepted?
d2 = CLIENTS / "dave-usurp"; d2.mkdir(exist_ok=True)
run(["openssl", "genrsa", "-out", str(d2 / "k.key"), "2048"])
run(["openssl", "req", "-new", "-key", str(d2 / "k.key"), "-subj", "/O=TAK/OU=TAKManager/CN=alice", "-out", str(d2 / "k.csr")])
csr = "".join(l for l in (d2 / "k.csr").read_text().splitlines() if "-----" not in l)
st, raw = http("POST", 8446, "/Marti/api/tls/signClient/v2?clientUid=x&version=spike", basic=("dave", USER_PW), body=csr,
               headers={"Content-Type": "text/plain", "Accept": "application/json"})
usurp_cn = ""
if st == 200:
    (d2 / "c.pem").write_text("-----BEGIN CERTIFICATE-----\n" + json.loads(raw)["signedCert"].strip() + "\n-----END CERTIFICATE-----\n")
    usurp_cn = cert_field(d2 / "c.pem", "subject")
check(st != 200 or "CN=alice" not in usurp_cn.replace(" ", ""),
      f"dave cannot obtain a certificate in alice's name (CSR CN=alice → HTTP {st}, subject issued: {usurp_cn or 'none'})")

# ------------------------------------------------------------------ 2. channels
print("\n== 2. Channels")
bob = connect_user("bob")
dave = TakClient("dave", cert, key).connect()
time.sleep(2)
# The server remembers channel state per account: start from "everything on".
st, d = api("GET", "/Marti/api/groups/all?useCache=true", cert=(cert, key, None))
api("PUT", f"/Marti/api/groups/active?clientUid={dave.uid}", cert=(cert, key, None), body=[dict(g, active=True) for g in d["data"]])
time.sleep(6)
check(bool(dave.received_from(bob, time.time() - 5)), "dave (enrolled certificate) receives bob through evt-spike")
ok, detail = channels_work(dave, bob, (cert, key, None), "evt-spike")
check(ok, "enrolled certificate: channel evt-spike switched off then on — " + detail)

# "old-style" certificate: makeCert.sh from the ZIP, same CA, no TAK extended key usage
if not (CERTS / "erin.pem").exists():
    subprocess.run(["docker", "run", "--rm", "--entrypoint", "bash", "-e", f"CAPASS={CERT_PW}", "-e", f"PASS={CERT_PW}",
                    "-v", f"{CERTS}:/tak/certs/files", "tak-spike/ca:5.8-RELEASE-103", "-c", "./makeCert.sh client erin"],
                   check=True, capture_output=True)
erin_cert = (CERTS / "erin.pem", CERTS / "erin.key", CERT_PW)
t_e = cert_text(erin_cert[0])
print("   erin, extended key usage:", t_e[t_e.find("Extended Key Usage"):].splitlines()[1].strip())
erin = TakClient("erin", erin_cert[0], erin_cert[1], key_password=CERT_PW).connect()
time.sleep(6)
got = bool(erin.received_from(bob, time.time() - 5))
check(erin.connected and got, "erin (makeCert.sh certificate) connects and receives bob: LDAP groups still apply")
ok_e, detail_e = channels_work(erin, bob, erin_cert, "evt-spike")
if REQUIRES_EKU:
    check(not ok_e, "makeCert.sh certificate + RequiresExtKeyUsage=true: channels CANNOT be toggled — " + detail_e)
else:
    check(ok_e, "makeCert.sh certificate + RequiresExtKeyUsage=false: channels work — " + detail_e)
erin.close()

# ------------------------------------------------------------------ 3. revocation
print("\n== 3. Revocation")
fp = cert_field(cert, "fingerprint")
st, d = api("GET", "/Marti/api/certadmin/cert?username=dave")
mine = [c for c in d["data"] if c["hash"].replace(":", "").lower() == fp.replace(":", "").lower()]
check(len(mine) == 1, f"the enrolled certificate is listed by GET /Marti/api/certadmin/cert?username=dave ({len(d['data'])} certificate(s) for dave)")
cid = mine[0]["id"]
print("   entry:", {k: mine[0][k] for k in ("id", "userDn", "clientUid", "serialNumber", "issuanceDate", "expirationDate", "revocationDate") if k in mine[0]})
check(api("GET", "/Marti/api/version", cert=(cert, key, None))[0] == 200, "before revocation: the certificate opens the 8443 API")

t_rev = time.time()
st, _ = api("DELETE", f"/Marti/api/certadmin/cert/revoke/{cid}")
check(st == 200, f"DELETE /Marti/api/certadmin/cert/revoke/{cid} → {st}")
end = time.time() + 40
while dave.connected and time.time() < end:
    time.sleep(0.2)
if dave.connected:
    last = dave.received_from(bob, t_rev)
    check(False, f"dave's open connection is cut after revocation (still open after 40 s, "
                 f"{len(last)} positions of bob received since)")
else:
    check(True, f"dave's open connection is cut {dave.closed_at - t_rev:.1f} s after revocation ({dave.error})")

# new connection with the revoked certificate
try:
    again = TakClient("dave-revoked", cert, key, uid="SPIKE-dave").connect()
    time.sleep(6)
    usable = again.connected and bool(again.received_from(bob, time.time() - 4))
    check(not usable, "new connection with the revoked certificate: refused or unusable "
                      f"(connected: {again.connected}, reason: {again.error})")
    again.close()
except (ssl.SSLError, OSError) as e:
    check(True, f"new connection with the revoked certificate: refused ({type(e).__name__}: {e})")
try:
    st = api("GET", "/Marti/api/version", cert=(cert, key, None))[0]
except (ssl.SSLError, OSError) as e:
    st = f"{type(e).__name__}"
check(st != 200, f"8443 API with the revoked certificate: refused ({st})")
st, d = api("GET", "/Marti/api/certadmin/cert/revoked")
check(any(c["id"] == cid for c in d["data"]), "the certificate shows up in GET /Marti/api/certadmin/cert/revoked")

# regeneration: enroll again with the same account
cert2, key2 = enroll("dave")
dave2 = TakClient("dave", cert2, key2).connect()
time.sleep(6)
check(dave2.connected and bool(dave2.received_from(bob, time.time() - 4)),
      "regeneration: enrolling the same account again yields a working certificate")

# ------------------------------------------------------------------ 4. account deleted, certificate not revoked
print("\n== 4. Deleting the LDAP account without revoking the certificate")
interval = int(ENV["LDAP_UPDATE_INTERVAL"])
t = time.time()
ldap_modify(f"dn: {user_dn('dave')}\nchangetype: delete\n")
api("GET", "/Marti/api/groups/update/dave")
time.sleep(interval + 8)
still = len(dave2.received_from(bob, time.time() - 4))
check(dave2.connected and still > 0,
      f"FINDING: {interval + 8} s after his LDAP account was deleted, dave's open session still receives bob "
      f"({still} positions in the last 4 s) — deleting the account does not cut a session")
try:
    again = TakClient("dave-again", cert2, key2, uid="SPIKE-dave-2").connect()
    time.sleep(6)
    seen = len(again.received_from(bob, time.time() - 4))
    check(seen == 0, f"a NEW connection with the unrevoked certificate of a deleted account: accepted={again.connected}, "
                     f"but no group, {seen} position received")
    again.close()
except (ssl.SSLError, OSError) as e:
    check(True, f"new connection refused ({e})")
sub = [s for s in subscriptions() if s["clientUid"] == "SPIKE-dave"]
for s in sub:
    api("DELETE", f"/Marti/api/subscriptions/delete/{s['subscriptionUid']}")
time.sleep(3)
check(not dave2.connected, "remedy: cutting the session through the API (subscriptions/delete) closes it")
dave2.close(); bob.close()
ldap_set_group("evt-spike", [])
finish()
