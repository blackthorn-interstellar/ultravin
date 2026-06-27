#!/usr/bin/env bash
# Drive the Postgres parity oracle: load the pinned vPIC dump and decode VINs
# against the unmodified vpic.spvindecode. The oracle is the source of truth W2
# diffs ultravin against; it is deterministic (dedup tiebreak ends in id ASC).
set -euo pipefail

cd "$(dirname "$0")/.."
SVC=oracle
POOL="oracle oracle2 oracle3 oracle4 oracle5"

usage() {
  echo "usage: scripts/oracle.sh {up|load <dump>|decode <VIN>|psql [args]|down|pool-up|pool-load <dump>|pool-down}" >&2
  exit 1
}

cmd="${1:-}"; shift || true
case "$cmd" in
  up)
    docker compose up -d --wait "$SVC"
    echo "oracle ready on localhost:55432 (db=vpic, user=postgres)"
    ;;
  load)
    dump="${1:?$(usage)}"
    echo "loading $dump into the oracle (this takes a few minutes for ~11M rows)..."
    case "$dump" in
      *.zip) unzip -p "$dump" ;;
      *)     cat "$dump" ;;
    esac | docker compose exec -T "$SVC" psql -q -U postgres -d vpic
    echo "loaded:"
    docker compose exec -T "$SVC" psql -tA -U postgres -d vpic \
      -c "select count(*) || ' patterns, ' || (select count(*) from vpic.wmi) || ' WMIs' from vpic.pattern;"
    ;;
  decode)
    vin="${1:?$(usage)}"
    docker compose exec -T "$SVC" psql -P pager=off -U postgres -d vpic \
      -c "select variable, value from vpic.spvindecode('$vin') where coalesce(value,'') <> '' order by itemelementid;"
    ;;
  psql)
    docker compose exec -T "$SVC" psql -U postgres -d vpic "$@"
    ;;
  down)
    docker compose down -v
    ;;
  pool-up)
    docker compose up -d --wait $POOL
    echo "oracle pool ready on localhost:55432-55436 (db=vpic, user=postgres)"
    ;;
  pool-load)
    dump="${1:?$(usage)}"
    tmp="$(mktemp -t vpic-dump-XXXXXX.sql)"
    echo "extracting $dump -> $tmp ..."
    case "$dump" in
      *.zip) unzip -p "$dump" > "$tmp" ;;
      *)     cp "$dump" "$tmp" ;;
    esac
    echo "loading all 5 oracles in parallel ..."
    for svc in $POOL; do
      ( docker compose exec -T "$svc" psql -q -U postgres -d vpic < "$tmp" >/dev/null 2>&1 && echo "  loaded $svc" ) &
    done
    wait
    rm -f "$tmp"
    for svc in $POOL; do
      n="$(docker compose exec -T "$svc" psql -tA -U postgres -d vpic -c 'select count(*) from vpic.pattern;' 2>/dev/null | tr -d '[:space:]')"
      echo "  $svc: $n patterns"
    done
    ;;
  pool-down)
    docker compose down -v
    ;;
  *)
    usage
    ;;
esac
