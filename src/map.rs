//! Web-mercator view math and OpenMapTiles styling.

use std::collections::HashSet;
use std::f64::consts::PI;
use std::sync::Arc;

use ratatui::style::{Color, Modifier, Style, Stylize};

use crate::canvas::{Canvas, Clip};
use crate::mvt::{Feature, GeomType, Tile};
use crate::tiles::{MAX_ZOOM, TileId, TileManager};

pub const MIN_ZOOM: f64 = 0.0;
pub const MAX_VIEW_ZOOM: f64 = 19.5;
const TILE_PX: f64 = 256.0;
const MAX_LAT: f64 = 85.051_128_78;

const WATER: Color = Color::Rgb(16, 36, 72);
const WATER_LINE: Color = Color::Rgb(70, 120, 210);
const PARK: Color = Color::Rgb(22, 46, 26);
const WOOD: Color = Color::Rgb(20, 40, 24);
const BUILDING: Color = Color::Rgb(46, 46, 54);
const BOUNDARY_COUNTRY: Color = Color::Rgb(200, 120, 200);
const BOUNDARY_STATE: Color = Color::Rgb(120, 85, 120);

/// Map position: centre in normalised mercator coordinates ([0,1) both axes) plus zoom.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
}

impl View {
    pub fn new(lat: f64, lon: f64, zoom: f64) -> Self {
        let (x, y) = to_world(lat, lon);
        View {
            x,
            y,
            zoom: zoom.clamp(MIN_ZOOM, MAX_VIEW_ZOOM),
        }
    }

    pub fn lat_lon(&self) -> (f64, f64) {
        to_lat_lon(self.x, self.y)
    }

    /// Size of the whole world in braille pixels at the current zoom.
    pub fn world_px(&self) -> f64 {
        TILE_PX * self.zoom.exp2()
    }

    pub fn pan_px(&mut self, dx: f64, dy: f64) {
        let ws = self.world_px();
        self.x = (self.x + dx / ws).rem_euclid(1.0);
        self.y = (self.y + dy / ws).clamp(0.0, 1.0);
    }

    /// Zoom by `delta` keeping the pixel at (`px`,`py`) (relative to the view centre) fixed.
    pub fn zoom_around(&mut self, delta: f64, px: f64, py: f64) {
        let before = self.world_px();
        self.zoom = (self.zoom + delta).clamp(MIN_ZOOM, MAX_VIEW_ZOOM);
        let after = self.world_px();
        self.x = (self.x + px / before - px / after).rem_euclid(1.0);
        self.y = (self.y + py / before - py / after).clamp(0.0, 1.0);
    }

    /// Screen pixel position of a world point, relative to the top-left of a w×h canvas.
    pub fn screen_pos(&self, wx: f64, wy: f64, w: f64, h: f64) -> (f64, f64) {
        let dx = (wx - self.x + 0.5).rem_euclid(1.0) - 0.5;
        let ws = self.world_px();
        (w / 2.0 + dx * ws, h / 2.0 + (wy - self.y) * ws)
    }
}

pub fn to_world(lat: f64, lon: f64) -> (f64, f64) {
    let lat = lat.clamp(-MAX_LAT, MAX_LAT).to_radians();
    let x = (lon + 180.0) / 360.0;
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / PI) / 2.0;
    (x.rem_euclid(1.0), y)
}

pub fn to_lat_lon(x: f64, y: f64) -> (f64, f64) {
    let lon = x * 360.0 - 180.0;
    let lat = (PI * (1.0 - 2.0 * y)).sinh().atan().to_degrees();
    (lat, lon)
}

/// Trade-off between zooming and panning in a flight; √2 is the value van Wijk & Nuij
/// found most natural (also the d3 and Mapbox default).
const RHO: f64 = std::f64::consts::SQRT_2;
/// Seconds of animation per unit of path length, and the allowed duration range.
const FLIGHT_PACE: f64 = 0.25;
const FLIGHT_MIN_SECS: f64 = 0.6;
const FLIGHT_MAX_SECS: f64 = 3.0;

