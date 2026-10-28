//! Minimal Mapbox Vector Tile (protobuf) decoder.
//! Spec: https://github.com/mapbox/vector-tile-spec/tree/master/2.1

use anyhow::{Result, bail};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GeomType {
    Unknown,
    Point,
    Line,
    Polygon,
}

/// The subset of feature properties the renderer cares about.
#[derive(Default, Debug)]
pub struct Props {
    pub name: Option<String>,
    pub class: Option<String>,
    pub subclass: Option<String>,
    pub admin_level: Option<i64>,
    pub rank: Option<i64>,
    pub housenumber: Option<String>,
    pub maritime: bool,
}

#[derive(Debug)]
pub struct Feature {
    pub kind: GeomType,
    /// Points: one part holding all points. Lines/polygons: one part per line/ring.
    pub geom: Vec<Vec<[i32; 2]>>,
    pub props: Props,
}

#[derive(Debug)]
pub struct Layer {
    pub name: String,
    pub extent: u32,
    pub features: Vec<Feature>,
}

#[derive(Debug, Default)]
pub struct Tile {
    pub layers: Vec<Layer>,
}

impl Tile {
    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }

    pub fn decode(buf: &[u8]) -> Result<Tile> {
        let mut r = Reader::new(buf);
        let mut layers = Vec::new();
        while !r.done() {
            let (field, wire) = r.key()?;
            if field == 3 && wire == 2 {
                layers.push(decode_layer(r.bytes()?)?);
            } else {
                r.skip(wire)?;
            }
        }
        Ok(Tile { layers })
    }
}

enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Value {
    fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Float(f) => Some(*f as i64),
            Value::Bool(b) => Some(*b as i64),
            Value::Str(s) => s.parse().ok(),
        }
    }

    fn as_string(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
        }
    }
}

fn decode_layer(buf: &[u8]) -> Result<Layer> {
    let mut r = Reader::new(buf);
    let mut name = String::new();
    let mut extent = 4096;
    let mut keys: Vec<&str> = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    // Features reference keys/values by index, which may come later in the message.
    let mut raw_features: Vec<&[u8]> = Vec::new();

    while !r.done() {
        let (field, wire) = r.key()?;
        match (field, wire) {
            (1, 2) => name = String::from_utf8_lossy(r.bytes()?).into_owned(),
            (2, 2) => raw_features.push(r.bytes()?),
            (3, 2) => keys.push(std::str::from_utf8(r.bytes()?).unwrap_or("")),
            (4, 2) => values.push(decode_value(r.bytes()?)?),
            (5, 0) => extent = r.varint()? as u32,
            _ => r.skip(wire)?,
        }
    }

    let mut features = Vec::with_capacity(raw_features.len());
    for raw in raw_features {
        features.push(decode_feature(raw, &keys, &values)?);
    }
    Ok(Layer {
        name,
        extent,
        features,
    })
}

fn decode_value(buf: &[u8]) -> Result<Value> {
    let mut r = Reader::new(buf);
    let mut v = Value::Bool(false);
    while !r.done() {
        let (field, wire) = r.key()?;
        v = match (field, wire) {
            (1, 2) => Value::Str(String::from_utf8_lossy(r.bytes()?).into_owned()),
            (2, 5) => Value::Float(f32::from_bits(r.fixed32()?) as f64),
            (3, 1) => Value::Float(f64::from_bits(r.fixed64()?)),
            (4, 0) | (5, 0) => Value::Int(r.varint()? as i64),
            (6, 0) => Value::Int(zigzag64(r.varint()?)),
            (7, 0) => Value::Bool(r.varint()? != 0),
            _ => {
                r.skip(wire)?;
                continue;
            }
        };
    }
    Ok(v)
}

