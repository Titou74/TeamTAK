#!/usr/bin/env python3
"""Question 5 — several simultaneous clients on the same certificate (shared identity).

The account "team-bravo-shared" is enrolled once; three clients (three devices, three
different UIDs and callsigns) connect at the same time with that single certificate.
"""
import time

from taklib import *

cert, key = enroll("team-bravo-shared")
ldap_set_group("evt-spike", [])
bob = connect_user("bob")
devices = [TakClient(f"shared-{i}", cert, key, uid=f"SPIKE-shared-{i}", callsign=f"BRAVO-{i}").connect() for i in (1, 2, 3)]
time.sleep(20)
since = time.time() - 6

print("== Three devices, one certificate")
check(all(d.connected for d in devices), "all three connections are still open after 20 s (none evicts the others)")
check(all(d.received_from(bob, since) for d in devices), "each one receives bob (team-bravo)")
check(all(bob.received_from(d, since) for d in devices), "bob receives all three devices")
check(all(a.received_from(b, since) for a in devices for b in devices if a is not b), "the three devices see each other")
subs = [s for s in subscriptions() if s["username"] == "team-bravo-shared"]
print("   as seen by TAK Server:", [(s["username"], s["callsign"], s["clientUid"], s["subscriptionUid"]) for s in subs])
check(len(subs) == 3 and len({s["clientUid"] for s in subs}) == 3, "TAK Server lists three distinct subscriptions for the same account")

print("\n== Group change: does it reach all three connections?")
interval = int(ENV["LDAP_UPDATE_INTERVAL"])
alice = connect_user("alice")
time.sleep(3)
t = time.time()
ldap_set_group("evt-spike", ["alice", "team-bravo-shared"])
firsts = [alice.wait_first_from(d, t, interval * 2 + 20) for d in devices]
check(all(firsts), "after the shared account is added to evt-spike, alice receives all three devices "
      + str([round(f - t, 1) if f else None for f in firsts]) + " s")

print("\n== Channels: is the state per device or per account?")
c = (cert, key, None)
st, d = api("GET", "/Marti/api/groups/all?useCache=true", cert=c)
groups = d["data"]
t = time.time()
api("PUT", f"/Marti/api/groups/active?clientUid={devices[0].uid}", cert=c,
    body=[dict(g, active=(g["name"] != "evt-spike")) for g in groups])
time.sleep(6)
got = [len(dv.received_from(alice, t + 2)) for dv in devices]
print(f"   device 1 switches evt-spike off → positions of alice then received by devices 1, 2, 3: {got}")
if got[0] == 0 and got[1] > 0 and got[2] > 0:
    print("   → channel state is per device")
elif got == [0, 0, 0]:
    print("   → channel state is SHARED by the account: one device switching a channel off switches it off for the whole team")
check(got[0] == 0, "the channel is off for the device that asked for it")
api("PUT", f"/Marti/api/groups/active?clientUid={devices[0].uid}", cert=c, body=[dict(g, active=True) for g in groups])

print("\n== Two connections with the same certificate AND the same device UID")
twin_a = TakClient("twin-a", cert, key, uid="SPIKE-twin", callsign="TWIN").connect()
time.sleep(3)
twin_b = TakClient("twin-b", cert, key, uid="SPIKE-twin", callsign="TWIN").connect()
time.sleep(10)
print(f"   first connection open: {twin_a.connected} ({twin_a.error}), second: {twin_b.connected} ({twin_b.error})")
check(twin_b.connected, "the second connection works")

print("\n== Revoking the shared certificate: do all connections drop?")
fp = cert_field(cert, "fingerprint").replace(":", "").lower()
cid = next(x["id"] for x in api("GET", "/Marti/api/certadmin/cert?username=team-bravo-shared")[1]["data"]
           if x["hash"].replace(":", "").lower() == fp)
t = time.time()
api("DELETE", f"/Marti/api/certadmin/cert/revoke/{cid}")
time.sleep(5)
everyone = devices + [twin_a, twin_b]
check(not any(d.connected for d in everyone),
      "every connection of the shared certificate is cut " + str([round(d.closed_at - t, 1) if d.closed_at else "open" for d in everyone]) + " s")
check(bob.connected and alice.connected, "bob and alice (other certificates) are not affected")

for x in everyone + [bob, alice]:
    x.close()
ldap_set_group("evt-spike", [])
finish()
