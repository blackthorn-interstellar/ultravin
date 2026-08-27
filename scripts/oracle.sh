#!/usr/bin/env bash
# Drive the Postgres parity oracle: load the pinned vPIC dump and decode VINs
# against vpic.spvindecode. The oracle is the source of truth W2 diffs ultravin
# against; it is deterministic (dedup tiebreak ends in id ASC). The procs load
# unmodified unless ULTRAVIN_ORACLE_FAST_PROCS=1 — see fast_procs() below.
set -euo pipefail

cd "$(dirname "$0")/.."
SVC="${ULTRAVIN_ORACLE_SVC:-oracle}" # oracle2..oracle5 target a pool member (e.g. a fast-procs probe oracle)
POOL="oracle oracle2 oracle3 oracle4 oracle5"

usage() {
  echo "usage: scripts/oracle.sh {up|load <dump>|decode <VIN>|psql [args]|down|pool-up|pool-load <dump>|pool-down}" >&2
  echo "  down       remove just \$ULTRAVIN_ORACLE_SVC (default: oracle)" >&2
  echo "  pool-down  DESTROY ALL FIVE oracles and their data" >&2
  exit 1
}

# ORACLE_TUNING step 5: rewrite the procs' temp-table lifecycle as the dump streams
# into psql. Stock spvindecode creates and drops 9-20 temp tables per decode; each
# CREATE/DROP churns the catalog, WALs it, and changes the table OIDs, which
# invalidates plpgsql's cached plans and forces a replan every single call. Keeping
# the tables and emptying them instead measured +75% single / +124% at 8 connections.
#
# OFF BY DEFAULT, and deliberately so: docs/ACCEPTANCE.md defines the parity oracle
# as the dump's spvindecode run *unmodified*, so the default load stays byte-faithful
# to NHTSA's text and the trust story is unchanged. Opt in for bulk runs, where the
# throughput is worth it:
#
#   ULTRAVIN_ORACLE_FAST_PROCS=1 make oracle-load DUMP=...
#
# Equivalence is measured, not assumed: 0 content mismatches over 5,292 VINs. The
# residual is intra-group row order among tie rows, which docs/ACCEPTANCE.md already
# classes as non-semantic and scripts/parity/normalize.py already excludes.
#
# The transform is two substitutions, applied only to the dump stream. vpic/procs/*.sql
# is NHTSA's committed source text and a CI gate requires vpic-import to reproduce it
# byte-identically, so it is never touched.
fast_procs() {
  if [ "${ULTRAVIN_ORACLE_FAST_PROCS:-0}" != 1 ]; then
    cat
    return
  fi
  # on commit drop -> on commit delete rows: truncate in place at commit (no catalog
  #   write, no new relfilenode, no WAL).
  # drop table X; -> delete from X;: zero catalog churn, and it keeps decodes
  #   independent of each other inside one transaction, which is what makes the
  #   batching in scripts/parity/oracle.py safe.
  sed -e 's/) on commit drop;/) on commit delete rows;/' \
      -e 's/^\([[:space:]]*\)drop table \([A-Za-z0-9_]*\);/\1delete from \2;/'
}

cmd="${1:-}"; shift || true
case "$cmd" in
  up)
    docker compose up -d --wait "$SVC"
    # Ask compose for the binding rather than hardcoding it: $SVC selects any pool
    # member and they are on 55432-55436 (see docker-compose.yml).
    echo "$SVC ready on $(docker compose port "$SVC" 5432) (db=vpic, user=postgres)"
    ;;
  load)
    dump="${1:?$(usage)}"
    echo "loading $dump into the oracle (this takes a few minutes for ~11M rows)..."
    case "$dump" in
      *.zip) unzip -p "$dump" ;;
      *)     cat "$dump" ;;
    esac | fast_procs | docker compose exec -T "$SVC" psql -q -U postgres -d vpic
    echo "loaded:"
    docker compose exec -T "$SVC" psql -tA -U postgres -d vpic \
      -c "select count(*) || ' patterns, ' || (select count(*) from vpic.wmi) || ' WMIs' from vpic.pattern;"
    ;;
  decode)
    vin="${1:?$(usage)}"
    docker compose exec -T "$SVC" psql -P pager=off -U postgres -d vpic \
      --set=vin="$vin" \
      -c "select variable, value from vpic.spvindecode(:'vin') where coalesce(value,'') <> '' order by itemelementid;"
    ;;
  psql)
    docker compose exec -T "$SVC" psql -U postgres -d vpic "$@"
    ;;
  down)
    # Scoped to $SVC, deliberately. This used to be a bare `docker compose down -v`,
    # which tears down all five pool members — so stopping one probe oracle threw
    # away the whole loaded pool and cost a multi-minute reload. `rm -sfv` stops and
    # removes exactly this service's container plus its anonymous volumes, and
    # cannot reach the others. `pool-down` is the way to destroy everything.
    docker compose rm -sfv "$SVC"
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
      *.zip) unzip -p "$dump" ;;
      *)     cat "$dump" ;;
    esac | fast_procs > "$tmp"
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
    # DESTROYS ALL FIVE ORACLES and their data. Reloading the pool is a
    # multi-minute `make oracle-pool-load`. Use `down` for a single service.
    echo "tearing down ALL FIVE oracles and their data (reload: make oracle-pool-load DUMP=...)" >&2
    docker compose down -v
    ;;
  *)
    usage
    ;;
esac
