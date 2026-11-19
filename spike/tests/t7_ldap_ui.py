#!/usr/bin/env python3
"""Question 7 — does TAK Server's "Manage LDAP Users" UI work with this OpenLDAP?

The UI (Administrative > Manage LDAP Users) is a web page calling the API
/Marti/api/ldap-user-management/*. This script tests that API, in two steps:

  A. with the spike configuration (read-only service account, under ou=Services);
  B. with a service account placed directly under the base DN and allowed to write, which
     is what TAK Server's API assumes: it takes as directory base everything after the
     first comma of serviceAccountDN. The script edits CoreConfig.xml, restarts TAK Server,
     tests, then restores the original configuration.

Rendering the page itself in a browser is not tested here (see the report).
"""
import subprocess

from taklib import *

ROOT_DN, ROOT_PW = f"cn=admin,{BASE_DN}", ENV["LDAP_ROOT_PW"]
RW_DN = f"cn=takserver-rw,{BASE_DN}"
RO_DN = f"cn=takserver,ou=Services,{BASE_DN}"
CORECONFIG = WORK / "tak" / "CoreConfig.xml"
ACL = f'to dn.subtree="{BASE_DN}" by dn.exact="{RW_DN}" write by * break'
L = "/Marti/api/ldap-user-management"
OK_CODES = (200, 201, 204)
UI_PW = "Spike-Passw0rd-" + "x" * 8


def config_ldapi(ldif: str, check_rc: bool = True) -> str:
    return run(COMPOSE + ["exec", "-T", "openldap", "ldapmodify", "-Q", "-Y", "EXTERNAL", "-H", "ldapi:///"], stdin=ldif, check_rc=check_rc)


def restart_takserver() -> None:
    subprocess.run(COMPOSE + ["restart", "takserver"], check=True, capture_output=True)
    subprocess.run([str(SPIKE / "scripts" / "03-post-start.sh")], check=True, capture_output=True)


def lapi(method: str, path: str, body=None):
    return api(method, L + path, body=body)


print("== A. Spike configuration: read-only service account under ou=Services")
st, compliant = lapi("GET", "/isLdapTakComplient")
print(f"   isLdapTakComplient → {st} {compliant}")
st_u, users = lapi("GET", "/users")
print(f"   GET users → {st_u} {str(users)[:160]}")
check(compliant is False and st_u != 200, "the UI reports itself as not compatible and lists nothing in this configuration")
check(http("GET", 8443, "/Marti/ldap-user-management/index.html", cert=ADMIN)[0] == 200,
      "the page /Marti/ldap-user-management/index.html is served to the administrator")

print("\n== B. Read-write service account placed directly under the base DN")
ldap_modify(f"dn: {RW_DN}\nchangetype: add\nobjectClass: top\nobjectClass: applicationProcess\n"
            f"objectClass: simpleSecurityObject\ncn: takserver-rw\nuserPassword: {ENV['LDAP_TAKSERVER_PW']}\n",
            bind_dn=ROOT_DN, password=ROOT_PW, check_rc=False)
config_ldapi(f"dn: olcDatabase={{1}}mdb,cn=config\nchangetype: modify\nadd: olcAccess\nolcAccess: {{0}}{ACL}\n")
original = CORECONFIG.read_text()
try:
    CORECONFIG.write_text(original.replace(f'serviceAccountDN="{RO_DN}"', f'serviceAccountDN="{RW_DN}"'))
    restart_takserver()

    st, compliant = lapi("GET", "/isLdapTakComplient")
    check(st == 200 and compliant is True, f"isLdapTakComplient → {st} {compliant}")
    st, users = lapi("GET", "/users")
    uids = sorted(u["uid"] for u in users) if st == 200 else users
    check(st == 200 and "alice" in uids, f"GET users lists the existing accounts ({st}) {uids}")
    alice = next((u for u in users if u["uid"] == "alice"), {}) if st == 200 else {}
    check("team-alpha" in str(alice.get("groups")), f"alice's groups are read through memberOf: {alice.get('groups')}")
    st, groups = lapi("GET", "/groups")
    check(st == 200 and "team-alpha" in str(groups), f"GET groups ({st}) {[g.get('groupName') for g in groups] if st == 200 else groups}")

    lapi("DELETE", "/users/uitest")
    st, r = lapi("POST", "/users", {"uid": "uitest", "givenName": "Ui", "surname": "Test", "email": "uitest@example.org",
                                    "password": UI_PW})
    check(st in OK_CODES, f"POST users (creating \"uitest\") → {st} {str(r)[:160]}")
    entry = ldap_search(user_dn("uitest"), attrs=["objectClass", "cn", "sn", "givenName", "mail"], scope="base")
    print("   entry created in the directory:\n      " + entry.strip().replace("\n", "\n      "))
    check(http("GET", 8446, "/Marti/api/tls/config", basic=("uitest", UI_PW))[0] == 200,
          "the account created by the UI can authenticate on the enrollment port")

    st, r = lapi("POST", "/groups/ui-group/users/add", ["uitest"])
    check(st in OK_CODES, f"POST groups/ui-group/users/add (group did not exist, created on the fly) → {st} {str(r)[:160]}")
    entry = ldap_search(group_dn("ui-group"), attrs=["objectClass", "member"], scope="base")
    print("   group created:\n      " + entry.strip().replace("\n", "\n      "))
    mo = ldap_search(user_dn("uitest"), attrs=["memberOf"], scope="base")
    check(group_dn("ui-group") in mo, "the group created by the UI feeds memberOf (so TAK Server sees it)")

    st, r = lapi("POST", "/users/uitest/lock?lock=true")
    locked = ldap_search(user_dn("uitest"), attrs=["pwdAccountLockedTime"], scope="base")
    check(st in OK_CODES and "pwdAccountLockedTime" in locked, f"locking the account → {st}; " + locked.strip().splitlines()[-1])
    check(http("GET", 8446, "/Marti/api/tls/config", basic=("uitest", UI_PW))[0] == 401,
          "locked account: authentication refused")
    lapi("POST", "/users/uitest/lock?lock=false")

    st, r = lapi("POST", "/groups/ui-group/users/remove", ["uitest"])
    left = ldap_search(group_dn("ui-group"), scope="base")
    check(st in OK_CODES and "ui-group" not in left, f"removing the last member → {st}; TAK Server deletes the empty group")
    st, r = lapi("DELETE", "/users/uitest")
    check(st in OK_CODES and "uid: uitest" not in ldap_search(user_dn("uitest"), scope="base"), f"DELETE users/uitest → {st}")
finally:
    print("\n== Restoring the original state")
    CORECONFIG.write_text(original)
    config_ldapi(f"dn: olcDatabase={{1}}mdb,cn=config\nchangetype: modify\ndelete: olcAccess\nolcAccess: {{0}}\n", check_rc=False)
    ldap_modify(f"dn: {RW_DN}\nchangetype: delete\n", bind_dn=ROOT_DN, password=ROOT_PW, check_rc=False)
    ldap_modify(f"dn: {group_dn('ui-group')}\nchangetype: delete\n", check_rc=False)
    ldap_modify(f"dn: {user_dn('uitest')}\nchangetype: delete\n", check_rc=False)
    restart_takserver()
    acl0 = run(COMPOSE + ["exec", "-T", "openldap", "ldapsearch", "-Q", "-LLL", "-o", "ldif-wrap=no", "-Y", "EXTERNAL", "-H", "ldapi:///",
                          "-b", "olcDatabase={1}mdb,cn=config", "-s", "base", "olcAccess"])
    check("takserver-rw" not in acl0, "temporary ACL and account removed, original configuration restored")
finish()
