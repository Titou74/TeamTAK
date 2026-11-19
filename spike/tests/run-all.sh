#!/usr/bin/env bash
# Replays every phase 0 test against the running stack (docker compose up -d + 03-post-start.sh).
# Full output goes to spike/work/results/. Takes about ten minutes.
# t7 restarts TAK Server twice, so it runs last.
set -uo pipefail
cd "$(dirname "$0")"
OUT=../work/results
mkdir -p "$OUT"
fail=0
run() { # name command...
  local name="$1"; shift
  printf '%-28s ' "$name"
  if "$@" > "$OUT/$name.log" 2>&1; then
    echo "OK    $(tail -1 "$OUT/$name.log")"
  else
    echo "FAIL  $(tail -1 "$OUT/$name.log")  → $OUT/$name.log"; fail=1
  fi
}
run t2_ldap_groups        python3 t2_ldap_groups.py
run t3_group_update       python3 t3_group_update.py 3
run t3_group_update_force python3 t3_group_update.py 3 --force
run t3b_last_group        python3 t3b_last_group.py
run t4_enroll_revoke      python3 t4_enroll_revoke.py
run t5_shared_cert        python3 t5_shared_cert.py
run t6_data_package       python3 t6_data_package.py
run t8_monitoring         python3 t8_monitoring.py
run t7_ldap_ui            python3 t7_ldap_ui.py
exit $fail
