#!/usr/bin/env python3
"""Question 8 — which TAK Server APIs can be used for monitoring?

Queries the running server (port 8443, administrator certificate) and prints what each
useful endpoint returns. "doc" = listed in the OpenAPI description served by the server
itself (GET /v3/api-docs); the /actuator/* endpoints are not listed there.
"""
import json
import time

from taklib import *

st, raw = http("GET", 8443, "/v3/api-docs", cert=ADMIN)
doc = json.loads(raw) if st == 200 else {"paths": {}}
(WORK / "openapi.json").write_bytes(raw)
check(st == 200, f"GET /v3/api-docs → {st}, OpenAPI {doc.get('openapi')}, {len(doc['paths'])} paths (copy: spike/work/openapi.json)")
check(http("GET", 8443, "/swagger-ui/index.html", cert=ADMIN)[0] == 200, "Swagger UI served at /swagger-ui/index.html")

alice, bob = connect_user("alice"), connect_user("bob")
time.sleep(5)


def show(path: str, expect=None, cert=ADMIN) -> object:
    st, r = api("GET", path, cert=cert)
    tag = "doc" if path.split("?")[0] in doc["paths"] else "   "
    txt = json.dumps(r, ensure_ascii=False)
    print(f"  {st} {tag} {path}\n        {txt[:300]}{'…' if len(txt) > 300 else ''}")
    if expect is not None:
        check(st == 200 and expect(r), f"{path} usable")
    return r


print("\n== Version")
show("/Marti/api/version", lambda r: "5.8" in str(r))
show("/Marti/api/version/info", lambda r: r.get("major") == 5 and r.get("minor") == 8)
show("/Marti/api/version/config", lambda r: "version" in r.get("data", {}))

print("\n== Overall state")
show("/actuator/health", lambda r: r.get("status") == "UP")
show("/Marti/api/node/id", lambda r: len(str(r)) > 8)
show("/actuator/takserver-database", lambda r: isinstance(r, dict))
show("/Marti/api/database/cotCount", lambda r: "cotEvents" in r.get("data", {}))
show("/Marti/api/inputs", lambda r: any(i["input"]["port"] == 8089 for i in r["data"]))
show("/Marti/api/security/verifyConfig", lambda r: r.get("data") == "true")

print("\n== Connected clients")
subs = show("/Marti/api/subscriptions/all", lambda r: {"alice", "bob"} <= {s["callsign"] for s in r["data"]})
print("        fields of a subscription:", sorted(subs["data"][0].keys()))
show("/actuator/custom-network-metrics", lambda r: r.get("numClients", 0) >= 2)
show("/Marti/api/clientEndPoints", lambda r: len(r["data"]) >= 2)
show("/Marti/api/contacts/all", lambda r: len(r) >= 2)

print("\n== Load")
show("/actuator/custom-memory-metrics", lambda r: "heapUsed" in r)
show("/actuator/custom-cpu-metrics", lambda r: "cpuUsage" in r)
show("/actuator/custom-queue-metrics", lambda r: "brokerSize" in r)
names = show("/actuator/metrics", lambda r: "jvm.memory.used" in r.get("names", []))
print(f"        {len(names.get('names', []))} metrics, e.g.", ", ".join(n for n in names.get("names", []) if n.startswith(("process.", "system.", "disk."))))
show("/actuator/metrics/process.uptime", lambda r: r["measurements"][0]["value"] > 0)

print("\n== Client certificates")
certs = show("/Marti/api/certadmin/cert/active", lambda r: len(r["data"]) > 0)
print("        fields of a certificate:", sorted(k for k in certs["data"][0].keys()))
show("/Marti/api/certadmin/cert/revoked", lambda r: isinstance(r["data"], list))
show("/Marti/api/certadmin/cert/expired", lambda r: isinstance(r["data"], list))

print("\n== All /actuator endpoints")
links = api("GET", "/actuator")[1].get("_links", {})
print("       ", ", ".join(sorted(links)))

print("\n== Isolation: what does a plain user (alice's certificate) get?")
a = (alice.cert, alice.key, None)
for p in ("/actuator/health", "/actuator/metrics", "/actuator/custom-network-metrics", "/Marti/api/certadmin/cert/active", "/Marti/api/inputs"):
    st, _ = api("GET", p, cert=a)
    print(f"  {st}     {p}")
check(api("GET", "/Marti/api/certadmin/cert/active", cert=a)[0] == 403, "the certificate list is restricted to the administrator")
check(api("GET", "/actuator/health", cert=a)[0] == 403, "/actuator is restricted to the administrator")
alice.close(); bob.close()
finish()
