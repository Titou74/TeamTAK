#!/usr/bin/env python3
"""Question 3 (continued) — removing the LAST group of a connected user.

Phase 0 finding: TAK Server applies a group change as long as the user keeps at least one
group. If the user is left with no group at all, the open session keeps its old groups.
The remedy checked here: cut the session through the API
(DELETE /Marti/api/subscriptions/delete/{subscriptionUid}); after reconnecting, no group.
"""
import time

from taklib import *


def see(a: TakClient, b: TakClient) -> bool:
    t = time.time() - 4
    return bool(a.received_from(b, t)) or bool(b.received_from(a, t))


def tak_groups(callsign: str):
    return next((sorted({g["name"] for g in s["groups"]}) for s in subscriptions() if s["callsign"] == callsign), None)


ldap_ensure_user("frank")
for g in ("team-bravo", "evt-spike"):
    ldap_modify(f"dn: {group_dn(g)}\nchangetype: modify\ndelete: member\nmember: {user_dn('frank')}\n", check_rc=False)
if user_dn("frank") not in ldap_search(group_dn("team-alpha"), attrs=["member"]):
    ldap_member("team-alpha", "frank", "add")

alice, bob, frank = connect_user("alice"), connect_user("bob"), connect_user("frank")
time.sleep(7)
check(see(frank, alice) and not see(frank, bob), f"start: frank is in team-alpha {tak_groups('frank')}")

ldap_member("team-bravo", "frank", "add")
ldap_member("team-alpha", "frank", "delete")
api("GET", "/Marti/api/groups/update/frank")
time.sleep(7)
check(see(frank, bob) and not see(frank, alice), f"team change alpha → bravo applied to the open session {tak_groups('frank')}")

ldap_member("team-bravo", "frank", "delete")
api("GET", "/Marti/api/groups/update/frank")
time.sleep(int(ENV["LDAP_UPDATE_INTERVAL"]) + 8)
still = see(frank, bob)
check(still, "FINDING: removed from his last group, frank still sees team-bravo on his open session "
             f"(groups according to TAK Server: {tak_groups('frank')})")

sub = next(s for s in subscriptions() if s["callsign"] == "frank")
t = time.time()
st, _ = api("DELETE", f"/Marti/api/subscriptions/delete/{sub['subscriptionUid']}")
time.sleep(3)
check(st == 200 and not frank.connected,
      f"remedy: DELETE /Marti/api/subscriptions/delete/{sub['subscriptionUid']} cuts the session "
      f"({(frank.closed_at or t) - t:.1f} s)")

frank2 = connect_user("frank")
time.sleep(7)
check(frank2.connected and not see(frank2, bob) and not see(frank2, alice),
      f"after reconnecting, frank has no group left {tak_groups('frank')} and sees nobody")
for c in (alice, bob, frank, frank2):
    c.close()
finish()