/// Animated "fly to" between two views: zoom out while travelling, zoom back in to arrive.
/// J. J. van Wijk & W. A. A. Nuij, "Smooth and efficient zooming and panning", 2003.
pub struct Flight {
    from: View,
    to: View,
    /// Pan offset, taking the short way around the antimeridian.
    dx: f64,
    dy: f64,
    /// Pan distance and the start/end viewport widths, in world units.
    dist: f64,
    w0: f64,
    r0: f64,
    /// Total path length.
    length: f64,
    viewport_px: f64,
    elapsed: f64,
    duration: f64,
}

impl Flight {
    /// `viewport_px` is the map width in braille pixels.
    pub fn new(from: View, to: View, viewport_px: f64) -> Self {
        let width = |zoom: f64| viewport_px / (TILE_PX * zoom.exp2());
        let (w0, w1) = (width(from.zoom), width(to.zoom));
        let dx = (to.x - from.x + 0.5).rem_euclid(1.0) - 0.5;
        let dy = to.y - from.y;
        let dist = dx.hypot(dy);
        let rho2 = RHO * RHO;
        let (r0, length) = if dist < 1e-12 {
            (0.0, (w1 / w0).ln() / RHO)
        } else {
            let b0 = (w1 * w1 - w0 * w0 + rho2 * rho2 * dist * dist) / (2.0 * w0 * rho2 * dist);
            let b1 = (w1 * w1 - w0 * w0 - rho2 * rho2 * dist * dist) / (2.0 * w1 * rho2 * dist);
            // r = ln(√(b²+1) − b) = −asinh(b), written to avoid cancellation for large b.
            let (r0, r1) = (-b0.asinh(), -b1.asinh());
            (r0, (r1 - r0) / RHO)
        };
        Flight {
            from,
            to,
            dx,
            dy,
            dist,
            w0,
            r0,
            length,
            viewport_px,
            elapsed: 0.0,
            duration: (length.abs() * FLIGHT_PACE).clamp(FLIGHT_MIN_SECS, FLIGHT_MAX_SECS),
        }
    }

    /// Advance by `dt` seconds. Returns the view at that time and whether the flight is over.
    pub fn step(&mut self, dt: f64) -> (View, bool) {
        self.elapsed += dt;
        let t = self.elapsed / self.duration;
        if t >= 1.0 {
            return (self.to, true);
        }
        let t = ease_in_out(t);
        let s = t * self.length;
        // u: fraction of the pan done, w: viewport width in world units.
        let (u, w) = if self.dist < 1e-12 {
            (t, self.w0 * (RHO * s).exp())
        } else {
            let (r0, rs) = (self.r0, RHO * s + self.r0);
            let u = self.w0 / (RHO * RHO * self.dist) * (r0.cosh() * rs.tanh() - r0.sinh());
            (u, self.w0 * r0.cosh() / rs.cosh())
        };
        let zoom = (self.viewport_px / (TILE_PX * w)).log2();
        let view = View {
            x: (self.from.x + u * self.dx).rem_euclid(1.0),
            y: (self.from.y + u * self.dy).clamp(0.0, 1.0),
            zoom: zoom.clamp(MIN_ZOOM, MAX_VIEW_ZOOM),
        };
        (view, false)
    }
}

fn ease_in_out(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (2.0 - 2.0 * t).powi(3) / 2.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Food,
    Drink,
    Shop,
    Health,
    Money,
    Lodging,
    Transport,
    Culture,
    Education,
    Worship,
    Other,
}

impl Category {
    pub const ALL: [Category; 11] = [
        Category::Food,
        Category::Drink,
        Category::Shop,
        Category::Health,
        Category::Money,
        Category::Lodging,
        Category::Transport,
        Category::Culture,
        Category::Education,
        Category::Worship,
        Category::Other,
    ];

