#!/usr/bin/env python3
"""Question 3 — does a group change in the directory reach an already-connected client?

alice (team-alpha) and bob (team-bravo) stay connected and never reconnect. Both are put
into a new group "evt-spike" (as a cross-team event would do), then bob is removed. The
delay between the LDAP write and the effect seen on the stream is measured.

Usage: t3_group_update.py [trials] [--force]
  --force  after each LDAP write, call GET /Marti/api/groups/update/{username} (admin API)
           to ask TAK Server to re-read that user's groups now instead of waiting for
           updateinterval.
"""
import statistics
import sys
import time

from taklib import *

args = [a for a in sys.argv[1:] if not a.startswith("--")]
FORCE = "--force" in sys.argv
trials = int(args[0]) if args else 3
interval = int(ENV["LDAP_UPDATE_INTERVAL"])


def refresh(*users: str) -> None:
    if FORCE:
        for u in users:
            st, _ = api("GET", f"/Marti/api/groups/update/{u}")
            if st != 200:
                print(f"   groups/update/{u} → HTTP {st}")


GROUP = "evt-spike"
print(f"configured updateinterval: {interval} s — {trials} trial(s)"
      + (" — refresh forced through the API after each write" if FORCE else "") + "\n")

ldap_set_group(GROUP, [])                      # starting state: no common group
alice, bob = connect_user("alice"), connect_user("bob")
time.sleep(interval + 8)                       # let one refresh cycle go by

t = time.time() - 5
check(not alice.received_from(bob, t) and not bob.received_from(alice, t),
      "before: alice and bob do not see each other (different teams)")

add_delays, del_delays = [], []
for i in range(1, trials + 1):
    time.sleep(1.7 * i)                        # shift the trial relative to the server's cycle
    t_add = time.time()
    ldap_set_group(GROUP, ["alice", "bob"])
    refresh("alice", "bob")
    first = alice.wait_first_from(bob, t_add, interval * 2 + 30)
    if first is None:
        check(False, f"trial {i}: alice never receives bob after he is added to the group")
        break
    add_delays.append(first - t_add)
    back = bob.wait_first_from(alice, t_add, 15)
    log(f"trial {i}: added   → alice receives bob after {first - t_add:5.1f} s"
        + (f", bob receives alice after {back - t_add:5.1f} s" if back else ", bob does not receive alice"))

    notif = [(round(ts - t_add, 1), ty) for ts, ty, _ in alice.other_types(t_add)]
    if i == 1:
        print(f"           control messages received by alice since the change: {notif or 'none'}")
        for ts, ty, xml in alice.other_types(t_add)[:1]:
            print("           ", xml[:400])

    time.sleep(3)
    t_del = time.time()
    ldap_member(GROUP, "bob", "delete")
    refresh("bob")
    last = alice.wait_silence_from(bob, t_del, interval * 2 + 30)
    if last is None:
        check(False, f"trial {i}: alice still receives bob after his removal")
        break
    del_delays.append(max(0.0, last - t_del))
    log(f"trial {i}: removed → last position of bob received by alice {last - t_del:5.1f} s later")
    ldap_set_group(GROUP, [])
    time.sleep(2)

check(alice.connected and bob.connected, "alice and bob stayed connected throughout (no reconnection)"
      + "".join(f" — {c.name}: {c.error}" for c in (alice, bob) if not c.connected))
if add_delays and del_delays:
    print(f"\nAdd delays:    min {min(add_delays):.1f} s, max {max(add_delays):.1f} s, mean {statistics.mean(add_delays):.1f} s")
    print(f"Remove delays: min {min(del_delays):.1f} s, max {max(del_delays):.1f} s, mean {statistics.mean(del_delays):.1f} s")
    limit = 5 if FORCE else interval + 10
    check(max(add_delays) <= limit, f"addition applied in under {limit} s")
    check(max(del_delays) <= limit, f"removal applied in under {limit} s")

alice.close(); bob.close()
finish()
