//! Offline reverse geocoding: turn a photo's GPS coordinates into place tags — the country,
//! administrative region and city it was taken in, and the nearest airport. It reads two embedded,
//! zstd-compressed reference tables (GeoNames populated places plus country and region names;
//! OurAirports IATA airports) and finds the nearest point by great-circle distance. No network,
//! and the reference data never touches a user's index — it only produces tags that attach to
//! *content* (see [`crate::tags`]). See docs/TAGGING.md §4 and data/geo/SOURCES.md for provenance
//! and the CC-BY credit GeoNames requires.

use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::tags::{self, TARGET_CONTENT};

const PLACES_ZST: &[u8] = include_bytes!("../data/geo/places.tsv.zst");
const AIRPORTS_ZST: &[u8] = include_bytes!("../data/geo/airports.tsv.zst");
const COUNTRIES_ZST: &[u8] = include_bytes!("../data/geo/countries.tsv.zst");
const ADMIN1_ZST: &[u8] = include_bytes!("../data/geo/admin1.tsv.zst");

/// Attribution required by the datasets' licences; shown by `backdown tag geo --about`.
pub const CREDIT: &str = "Place, country and region data © GeoNames (geonames.org), CC BY 4.0 \
    <https://creativecommons.org/licenses/by/4.0/>. Airport data from OurAirports \
    (ourairports.com), public domain.";

/// Beyond this great-circle distance a "nearest" airport is not meaningful, so none is tagged.
const AIRPORT_MAX_KM: f64 = 120.0;

/// A populated place from GeoNames.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub cc: String,     // ISO-3166 alpha-2, e.g. "US"
    pub admin1: String, // GeoNames admin1 code, e.g. "FL"
    pub population: u64,
}

/// An airport with an IATA code from OurAirports.
#[derive(Debug, Clone, PartialEq)]
pub struct Airport {
    pub iata: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

/// What a coordinate resolved to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reverse {
    pub cc: Option<String>,
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub place_km: f64,
    pub airport: Option<String>,
    pub airport_name: Option<String>,
    pub airport_km: f64,
}

/// The loaded reference tables.
pub struct GeoRef {
    places: Vec<Place>,
    airports: Vec<Airport>,
    countries: HashMap<String, String>, // "US" -> "United States"
    admin1: HashMap<String, String>,    // "US.FL" -> "Florida"
}

/// Great-circle distance in kilometres (Haversine, mean Earth radius).
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6371.0088;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().clamp(0.0, 1.0).asin()
}

const R_KM: f64 = 6371.0088;

/// Index of the nearest point to `(lat, lon)` and its distance, over anything that yields a
/// coordinate. A latitude-only lower bound (difference in latitude can only *under*-state the
/// great-circle distance) prunes the trig once a close candidate is in hand.
fn nearest<T>(items: &[T], lat: f64, lon: f64, coord: impl Fn(&T) -> (f64, f64)) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, it) in items.iter().enumerate() {
        let (ilat, ilon) = coord(it);
        if let Some((_, bd)) = best {
            let lower = (ilat - lat).abs().to_radians() * R_KM;
            if lower >= bd {
                continue;
            }
        }
        let d = haversine_km(lat, lon, ilat, ilon);
        if best.is_none_or(|(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// Map a distance (km) to a 0..1 confidence: full up to `near`, falling to a 0.4 floor by `far`.
fn confidence(km: f64, near: f64, far: f64) -> f64 {
    if km <= near {
        1.0
    } else if km >= far {
        0.4
    } else {
        1.0 - 0.6 * (km - near) / (far - near)
    }
}

fn unzstd(blob: &[u8]) -> Result<String> {
    let mut dec = ruzstd::decoding::StreamingDecoder::new(blob)
        .map_err(|e| anyhow::anyhow!("geo reference data is not a Zstandard stream: {e}"))?;
    let mut s = String::new();
    dec.read_to_string(&mut s)
        .context("decoding embedded geo reference data")?;
    Ok(s)
}

fn parse_places(s: &str) -> Vec<Place> {
    s.lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let name = it.next()?;
            let lat = it.next()?.parse().ok()?;
            let lon = it.next()?.parse().ok()?;
            let cc = it.next().unwrap_or("");
            let admin1 = it.next().unwrap_or("");
            let population = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
            Some(Place {
                name: name.to_string(),
                lat,
                lon,
                cc: cc.to_string(),
                admin1: admin1.to_string(),
                population,
            })
        })
        .collect()
}

fn parse_airports(s: &str) -> Vec<Airport> {
    s.lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let iata = it.next()?;
            let name = it.next().unwrap_or("");
            let lat = it.next()?.parse().ok()?;
            let lon = it.next()?.parse().ok()?;
            Some(Airport {
                iata: iata.to_string(),
                name: name.to_string(),
                lat,
                lon,
            })
        })
        .collect()
}

