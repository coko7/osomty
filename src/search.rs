//! Place search through OSM Nominatim (or plain "lat, lon" input).

use anyhow::Result;
use serde::Deserialize;

use crate::map::to_world;
use crate::tiles::http_agent;

const NOMINATIM_URL: &str = "https://nominatim.openstreetmap.org/search";

#[derive(Clone, Debug)]
pub struct Place {
    pub name: String,
    pub kind: String,
    pub lat: f64,
    pub lon: f64,
    pub zoom: f64,
}

#[derive(Deserialize)]
struct NominatimHit {
    display_name: String,
    lat: String,
    lon: String,
    #[serde(default)]
    boundingbox: Vec<String>,
    #[serde(default, rename = "type")]
    kind: String,
}

/// Parse "lat, lon" or "lat lon" input.
pub fn parse_coords(q: &str) -> Option<(f64, f64)> {
    let mut it = q
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(str::parse::<f64>);
    let lat = it.next()?.ok()?;
    let lon = it.next()?.ok()?;
    (it.next().is_none() && lat.abs() <= 90.0 && lon.abs() <= 180.0).then_some((lat, lon))
}

pub fn search(query: &str) -> Result<Vec<Place>> {
    if let Some((lat, lon)) = parse_coords(query) {
        return Ok(vec![Place {
            name: format!("{lat:.5}, {lon:.5}"),
            kind: "coordinates".into(),
            lat,
            lon,
            zoom: 16.0,
        }]);
    }
    let hits: Vec<NominatimHit> = http_agent()
        .get(NOMINATIM_URL)
        .query("q", query)
        .query("format", "jsonv2")
        .query("limit", "10")
        .call()?
        .into_json()?;
    Ok(hits
        .into_iter()
        .filter_map(|h| {
            let lat = h.lat.parse().ok()?;
            let lon = h.lon.parse().ok()?;
            Some(Place {
                zoom: zoom_for_bbox(&h.boundingbox),
                name: h.display_name,
                kind: h.kind.replace('_', " "),
                lat,
                lon,
            })
        })
        .collect())
}

/// Zoom level that roughly fits a Nominatim bounding box [south, north, west, east].
fn zoom_for_bbox(bbox: &[String]) -> f64 {
    let v: Vec<f64> = bbox.iter().filter_map(|s| s.parse().ok()).collect();
    let [s, n, w, e] = v[..] else { return 16.0 };
    let (x0, y0) = to_world(n, w);
    let (x1, y1) = to_world(s, e);
    let span = (x1 - x0).abs().max((y1 - y0).abs());
    if span <= 0.0 {
        return 17.5;
    }
    // Aim for the box to cover ~200 braille pixels.
    (200.0 / (span * 256.0)).log2().clamp(2.0, 17.5)
}