fn decode_feature(buf: &[u8], keys: &[&str], values: &[Value]) -> Result<Feature> {
    let mut r = Reader::new(buf);
    let mut tags = Vec::new();
    let mut geometry = Vec::new();
    let mut kind = GeomType::Unknown;

    while !r.done() {
        let (field, wire) = r.key()?;
        match (field, wire) {
            (2, 2) => r.packed(&mut tags)?,
            (2, 0) => tags.push(r.varint()? as u32),
            (3, 0) => {
                kind = match r.varint()? {
                    1 => GeomType::Point,
                    2 => GeomType::Line,
                    3 => GeomType::Polygon,
                    _ => GeomType::Unknown,
                }
            }
            (4, 2) => r.packed(&mut geometry)?,
            (4, 0) => geometry.push(r.varint()? as u32),
            _ => r.skip(wire)?,
        }
    }

    let mut props = Props::default();
    let mut fallback_name = None;
    for pair in tags.chunks_exact(2) {
        let (Some(key), Some(val)) = (keys.get(pair[0] as usize), values.get(pair[1] as usize))
        else {
            continue;
        };
        match *key {
            "name" => props.name = Some(val.as_string()),
            "name:latin" | "name_en" if fallback_name.is_none() => {
                fallback_name = Some(val.as_string())
            }
            "class" => props.class = Some(val.as_string()),
            "subclass" => props.subclass = Some(val.as_string()),
            "admin_level" => props.admin_level = val.as_i64(),
            "rank" => props.rank = val.as_i64(),
            "housenumber" => props.housenumber = Some(val.as_string()),
            "maritime" => props.maritime = val.as_i64() == Some(1),
            _ => {}
        }
    }
    if props.name.is_none() {
        props.name = fallback_name;
    }
    // Some names hold several values ("Afrika;أفريقيا"); keep the first.
    props.name = props
        .name
        .map(|n| n.split(';').next().unwrap_or_default().trim().to_owned())
        .filter(|n| !n.is_empty());

    Ok(Feature {
        kind,
        geom: decode_geometry(&geometry, kind),
        props,
    })
}

fn decode_geometry(g: &[u32], kind: GeomType) -> Vec<Vec<[i32; 2]>> {
    let mut parts = Vec::new();
    let mut cur: Vec<[i32; 2]> = Vec::new();
    let (mut x, mut y) = (0i32, 0i32);
    let mut i = 0;
    while i < g.len() {
        let cmd = g[i] & 0x7;
        let count = (g[i] >> 3) as usize;
        i += 1;
        match cmd {
            1 | 2 => {
                for _ in 0..count {
                    if i + 1 >= g.len() {
                        break;
                    }
                    x = x.wrapping_add(zigzag32(g[i]));
                    y = y.wrapping_add(zigzag32(g[i + 1]));
                    i += 2;
                    if cmd == 1 && kind != GeomType::Point && !cur.is_empty() {
                        parts.push(std::mem::take(&mut cur));
                    }
                    cur.push([x, y]);
                }
            }
            7 => {
                if let Some(&first) = cur.first() {
                    cur.push(first);
                }
            }
            _ => break,
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

fn zigzag32(n: u32) -> i32 {
    ((n >> 1) as i32) ^ -((n & 1) as i32)
}

fn zigzag64(n: u64) -> i64 {
    ((n >> 1) as i64) ^ -((n & 1) as i64)
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn varint(&mut self) -> Result<u64> {
        let mut out = 0u64;
        for shift in (0..64).step_by(7) {
            let Some(&b) = self.buf.get(self.pos) else {
                bail!("truncated varint");
            };
            self.pos += 1;
            out |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(out);
            }
        }
        bail!("varint too long")
    }

    fn key(&mut self) -> Result<(u32, u8)> {
        let k = self.varint()?;
        Ok(((k >> 3) as u32, (k & 7) as u8))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len());
        let Some(end) = end else {
            bail!("truncated message");
        };
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.varint()? as usize;
        self.take(n)
    }

    fn fixed32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn fixed64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn packed(&mut self, out: &mut Vec<u32>) -> Result<()> {
        let mut inner = Reader::new(self.bytes()?);
        while !inner.done() {
            out.push(inner.varint()? as u32);
        }
        Ok(())
    }

    fn skip(&mut self, wire: u8) -> Result<()> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => {
                self.take(8)?;
            }
            2 => {
                self.bytes()?;
            }
            5 => {
                self.take(4)?;
            }
            _ => bail!("unsupported wire type {wire}"),
        }
        Ok(())
    }
}