fn parse_pairs(s: &str) -> HashMap<String, String> {
    s.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('\t')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

impl GeoRef {
    fn from_blobs(
        places: &[u8],
        airports: &[u8],
        countries: &[u8],
        admin1: &[u8],
    ) -> Result<GeoRef> {
        Ok(GeoRef {
            places: parse_places(&unzstd(places)?),
            airports: parse_airports(&unzstd(airports)?),
            countries: parse_pairs(&unzstd(countries)?),
            admin1: parse_pairs(&unzstd(admin1)?),
        })
    }

    /// Load (once per process) the reference tables embedded in the binary.
    pub fn embedded() -> Result<&'static GeoRef> {
        static GEO: OnceLock<GeoRef> = OnceLock::new();
        if let Some(g) = GEO.get() {
            return Ok(g);
        }
        let g = GeoRef::from_blobs(PLACES_ZST, AIRPORTS_ZST, COUNTRIES_ZST, ADMIN1_ZST)?;
        Ok(GEO.get_or_init(|| g))
    }

    /// How many places and airports are loaded (for `--about`).
    pub fn counts(&self) -> (usize, usize) {
        (self.places.len(), self.airports.len())
    }

    /// The nearest populated place to a coordinate, with its distance in km.
    pub fn nearest_place(&self, lat: f64, lon: f64) -> Option<(&Place, f64)> {
        let i = nearest(&self.places, lat, lon, |p| (p.lat, p.lon))?;
        let p = &self.places[i];
        Some((p, haversine_km(lat, lon, p.lat, p.lon)))
    }

    /// The nearest airport to a coordinate, with its distance in km.
    pub fn nearest_airport(&self, lat: f64, lon: f64) -> Option<(&Airport, f64)> {
        let i = nearest(&self.airports, lat, lon, |a| (a.lat, a.lon))?;
        let a = &self.airports[i];
        Some((a, haversine_km(lat, lon, a.lat, a.lon)))
    }

    /// Resolve a coordinate to country, region, city and nearest airport.
    pub fn reverse(&self, lat: f64, lon: f64) -> Reverse {
        let mut r = Reverse::default();
        if let Some((p, km)) = self.nearest_place(lat, lon) {
            r.place_km = km;
            r.city = Some(p.name.clone());
            if !p.cc.is_empty() {
                r.cc = Some(p.cc.clone());
                r.country = self.countries.get(&p.cc).cloned();
                if !p.admin1.is_empty() {
                    r.region = self.admin1.get(&format!("{}.{}", p.cc, p.admin1)).cloned();
                }
            }
        }
        if let Some((a, km)) = self.nearest_airport(lat, lon)
            && km <= AIRPORT_MAX_KM
        {
            r.airport = Some(a.iata.clone());
            r.airport_name = Some(a.name.clone());
            r.airport_km = km;
        }
        r
    }
}