    /// Classify an OpenMapTiles `poi` feature from its `class`/`subclass`.
    pub fn of(class: &str, subclass: &str) -> Category {
        let pick = |c: &str| match c {
            "restaurant" | "fast_food" | "food_court" | "bakery" | "ice_cream" | "pastry"
            | "confectionery" => Some(Category::Food),
            "cafe" | "bar" | "pub" | "beer" | "biergarten" | "nightclub" | "wine" => {
                Some(Category::Drink)
            }
            "shop" | "grocery" | "supermarket" | "convenience" | "clothing_store" | "clothes"
            | "alcohol_shop" | "butcher" | "jewelry" | "furniture" | "hardware" | "florist"
            | "gift" | "mobile_phone" | "optician" | "shoe" | "shoes" | "sports" | "stationery"
            | "toy" | "toys" | "books" | "hairdresser" | "beauty" | "laundry" | "electronics"
            | "deli" | "greengrocer" | "cosmetics" | "department_store" | "mall"
            | "marketplace" | "music" | "art" | "chemist" | "kiosk" | "tobacco" | "newsagent"
            | "bicycle" | "car" | "car_repair" | "pet" | "computer" | "variety_store"
            | "boutique" | "perfumery" | "tea" | "coffee" | "cheese" => Some(Category::Shop),
            "hospital" | "pharmacy" | "dentist" | "doctors" | "doctor" | "clinic"
            | "veterinary" => Some(Category::Health),
            "bank" | "atm" | "bureau_de_change" | "money_transfer" => Some(Category::Money),
            "lodging" | "hotel" | "hostel" | "motel" | "guest_house" | "campsite" | "apartment" => {
                Some(Category::Lodging)
            }
            "bus" | "railway" | "subway" | "tram" | "tram_stop" | "ferry_terminal"
            | "aerialway" | "parking" | "fuel" | "bicycle_rental" | "car_rental"
            | "charging_station" | "taxi" | "station" | "bus_station" | "bicycle_parking" => {
                Some(Category::Transport)
            }
            "museum" | "attraction" | "theatre" | "cinema" | "art_gallery" | "gallery"
            | "castle" | "monument" | "zoo" | "stadium" | "viewpoint" | "arts_centre"
            | "memorial" | "artwork" | "aquarium" | "theme_park" => Some(Category::Culture),
            "school" | "college" | "university" | "kindergarten" | "library" => {
                Some(Category::Education)
            }
            "place_of_worship" | "religion" => Some(Category::Worship),
            _ => None,
        };
        // Subclass (raw OSM tag value) is more precise, e.g. class=shop subclass=bakery.
        pick(subclass)
            .or_else(|| pick(class))
            .unwrap_or(Category::Other)
    }

    pub fn symbol(self) -> char {
        match self {
            Category::Food => '◆',
            Category::Drink => '◆',
            Category::Shop => '■',
            Category::Health => '+',
            Category::Money => '$',
            Category::Lodging => '⌂',
            Category::Transport => '▪',
            Category::Culture => '★',
            Category::Education => '▲',
            Category::Worship => '†',
            Category::Other => '•',
        }
    }

    pub fn color(self) -> Color {
        match self {
            Category::Food => Color::Rgb(255, 110, 90),
            Category::Drink => Color::Rgb(240, 190, 60),
            Category::Shop => Color::Rgb(220, 120, 235),
            Category::Health => Color::Rgb(110, 220, 120),
            Category::Money => Color::Rgb(130, 200, 130),
            Category::Lodging => Color::Rgb(100, 190, 240),
            Category::Transport => Color::Rgb(120, 150, 255),
            Category::Culture => Color::Rgb(250, 230, 120),
            Category::Education => Color::Rgb(205, 165, 115),
            Category::Worship => Color::Rgb(200, 200, 220),
            Category::Other => Color::Rgb(165, 165, 165),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Category::Food => "food",
            Category::Drink => "drinks",
            Category::Shop => "shops",
            Category::Health => "health",
            Category::Money => "money",
            Category::Lodging => "lodging",
            Category::Transport => "transport",
            Category::Culture => "culture",
            Category::Education => "education",
            Category::Worship => "worship",
            Category::Other => "other",
        }
    }
}

/// A named point of interest currently on screen.
#[derive(Clone, Debug)]
pub struct Poi {
    pub name: String,
    pub kind: String,
    pub category: Category,
    pub x: f64,
    pub y: f64,
    pub dist_px: f64,
}

/// A highlighted location (search result, selected POI).
pub struct Marker {
    pub x: f64,
    pub y: f64,
    pub label: String,
}

