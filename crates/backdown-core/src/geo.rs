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
use serde::Serialize;

use crate::find::fold;
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
    /// place and airport names as search compares them (`find::fold`), worked out on first use
    folded: OnceLock<(Vec<String>, Vec<String>)>,
}

/// Where a search is centred: an airport, a populated place, or a coordinate typed as it is.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Centre {
    /// "airport" | "place" | "point"
    pub kind: &'static str,
    /// how it's shown: "HKG", "Paris", "22.308900, 113.914600"
    pub label: String,
    /// what it is: "Hong Kong International Airport", "Île-de-France, France"
    pub detail: String,
    pub lat: f64,
    pub lon: f64,
}

/// A name to draw on a map.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Label {
    /// "place" | "airport"
    pub kind: &'static str,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub population: u64,
}

/// A coordinate typed as `lat,lon` (or `lat lon`), in range.
pub fn parse_point(s: &str) -> Option<(f64, f64)> {
    let s = s.trim();
    let (a, b) = s
        .split_once(',')
        .or_else(|| s.split_once(char::is_whitespace))?;
    let lat: f64 = a.trim().parse().ok()?;
    let lon: f64 = b.trim().parse().ok()?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
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
            folded: OnceLock::new(),
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

    /// The name of a country by its ISO-3166 code.
    pub fn country(&self, cc: &str) -> Option<&str> {
        self.countries.get(cc).map(String::as_str)
    }

    /// The name of a region by country and GeoNames admin1 code.
    pub fn region(&self, cc: &str, admin1: &str) -> Option<&str> {
        if admin1.is_empty() {
            return None;
        }
        self.admin1
            .get(&format!("{cc}.{admin1}"))
            .map(String::as_str)
    }

    /// An airport by its three-letter IATA code, in either case.
    pub fn airport(&self, code: &str) -> Option<&Airport> {
        let code = code.trim();
        if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_alphabetic()) {
            return None;
        }
        self.airports
            .iter()
            .find(|a| a.iata.eq_ignore_ascii_case(code))
    }

    fn folded(&self) -> &(Vec<String>, Vec<String>) {
        self.folded.get_or_init(|| {
            (
                self.places.iter().map(|p| fold(&p.name)).collect(),
                self.airports.iter().map(|a| fold(&a.name)).collect(),
            )
        })
    }

    /// "Region, Country" for a place, leaving out what repeats its own name (Hong Kong is a city
    /// in the country Hong Kong).
    fn where_of(&self, p: &Place) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if let Some(r) = self.region(&p.cc, &p.admin1)
            && r != p.name
        {
            parts.push(r);
        }
        if let Some(c) = self.country(&p.cc)
            && c != p.name
        {
            parts.push(c);
        }
        parts.join(", ")
    }

    fn place_centre(&self, p: &Place) -> Centre {
        Centre {
            kind: "place",
            label: p.name.clone(),
            detail: self.where_of(p),
            lat: p.lat,
            lon: p.lon,
        }
    }

    fn airport_centre(&self, a: &Airport) -> Centre {
        // airports carry no country of their own: the nearest town's says where it is
        let country = self
            .nearest_place(a.lat, a.lon)
            .and_then(|(p, _)| self.country(&p.cc));
        Centre {
            kind: "airport",
            label: a.iata.clone(),
            detail: match country {
                Some(c) if !a.name.is_empty() => format!("{}, {c}", a.name),
                Some(c) => c.to_string(),
                None => a.name.clone(),
            },
            lat: a.lat,
            lon: a.lon,
        }
    }

    /// What a person typed as the centre of a search: a coordinate (`22.3,113.9`), an airport's
    /// three-letter code (`HKG`), or a place's name, the most populous one spelt that way. A
    /// region or country after a comma picks between places of one name (`Paris, Texas`).
    pub fn resolve(&self, q: &str) -> Option<Centre> {
        let q = q.trim();
        if q.is_empty() {
            return None;
        }
        if let Some((lat, lon)) = parse_point(q) {
            return Some(point_centre(lat, lon));
        }
        if let Some(a) = self.airport(q) {
            return Some(self.airport_centre(a));
        }
        let (name, within) = match q.split_once(',') {
            Some((n, w)) => (fold(n.trim()), Some(fold(w.trim()))),
            None => (fold(q), None),
        };
        let (places, airports) = self.folded();
        let fits = |p: &Place| {
            within.as_deref().is_none_or(|w| {
                w.is_empty()
                    || p.cc.eq_ignore_ascii_case(w)
                    || self.country(&p.cc).is_some_and(|c| fold(c).starts_with(w))
                    || self
                        .region(&p.cc, &p.admin1)
                        .is_some_and(|r| fold(r).starts_with(w))
            })
        };
        let best = self
            .places
            .iter()
            .zip(places)
            .filter(|(p, f)| **f == name && fits(p))
            .max_by_key(|(p, _)| p.population)
            .map(|(p, _)| p);
        if let Some(p) = best {
            return Some(self.place_centre(p));
        }
        // an airport by its whole name ("Heathrow" isn't a town)
        self.airports
            .iter()
            .zip(airports)
            .find(|(_, f)| **f == name || f.split(' ').any(|w| w == name))
            .map(|(a, _)| self.airport_centre(a))
    }

    /// Centres for what's been typed so far: an airport by its code first, then places whose
    /// name starts with it (most populous first) and airports with a word of their name that
    /// does. A coordinate is its own one suggestion.
    pub fn suggest(&self, q: &str, limit: usize) -> Vec<Centre> {
        let q = q.trim();
        if q.is_empty() || limit == 0 {
            return Vec::new();
        }
        if let Some((lat, lon)) = parse_point(q) {
            return vec![point_centre(lat, lon)];
        }
        let f = fold(q);
        let mut out = Vec::new();
        let code = self.airport(q);
        if let Some(a) = code {
            out.push(self.airport_centre(a));
        }
        if f.chars().count() < 2 {
            return out;
        }
        let (places, airports) = self.folded();
        let mut ps: Vec<&Place> = self
            .places
            .iter()
            .zip(places)
            .filter(|(_, n)| n.starts_with(&f))
            .map(|(p, _)| p)
            .collect();
        ps.sort_by(|a, b| b.population.cmp(&a.population).then(a.name.cmp(&b.name)));
        // airports whose name starts that way before those with a later word that does, and
        // international ones first among them
        let mut air: Vec<(&Airport, &String)> = self
            .airports
            .iter()
            .zip(airports)
            .filter(|(a, n)| {
                code.is_none_or(|c| c.iata != a.iata) && n.split(' ').any(|w| w.starts_with(&f))
            })
            .collect();
        air.sort_by_key(|(_, n)| (!n.starts_with(&f), !n.contains("international")));
        let air: Vec<&Airport> = air.into_iter().map(|(a, _)| a).take(3).collect();
        // places first, keeping room for a couple of airports
        let places_room = limit.saturating_sub(out.len() + air.len().min(2));
        let mut seen = std::collections::HashSet::new();
        let mut placed = 0;
        for p in ps {
            if placed >= places_room {
                break;
            }
            // one "Springfield, Illinois" is enough, but different Springfields stay
            if seen.insert((p.name.as_str(), p.cc.as_str(), p.admin1.as_str())) {
                out.push(self.place_centre(p));
                placed += 1;
            }
        }
        for a in air {
            if out.len() >= limit {
                break;
            }
            out.push(self.airport_centre(a));
        }
        out.truncate(limit);
        out
    }

    /// The places (and, zoomed in, the airports) inside a box, most populous first: the names a
    /// map draws. West may be greater than east, for a box across the antimeridian.
    pub fn labels(&self, south: f64, west: f64, north: f64, east: f64, limit: usize) -> Vec<Label> {
        let inside = |lat: f64, lon: f64| {
            lat >= south
                && lat <= north
                && if west <= east {
                    lon >= west && lon <= east
                } else {
                    lon >= west || lon <= east
                }
        };
        let mut ps: Vec<&Place> = self
            .places
            .iter()
            .filter(|p| inside(p.lat, p.lon))
            .collect();
        ps.sort_by_key(|p| std::cmp::Reverse(p.population));
        let mut out: Vec<Label> = ps
            .into_iter()
            .take(limit)
            .map(|p| Label {
                kind: "place",
                name: p.name.clone(),
                lat: p.lat,
                lon: p.lon,
                population: p.population,
            })
            .collect();
        // airports only once the box is a region, not a continent
        if north - south <= 4.0 {
            out.extend(
                self.airports
                    .iter()
                    .filter(|a| inside(a.lat, a.lon))
                    .take(limit / 2)
                    .map(|a| Label {
                        kind: "airport",
                        name: a.iata.clone(),
                        lat: a.lat,
                        lon: a.lon,
                        population: 0,
                    }),
            );
        }
        out
    }

    /// Every place as a dot for a map drawn without going online: per place, latitude and
    /// longitude in hundredths of a degree (two little-endian i16) and a size class (u8: under
    /// 10,000 people, under 100,000, under a million, more).
    pub fn basemap(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.places.len() * 5);
        for p in &self.places {
            let lat = (p.lat * 100.0).round().clamp(-9000.0, 9000.0) as i16;
            let lon = (p.lon * 100.0).round().clamp(-18000.0, 18000.0) as i16;
            let class = match p.population {
                0..10_000 => 0u8,
                10_000..100_000 => 1,
                100_000..1_000_000 => 2,
                _ => 3,
            };
            out.extend_from_slice(&lat.to_le_bytes());
            out.extend_from_slice(&lon.to_le_bytes());
            out.push(class);
        }
        out
    }
}

