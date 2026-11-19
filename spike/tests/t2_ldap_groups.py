#!/usr/bin/env python3
"""Question 2 — does the OpenLDAP + CoreConfig.xml configuration work?

Checks, on the running stack:
  - the directory: memberOf computed by the overlay, rights of TAK Server's service account;
  - LDAP password authentication on the enrollment port 8446;
  - TAK groups derived from the certificate CN (auth="x509") and isolation between teams;
  - no __ANON__ group (x509addAnonymous="false");
  - the administrator role (UserAuthenticationFile.xml; adminGroup has no effect).
"""
import time

from taklib import *

TAKSERVER_DN = f"cn=takserver,ou=Services,{BASE_DN}"
TAKSERVER_PW = ENV["LDAP_TAKSERVER_PW"]

print("== Directory")
out = ldap_search(user_dn("alice"), attrs=["memberOf"], bind_dn=TAKSERVER_DN, password=TAKSERVER_PW, scope="base")
print("   " + out.strip().replace("\n", "\n   "))
check(f"memberOf: {group_dn('team-alpha')}" in out, "TAK Server's service account reads alice's memberOf (memberOf overlay on groupOfNames)")
out = ldap_search(user_dn("alice"), attrs=["userPassword"], bind_dn=TAKSERVER_DN, password=TAKSERVER_PW, scope="base")
check("userPassword" not in out, "TAK Server's service account cannot read passwords")
out = ldap_modify(f"dn: {user_dn('alice')}\nchangetype: modify\nreplace: sn\nsn: pirate\n",
                  bind_dn=TAKSERVER_DN, password=TAKSERVER_PW, check_rc=False)
check("Insufficient access" in out, "TAK Server's service account is read-only")
out = ldap_search(f"ou=People,{BASE_DN}", "(uid=bob)", ["uid", "memberOf"], bind_dn=user_dn("alice"), password=USER_PW)
check("uid: bob" not in out, "a TAK user cannot read another user's entry")

print("\n== LDAP authentication on the enrollment port (8446)")
check(http("GET", 8446, "/Marti/api/tls/config")[0] == 401, "no credentials: 401")
check(http("GET", 8446, "/Marti/api/tls/config", basic=("alice", "wrong-password"))[0] == 401, "wrong password: 401")
check(http("GET", 8446, "/Marti/api/tls/config", basic=("nobody", USER_PW))[0] == 401, "user not in the directory: 401")
check(http("GET", 8446, "/Marti/api/tls/config", basic=("alice", USER_PW))[0] == 200, "alice with her LDAP password: 200")

print("\n== Groups from the certificate, and isolation (8089)")
ldap_ensure_user("nogroup")
alice, bob, charlie, nogroup = (connect_user(u) for u in ("alice", "bob", "charlie", "nogroup"))
time.sleep(8)
since = time.time() - 6
check(bool(alice.received_from(charlie, since)) and bool(charlie.received_from(alice, since)),
      "alice and charlie (team-alpha) see each other")
check(not alice.received_from(bob, since) and not bob.received_from(alice, since) and not bob.received_from(charlie, since),
      "bob (team-bravo) does not see team-alpha and is not seen by it")
check(nogroup.connected and not [e for e in nogroup.events if e[1].startswith("SPIKE-")]
      and not any(c.received_from(nogroup) for c in (alice, bob, charlie)),
      "an account with no group connects but sees nobody and is seen by nobody (no __ANON__)")

subs = {s["callsign"]: sorted({g["name"] for g in s["groups"]}) for s in subscriptions()}
print("   groups as seen by TAK Server:", subs)
check(subs.get("alice") == ["team-alpha"] and subs.get("bob") == ["team-bravo"] and subs.get("nogroup") == [],
      "TAK group name = cn of the LDAP group; no __ANON__ group")
a = (CLIENTS / "alice" / "client.pem", CLIENTS / "alice" / "client.key", None)
seen = sorted(s["callsign"] for s in api("GET", "/Marti/api/subscriptions/all", cert=a)[1]["data"])
check(seen == ["alice", "charlie"], f"8443 API with alice's certificate: only lists her own team {seen}")
for c in (alice, bob, charlie, nogroup):
    c.close()

print("\n== Administrator role")
check(api("GET", "/Marti/api/certadmin/cert")[0] == 200,
      "admin.pem declared with `UserManager.jar certmod -A`: admin API allowed")
check(api("GET", "/Marti/api/certadmin/cert", cert=a)[0] == 403, "alice's certificate: admin API refused (403)")
svc = enroll("svc-tak-manager")
status = api("GET", "/Marti/api/certadmin/cert", cert=(svc[0], svc[1], None))[0]
check(status == 403, f"finding: the adminGroup attribute of <ldap> does NOT give the admin role to a certificate "
                     f"(svc-tak-manager, member of tak-admins → {status})")
finish()