struct Placed {
    tile: Arc<Tile>,
    z: u8,
    ox: f64,
    oy: f64,
    size: f64,
    exact: bool,
}

impl Placed {
    fn project(&self, extent: u32, pts: &[[i32; 2]]) -> Vec<(f64, f64)> {
        let s = self.size / extent as f64;
        pts.iter()
            .map(|p| (self.ox + p[0] as f64 * s, self.oy + p[1] as f64 * s))
            .collect()
    }

    fn clip(&self, screen: Clip) -> Option<Clip> {
        Clip {
            x0: self.ox,
            y0: self.oy,
            x1: self.ox + self.size,
            y1: self.oy + self.size,
        }
        .intersect(screen)
    }

    fn features<'a>(&'a self, layer: &str) -> impl Iterator<Item = (&'a Feature, u32)> + 'a {
        self.tile
            .layer(layer)
            .into_iter()
            .flat_map(|l| l.features.iter().map(move |f| (f, l.extent)))
    }
}

enum LabelKind {
    Centered(String),
    Marker(char, String),
}

struct Candidate {
    prio: i64,
    x: f64,
    y: f64,
    kind: LabelKind,
    style: Style,
}

/// Render the map into `canvas`, request missing tiles, and return visible POIs (nearest first).
pub fn draw(
    canvas: &mut Canvas,
    view: &View,
    tiles: &mut TileManager,
    filter: Option<Category>,
    markers: &[Marker],
) -> Vec<Poi> {
    let (w, h) = (canvas.width_px(), canvas.height_px());
    let screen = canvas.bounds();
    let ws = view.world_px();
    let left = view.x - w / 2.0 / ws;
    let top = view.y - h / 2.0 / ws;
    let zoom = view.zoom;

    let placed = collect_tiles(view, tiles, left, top, w, h);

    // Area fills.
    for p in &placed {
        let Some(clip) = p.clip(screen) else { continue };
        if zoom >= 8.0 {
            for (f, ext) in p.features("landcover") {
                if matches!(f.props.class.as_deref(), Some("wood" | "forest")) {
                    fill(canvas, p, f, ext, &clip, WOOD);
                }
            }
            for (f, ext) in p.features("park") {
                fill(canvas, p, f, ext, &clip, PARK);
            }
        }
        for (f, ext) in p.features("water") {
            fill(canvas, p, f, ext, &clip, WATER);
        }
        if zoom >= 15.0 {
            for (f, ext) in p.features("building") {
                fill(canvas, p, f, ext, &clip, BUILDING);
            }
        }
    }

    // Lines.
    for p in &placed {
        let Some(clip) = p.clip(screen) else { continue };
        for (f, ext) in p.features("water") {
            stroke(canvas, p, f, ext, &clip, WATER_LINE);
        }
        for (f, ext) in p.features("waterway") {
            stroke(canvas, p, f, ext, &clip, WATER_LINE);
        }
        for major in [false, true] {
            for (f, ext) in p.features("transportation") {
                if let Some((color, is_major)) = road_style(f.props.class.as_deref(), zoom)
                    && is_major == major
                {
                    stroke(canvas, p, f, ext, &clip, color);
                }
            }
        }
        for (f, ext) in p.features("boundary") {
            if f.props.maritime {
                continue;
            }
            let color = match f.props.admin_level {
                Some(2) => BOUNDARY_COUNTRY,
                Some(3 | 4) if zoom >= 4.0 => BOUNDARY_STATE,
                _ => continue,
            };
            stroke(canvas, p, f, ext, &clip, color);
        }
    }

    // Labels: gather candidates from exact-zoom tiles, then place by priority.
    let mut cands = Vec::new();
    let mut pois = Vec::new();
    for p in placed.iter().filter(|p| p.exact) {
        let Some(clip) = p.clip(screen) else { continue };
        collect_labels(p, &clip, zoom, filter, &mut cands);
        if zoom >= 14.0 {
            for (f, ext) in p.features("poi") {
                let (Some(name), Some(&pt)) = (&f.props.name, point_of(f)) else {
                    continue;
                };
                let (x, y) = p.project(ext, &[pt])[0];
                if !clip.contains(x, y) {
                    continue;
                }
                let class = f.props.class.as_deref().unwrap_or("");
                let subclass = f.props.subclass.as_deref().unwrap_or(class);
                let category = Category::of(class, subclass);
                if filter.is_some_and(|c| c != category) {
                    continue;
                }
                pois.push(Poi {
                    name: name.clone(),
                    kind: subclass.replace('_', " "),
                    category,
                    x: left + x / ws,
                    y: top + y / ws,
                    dist_px: ((x - w / 2.0).powi(2) + (y - h / 2.0).powi(2)).sqrt(),
                });
            }
        }
    }

    let marker_style = Style::default()
        .fg(Color::Rgb(255, 60, 60))
        .add_modifier(Modifier::BOLD | Modifier::REVERSED);
    for m in markers {
        let (x, y) = view.screen_pos(m.x, m.y, w, h);
        if screen.contains(x, y) {
            canvas.label_marker(x, y, '◉', &m.label, marker_style);
        }
    }

    cands.sort_by_key(|c| c.prio);
    let mut road_names = HashSet::new();
    for c in cands {
        match &c.kind {
            LabelKind::Centered(text) => {
                // Road names repeat across features and tiles; label each once.
                if c.prio >= ROAD_PRIO && c.prio < HOUSE_PRIO && road_names.contains(text) {
                    continue;
                }
                if canvas.label_centered(c.x, c.y, text, c.style) && c.prio >= ROAD_PRIO {
                    road_names.insert(text.clone());
                }
            }
            LabelKind::Marker(sym, name) => {
                canvas.label_marker(c.x, c.y, *sym, name, c.style);
            }
        }
    }

    pois.sort_by(|a, b| a.dist_px.total_cmp(&b.dist_px));
    pois.dedup_by(|a, b| a.name == b.name && (a.dist_px - b.dist_px).abs() < 1.0);
    pois
}