fn point_centre(lat: f64, lon: f64) -> Centre {
    Centre {
        kind: "point",
        label: format!("{lat:.6}, {lon:.6}"),
        detail: String::new(),
        lat,
        lon,
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
            folded: OnceLock::new(),
        }
    }

    #[test]
    fn points_parse_in_range_only() {
        assert_eq!(parse_point("22.3089,113.9146"), Some((22.3089, 113.9146)));
        assert_eq!(parse_point(" 22.3 , 113.9 "), Some((22.3, 113.9)));
        assert_eq!(parse_point("-33.9 151.2"), Some((-33.9, 151.2)));
        assert_eq!(parse_point("95,10"), None);
        assert_eq!(parse_point("Paris, Texas"), None);
        assert_eq!(parse_point("HKG"), None);
    }

    #[test]
    fn resolve_codes_names_and_points() {
        let g = synthetic();
        let a = g.resolve("fll").unwrap();
        assert_eq!((a.kind, a.label.as_str()), ("airport", "FLL"));
        let p = g.resolve("fort lauderdale").unwrap();
        assert_eq!(
            (p.kind, p.detail.as_str()),
            ("place", "Florida, United States")
        );
        // a qualifier after the comma has to fit
        assert!(g.resolve("Fort Lauderdale, Florida").is_some());
        assert!(g.resolve("Fort Lauderdale, France").is_none());
        let pt = g.resolve("22.3,113.9").unwrap();
        assert_eq!(pt.kind, "point");
        assert!(g.resolve("Atlantis").is_none());
    }

    // The real tables: HKG, Hong Kong, and suggestions as someone types.
    #[test]
    fn embedded_lookup_finds_hkg_and_suggests() {
        let g = GeoRef::embedded().unwrap();
        let hkg = g.resolve("HKG").unwrap();
        assert_eq!(hkg.kind, "airport");
        assert!(
            (hkg.lat - 22.31).abs() < 0.05 && (hkg.lon - 113.92).abs() < 0.05,
            "{hkg:?}"
        );
        assert!(hkg.detail.contains("Hong Kong"), "{hkg:?}");
        let paris = g.resolve("Paris").unwrap();
        assert!(paris.detail.ends_with("France"), "{paris:?}");
        let s = g.suggest("hkg", 8);
        assert_eq!(s[0].label, "HKG");
        let s = g.suggest("hong k", 8);
        assert!(s.iter().any(|c| c.label == "Hong Kong"), "{s:?}");
        let s = g.suggest("hong", 6);
        let first_airport = s.iter().find(|c| c.kind == "airport").unwrap();
        assert_eq!(first_airport.label, "HKG", "{s:?}");
        // a box around Hong Kong has Hong Kong's places in it, the biggest first, and its airport
        let l = g.labels(22.1, 113.8, 22.6, 114.4, 20);
        assert!(
            l.iter().any(|x| x.kind == "airport" && x.name == "HKG"),
            "{l:?}"
        );
        assert!(l[0].population >= l[1].population);
        assert_eq!(g.basemap().len(), g.counts().0 * 5);
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
