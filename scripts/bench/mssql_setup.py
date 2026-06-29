"""Restore the NHTSA vPIC .bak into the running SQL Server container.

The MSSQL oracle is the original NHTSA reference: the unmodified `spVinDecode`
T-SQL stored procedure shipped in `vPICList_lite_<month>.bak`, restored into
SQL Server (azure-sql-edge on arm64). Waits for the server, reads the backup's
logical file names, and restores with MOVE into the container data dir.

Prereq: container started with the .bak mounted at /bak, e.g.
  docker run -d --name ultravin-mssql -e ACCEPT_EULA=Y \\
    -e MSSQL_SA_PASSWORD=Ultravin!2026 -p 1433:1433 \\
    -v "$PWD/downloads:/bak:ro" mcr.microsoft.com/azure-sql-edge:latest

Usage: python -m scripts.bench.mssql_setup [--bak /bak/VPICList_lite_2026_06.bak]
"""

from __future__ import annotations

import argparse
import time

import pymssql

PASSWORD = "Ultravin!2026"
DB = "vPICList_lite"


def _connect(database: str = "master", tries: int = 60):
    last: Exception | None = None
    for _ in range(tries):
        try:
            return pymssql.connect(
                server="localhost",
                port="1433",
                user="sa",
                password=PASSWORD,
                database=database,
                autocommit=True,
            )
        except pymssql.Error as e:  # server still booting
            last = e
            time.sleep(2)
    msg = f"SQL Server never came up: {last}"
    raise RuntimeError(msg)


def restore(bak: str) -> None:
    conn = _connect("master")
    cur = conn.cursor()
    cur.execute(f"RESTORE FILELISTONLY FROM DISK = '{bak}'")
    files = cur.fetchall()  # (LogicalName, PhysicalName, Type, ...)
    moves = []
    for row in files:
        logical, _physical, ftype = row[0], row[1], row[2]
        ext = "mdf" if ftype.strip() == "D" else "ldf"
        moves.append(f"MOVE '{logical}' TO '/var/opt/mssql/data/{logical}.{ext}'")
    sql = f"RESTORE DATABASE [{DB}] FROM DISK = '{bak}' WITH " + ", ".join(moves) + ", REPLACE, RECOVERY"
    print("restoring (a minute or two for ~11M rows)...")
    cur.execute(sql)
    while cur.nextset():  # drain progress result sets
        pass

    conn2 = _connect(DB)
    c2 = conn2.cursor()
    c2.execute("SELECT COUNT(*) FROM dbo.Pattern")
    (npat,) = c2.fetchone()
    c2.execute("SELECT COUNT(*) FROM sys.procedures WHERE name = 'spVinDecode'")
    (nproc,) = c2.fetchone()
    print(f"restored {DB}: {npat:,} patterns, spVinDecode present={bool(nproc)}")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bak", default="/bak/VPICList_lite_2026_06.bak")
    args = ap.parse_args(argv)
    restore(args.bak)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