/// Resolve a content item's coordinate and attach country / region / city / airport tags to it.
/// Returns the labels applied. `source` records where the tags came from (e.g. "geo").
pub fn derive_into(
    conn: &Connection,
    geo: &GeoRef,
    sha256_hex: &str,
    lat: f64,
    lon: f64,
    source: &str,
) -> Result<Vec<String>> {
    let r = geo.reverse(lat, lon);
    let mut applied = Vec::new();
    let mut country_id = None;
    let mut parent = None;

    if let (Some(cc), Some(country)) = (&r.cc, &r.country) {
        let data = format!("{{\"cc\":\"{cc}\"}}");
        let id = tags::upsert_tag(conn, "country", country, None, Some(&data))?;
        tags::attach(
            conn,
            TARGET_CONTENT,
            sha256_hex,
            id,
            source,
            confidence(r.place_km, 10.0, 400.0),
        )?;
        applied.push(format!("country={country}"));
        country_id = Some(id);
        parent = Some(id);
    }
    if let Some(region) = &r.region {
        let id = tags::upsert_tag(conn, "region", region, country_id, None)?;
        tags::attach(
            conn,
            TARGET_CONTENT,
            sha256_hex,
            id,
            source,
            confidence(r.place_km, 15.0, 150.0),
        )?;
        applied.push(format!("region={region}"));
        parent = Some(id);
    }
    if let Some(city) = &r.city {
        let data = format!("{{\"km\":{:.1}}}", r.place_km);
        let id = tags::upsert_tag(conn, "city", city, parent, Some(&data))?;
        tags::attach(
            conn,
            TARGET_CONTENT,
            sha256_hex,
            id,
            source,
            confidence(r.place_km, 15.0, 100.0),
        )?;
        applied.push(format!("city={city}"));
    }
    if let (Some(iata), Some(name)) = (&r.airport, &r.airport_name) {
        let data = format!("{{\"name\":{},\"km\":{:.1}}}", json_str(name), r.airport_km);
        let id = tags::upsert_tag(conn, "airport", iata, None, Some(&data))?;
        tags::attach(
            conn,
            TARGET_CONTENT,
            sha256_hex,
            id,
            source,
            confidence(r.airport_km, 15.0, AIRPORT_MAX_KM),
        )?;
        applied.push(format!("airport={iata}"));
    }
    Ok(applied)
}

/// Every content item carrying a `location` tag, as `(sha256_hex, lat, lon)`. The location tag's
/// value is `"lat,lon"` (written by [`crate::exif`]).
pub fn content_locations(conn: &Connection) -> Result<Vec<(String, f64, f64)>> {
    let mut st = conn.prepare(
        "SELECT g.target_key, t.value
         FROM taggings g JOIN tags t ON t.id = g.tag_id
         WHERE g.target_type = 'content' AND t.kind = 'location'",
    )?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (key, value) = row?;
        if let Some((la, lo)) = value.split_once(',')
            && let (Ok(la), Ok(lo)) = (la.parse(), lo.parse())
        {
            out.push((key, la, lo));
        }
    }
    Ok(out)
}