const POI_PRIO: i64 = 5_000;
const ROAD_PRIO: i64 = 8_500;
const HOUSE_PRIO: i64 = 9_500;

fn collect_labels(
    p: &Placed,
    clip: &Clip,
    zoom: f64,
    filter: Option<Category>,
    out: &mut Vec<Candidate>,
) {
    let mut push = |f: &Feature, ext: u32, prio: i64, kind: LabelKind, style: Style| {
        let anchor = match f.kind {
            GeomType::Point => point_of(f).copied(),
            _ => line_anchor(f),
        };
        let Some(pt) = anchor else { return };
        let (x, y) = p.project(ext, &[pt])[0];
        if clip.contains(x, y) {
            out.push(Candidate {
                prio,
                x,
                y,
                kind,
                style,
            });
        }
    };

    for (f, ext) in p.features("place") {
        let Some(name) = &f.props.name else { continue };
        let class = f.props.class.as_deref().unwrap_or("");
        let (base, min_z, max_z, style) = match class {
            "continent" => (0, 0.0, 3.0, Style::new().fg(Color::White).bold()),
            "country" => (1, 1.5, 8.0, Style::new().fg(Color::White).bold()),
            "city" => (2, 3.5, 15.0, Style::new().fg(Color::White).bold()),
            "state" | "province" => (3, 4.5, 8.0, Style::new().fg(Color::Rgb(170, 140, 170))),
            "town" => (4, 7.0, 16.0, Style::new().fg(Color::Rgb(220, 220, 220))),
            "village" => (6, 10.0, 17.0, Style::new().fg(Color::Rgb(190, 190, 190))),
            "suburb" => (7, 11.0, 16.0, Style::new().fg(Color::Rgb(160, 160, 190))),
            "island" | "islet" => (9, 8.0, 20.0, Style::new().fg(Color::Rgb(170, 200, 170))),
            _ => (8, 13.5, 17.5, Style::new().fg(Color::Rgb(140, 140, 165))),
        };
        if zoom < min_z || zoom > max_z {
            continue;
        }
        let prio = base * 1000 + f.props.rank.unwrap_or(99).min(999);
        push(f, ext, prio, LabelKind::Centered(name.clone()), style);
    }

    for (f, ext) in p.features("water_name") {
        if let Some(name) = &f.props.name {
            let style = Style::new().fg(Color::Rgb(110, 150, 220)).italic();
            push(f, ext, 4_500, LabelKind::Centered(name.clone()), style);
        }
    }

    if zoom >= 15.0 {
        for (f, ext) in p.features("poi") {
            let Some(name) = &f.props.name else { continue };
            let class = f.props.class.as_deref().unwrap_or("");
            let category = Category::of(class, f.props.subclass.as_deref().unwrap_or(class));
            if filter.is_some_and(|c| c != category) {
                continue;
            }
            let prio = POI_PRIO + f.props.rank.unwrap_or(99).min(999);
            let style = Style::new().fg(category.color());
            push(
                f,
                ext,
                prio,
                LabelKind::Marker(category.symbol(), name.clone()),
                style,
            );
        }
    }

    if zoom >= 15.5 {
        for (f, ext) in p.features("transportation_name") {
            if let Some(name) = &f.props.name {
                let style = Style::new().fg(Color::Rgb(150, 150, 150)).italic();
                push(f, ext, ROAD_PRIO, LabelKind::Centered(name.clone()), style);
            }
        }
    }

    if zoom >= 18.0 {
        for (f, ext) in p.features("housenumber") {
            if let Some(num) = &f.props.housenumber {
                let style = Style::new().fg(Color::Rgb(100, 100, 110));
                push(f, ext, HOUSE_PRIO, LabelKind::Centered(num.clone()), style);
            }
        }
    }
}

