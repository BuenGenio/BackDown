#!/usr/bin/env python3
"""Trim GeoNames + OurAirports raw dumps into compact TSVs for embedding.

Usage: python3 -I trim.py <raw_dir> <out_dir>
Emits: places.tsv, airports.tsv, countries.tsv, admin1.tsv (all tab-separated, no header).
"""
import csv
import sys
from pathlib import Path


def r4(s: str) -> str:
    """Round a coordinate to 4 decimals (~11 m), trimming trailing zeros."""
    return f"{float(s):.4f}".rstrip("0").rstrip(".")


def main() -> int:
    raw = Path(sys.argv[1])
    out = Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)

    # cities1000.txt: name=1 lat=4 lon=5 cc=8 admin1=10 pop=14
    n = 0
    with (raw / "cities1000.txt").open(encoding="utf-8") as f, \
         (out / "places.tsv").open("w", encoding="utf-8") as w:
        for line in f:
            c = line.rstrip("\n").split("\t")
            if len(c) < 15 or not c[4] or not c[5]:
                continue
            name = c[1].replace("\t", " ").strip()
            if not name:
                continue
            pop = c[14] or "0"
            w.write(f"{name}\t{r4(c[4])}\t{r4(c[5])}\t{c[8]}\t{c[10]}\t{pop}\n")
            n += 1
    print(f"places: {n}")

    # airports.csv: type=2 name=3 lat=4 lon=5 iata_code=13 — keep rows with a 3-letter IATA code
    n = 0
    with (raw / "airports.csv").open(encoding="utf-8", newline="") as f, \
         (out / "airports.tsv").open("w", encoding="utf-8") as w:
        reader = csv.reader(f)
        header = next(reader)
        iata_i = header.index("iata_code")
        name_i = header.index("name")
        lat_i = header.index("latitude_deg")
        lon_i = header.index("longitude_deg")
        for row in reader:
            iata = row[iata_i].strip().upper()
            if len(iata) != 3 or not iata.isalpha():
                continue
            if not row[lat_i] or not row[lon_i]:
                continue
            name = row[name_i].replace("\t", " ").strip()
            w.write(f"{iata}\t{name}\t{r4(row[lat_i])}\t{r4(row[lon_i])}\n")
            n += 1
    print(f"airports: {n}")

    # countryInfo.txt: ISO=0 Country=4 (skip # comments)
    n = 0
    with (raw / "countryInfo.txt").open(encoding="utf-8") as f, \
         (out / "countries.tsv").open("w", encoding="utf-8") as w:
        for line in f:
            if line.startswith("#"):
                continue
            c = line.rstrip("\n").split("\t")
            if len(c) < 5 or not c[0] or not c[4]:
                continue
            w.write(f"{c[0]}\t{c[4]}\n")
            n += 1
    print(f"countries: {n}")

    # admin1CodesASCII.txt: code=0 name=1
    n = 0
    with (raw / "admin1CodesASCII.txt").open(encoding="utf-8") as f, \
         (out / "admin1.tsv").open("w", encoding="utf-8") as w:
        for line in f:
            c = line.rstrip("\n").split("\t")
            if len(c) < 2 or not c[0] or not c[1]:
                continue
            w.write(f"{c[0]}\t{c[1]}\n")
            n += 1
    print(f"admin1: {n}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