/// Minimal JSON string escaping for the few fields we embed in a tag's `data`.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic() -> GeoRef {
        GeoRef {
            places: vec![
                Place {
                    name: "Fort Lauderdale".into(),
                    lat: 26.1223,
                    lon: -80.1434,
                    cc: "US".into(),
                    admin1: "FL".into(),
                    population: 183146,
                },
                Place {
                    name: "Hong Kong".into(),
                    lat: 22.2855,
                    lon: 114.1577,
                    cc: "HK".into(),
                    admin1: "".into(),
                    population: 7012738,
                },
            ],
            airports: vec![Airport {
                iata: "FLL".into(),
                name: "Fort Lauderdale Hollywood International Airport".into(),
                lat: 26.0726,
                lon: -80.1527,
            }],
            countries: HashMap::from([
                ("US".into(), "United States".into()),
                ("HK".into(), "Hong Kong".into()),
            ]),
            admin1: HashMap::from([("US.FL".into(), "Florida".into())]),
        }
    }

    #[test]
    fn haversine_known_distance() {
        // Fort Lauderdale downtown to its airport is ~6 km.
        let km = haversine_km(26.1224, -80.1373, 26.0726, -80.1527);
        assert!((5.0..7.5).contains(&km), "{km}");
    }

    #[test]
    fn reverse_resolves_city_region_country_airport() {
        let g = synthetic();
        let r = g.reverse(26.1224, -80.1373); // downtown Fort Lauderdale
        assert_eq!(r.city.as_deref(), Some("Fort Lauderdale"));
        assert_eq!(r.region.as_deref(), Some("Florida"));
        assert_eq!(r.country.as_deref(), Some("United States"));
        assert_eq!(r.cc.as_deref(), Some("US"));
        assert_eq!(r.airport.as_deref(), Some("FLL"));
        assert!(r.airport_km < AIRPORT_MAX_KM);
    }

    #[test]
    fn reverse_far_from_any_airport_tags_none() {
        let g = synthetic();
        let r = g.reverse(22.2855, 114.1577); // Hong Kong; nearest airport (FLL) is a world away
        assert_eq!(r.country.as_deref(), Some("Hong Kong"));
        assert_eq!(r.city.as_deref(), Some("Hong Kong"));
        assert!(r.region.is_none());
        assert!(r.airport.is_none());
    }

    #[test]
    fn derive_attaches_the_expected_kinds() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        c.execute_batch(tags::SCHEMA).unwrap();
        let g = synthetic();
        let applied = derive_into(&c, &g, "abc123", 26.1224, -80.1373, "geo").unwrap();
        assert_eq!(applied.len(), 4, "{applied:?}");
        let got = tags::tags_of(&c, TARGET_CONTENT, "abc123").unwrap();
        let kinds: Vec<_> = got.iter().map(|t| t.tag.kind.as_str()).collect();
        for k in ["country", "region", "city", "airport"] {
            assert!(kinds.contains(&k), "missing {k} in {kinds:?}");
        }
        // city hangs under region, region under country
        let country = got.iter().find(|t| t.tag.kind == "country").unwrap();
        let region = got.iter().find(|t| t.tag.kind == "region").unwrap();
        let city = got.iter().find(|t| t.tag.kind == "city").unwrap();
        assert_eq!(region.tag.parent_id, Some(country.tag.id));
        assert_eq!(city.tag.parent_id, Some(region.tag.id));
    }

    #[test]
    fn content_locations_reads_exif_tags() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        c.execute_batch(tags::SCHEMA).unwrap();
        let loc = tags::upsert_tag(&c, "location", "26.122400,-80.137300", None, None).unwrap();
        tags::attach(&c, TARGET_CONTENT, "abc123", loc, "exif", 1.0).unwrap();
        let got = content_locations(&c).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "abc123");
        assert!((got[0].1 - 26.1224).abs() < 1e-6);
        assert!((got[0].2 + 80.1373).abs() < 1e-6);
    }

    // The real embedded tables decode and resolve well-known coordinates.
    #[test]
    fn embedded_tables_resolve_real_coordinates() {
        let g = GeoRef::embedded().unwrap();
        let (places, airports) = g.counts();
        assert!(places > 100_000, "places={places}");
        assert!(airports > 5_000, "airports={airports}");

        let r = g.reverse(26.1224, -80.1373); // Fort Lauderdale
        assert_eq!(r.cc.as_deref(), Some("US"));
        assert_eq!(r.region.as_deref(), Some("Florida"));
        assert_eq!(r.airport.as_deref(), Some("FLL"));

        let hk = g.reverse(22.2800, 114.1588); // Hong Kong, Central
        assert_eq!(hk.cc.as_deref(), Some("HK"));

        let cdg = g.reverse(49.0097, 2.5479); // Paris CDG apron
        assert_eq!(cdg.cc.as_deref(), Some("FR"));
        assert_eq!(cdg.airport.as_deref(), Some("CDG"));
    }
}