/// Visible tiles at the best available zoom, falling back to cached ancestors while loading.
fn collect_tiles(
    view: &View,
    tiles: &mut TileManager,
    left: f64,
    top: f64,
    w: f64,
    h: f64,
) -> Vec<Placed> {
    let ws = view.world_px();
    let tz = (view.zoom.floor() as i64).clamp(0, MAX_ZOOM as i64) as u8;
    let n = 1i64 << tz;
    let nf = n as f64;
    let tx0 = (left * nf).floor() as i64;
    let tx1 = ((left + w / ws) * nf).floor() as i64;
    let ty0 = ((top * nf).floor() as i64).max(0);
    let ty1 = (((top + h / ws) * nf).floor() as i64).min(n - 1);

    let placed_at = |z: u8, tx: i64, ty: i64, tile: Arc<Tile>, exact: bool| {
        let size = ws / (1i64 << z) as f64;
        Placed {
            tile,
            z,
            ox: tx as f64 * size - left * ws,
            oy: ty as f64 * size - top * ws,
            size,
            exact,
        }
    };

    let mut placed = Vec::new();
    let mut wanted = Vec::new();
    let mut seen = HashSet::new();
    let (cx, cy) = (view.x * nf, view.y * nf);
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let id = TileId {
                z: tz,
                x: tx.rem_euclid(n) as u32,
                y: ty as u32,
            };
            if let Some(tile) = tiles.get(id) {
                placed.push(placed_at(tz, tx, ty, tile, true));
                continue;
            }
            let d = (tx as f64 + 0.5 - cx).powi(2) + (ty as f64 + 0.5 - cy).powi(2);
            wanted.push((d, id));
            for k in 1..=tz {
                let z = tz - k;
                let (ax, ay) = (tx.div_euclid(1 << k), ty >> k);
                let aid = TileId {
                    z,
                    x: ax.rem_euclid(1 << z) as u32,
                    y: ay as u32,
                };
                if let Some(tile) = tiles.get(aid) {
                    if seen.insert((z, ax, ay)) {
                        placed.push(placed_at(z, ax, ay, tile, false));
                    }
                    break;
                }
            }
        }
    }
    wanted.sort_by(|a, b| a.0.total_cmp(&b.0));
    tiles.request(&wanted.into_iter().map(|(_, id)| id).collect::<Vec<_>>());
    placed.sort_by_key(|p| p.z);
    placed
}

