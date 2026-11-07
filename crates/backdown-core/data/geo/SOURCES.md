# Geo reference data

These compressed blobs are embedded in the `backdown` binary (see `src/geo.rs`) and used only to
turn a photo's GPS coordinates into country / city / nearest-airport tags. They are reference data,
never written to a user's index.

## places.tsv.zst, countries.tsv.zst, admin1.tsv.zst — GeoNames

Derived from the [GeoNames](https://www.geonames.org/) geographical database
(`cities1000.txt`, `countryInfo.txt`, `admin1CodesASCII.txt`), trimmed to the columns BackDown
needs (place name, coordinates, country, admin-1 region, population; country and region names).

GeoNames data is licensed under **Creative Commons Attribution 4.0** (CC BY 4.0):
<https://creativecommons.org/licenses/by/4.0/>. This product uses data created by GeoNames
(<https://www.geonames.org/>). Attribution is surfaced to users via `backdown tag geo --about`.

## airports.tsv.zst — OurAirports

Derived from [OurAirports](https://ourairports.com/data/) `airports.csv`, filtered to airports that
carry a 3-letter IATA code (name, IATA code, coordinates). OurAirports data is released into the
**public domain**.

## Regenerating

The raw dumps are trimmed by `scripts/geo/trim.py` (TSV → compact TSV) and compressed with
`zstd -19`. See that script's header for the exact field mapping. Re-run it against fresh dumps to
update these blobs; the format is a header-less, tab-separated text table per file.