fn road_style(class: Option<&str>, zoom: f64) -> Option<(Color, bool)> {
    let (color, major, min_zoom) = match class? {
        "motorway" => (Color::Rgb(235, 150, 60), true, 0.0),
        "trunk" | "primary" => (Color::Rgb(225, 205, 110), true, 5.0),
        "secondary" | "tertiary" => (Color::Rgb(200, 200, 200), false, 9.0),
        "minor" => (Color::Rgb(140, 140, 140), false, 13.0),
        "service" | "track" => (Color::Rgb(100, 100, 100), false, 15.0),
        "path" | "pedestrian" => (Color::Rgb(125, 110, 85), false, 16.0),
        "rail" | "transit" => (Color::Rgb(150, 100, 160), false, 10.0),
        "ferry" => (Color::Rgb(70, 110, 190), false, 8.0),
        _ => return None,
    };
    (zoom >= min_zoom).then_some((color, major))
}

fn fill(canvas: &mut Canvas, p: &Placed, f: &Feature, ext: u32, clip: &Clip, color: Color) {
    if f.kind == GeomType::Polygon {
        let rings: Vec<_> = f.geom.iter().map(|r| p.project(ext, r)).collect();
        canvas.fill_polygon(&rings, clip, color);
    }
}

fn stroke(canvas: &mut Canvas, p: &Placed, f: &Feature, ext: u32, clip: &Clip, color: Color) {
    if matches!(f.kind, GeomType::Line | GeomType::Polygon) {
        for part in &f.geom {
            canvas.polyline(&p.project(ext, part), clip, color);
        }
    }
}

fn point_of(f: &Feature) -> Option<&[i32; 2]> {
    f.geom.first().and_then(|g| g.first())
}

/// Middle vertex of the longest part, used to anchor labels on lines/areas.
fn line_anchor(f: &Feature) -> Option<[i32; 2]> {
    let part = f.geom.iter().max_by_key(|g| g.len())?;
    part.get(part.len() / 2).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fly(from: View, to: View) -> Vec<View> {
        let mut flight = Flight::new(from, to, 240.0);
        let mut views = Vec::new();
        loop {
            let (view, done) = flight.step(1.0 / 60.0);
            views.push(view);
            if done {
                return views;
            }
        }
    }

    fn assert_smooth(views: &[View]) {
        for w in views.windows(2) {
            assert!((w[1].zoom - w[0].zoom).abs() < 0.5, "zoom jump {:?}", w);
        }
    }

    #[test]
    fn long_flight_zooms_out_and_lands_on_target() {
        let paris = View::new(48.8566, 2.3522, 17.0);
        let tokyo = View::new(35.6762, 139.6503, 17.0);
        let views = fly(paris, tokyo);
        assert!((views[0].zoom - paris.zoom).abs() < 0.1);
        assert!((views[0].x - paris.x).abs() < 1e-4);
        let last = views.last().unwrap();
        assert_eq!((last.x, last.y, last.zoom), (tokyo.x, tokyo.y, tokyo.zoom));
        let lowest = views.iter().map(|v| v.zoom).fold(f64::INFINITY, f64::min);
        assert!(lowest < 5.0, "lowest zoom {lowest}");
        assert!(views.len() as f64 <= FLIGHT_MAX_SECS * 60.0 + 2.0);
        assert_smooth(&views);
    }

    #[test]
    fn short_flight_stays_close_in() {
        let a = View::new(48.8530, 2.3499, 17.0);
        let b = View::new(48.8566, 2.3522, 17.0);
        let views = fly(a, b);
        let lowest = views.iter().map(|v| v.zoom).fold(f64::INFINITY, f64::min);
        assert!(lowest > 13.0, "lowest zoom {lowest}");
        assert_smooth(&views);
    }

    #[test]
    fn crosses_antimeridian_the_short_way() {
        let views = fly(View::new(0.0, 179.0, 8.0), View::new(0.0, -179.0, 8.0));
        // Never passes over longitude 0 (x = 0.5).
        assert!(views.iter().all(|v| (v.x - 0.5).abs() > 0.4));
    }

    #[test]
    fn zoom_only_flight() {
        let from = View::new(48.8566, 2.3522, 2.0);
        let to = View::new(48.8566, 2.3522, 15.0);
        let views = fly(from, to);
        assert!(views.windows(2).all(|w| w[1].zoom >= w[0].zoom - 1e-9));
        assert_eq!(views.last().unwrap().zoom, 15.0);
        assert_smooth(&views);
    }
}
