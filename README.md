# 🗺️ osomty

> [!NOTE]
> 🤖 *bip boop ai*
>
> This project is vibe coded: it was written almost entirely by an AI coding assistant (Claude Code), with only light human review. Expect rough edges.

Browse the OpenStreetMap world map in your terminal, from continents down to the names of shops, cafés and restaurants on a street.

Inspired by [mapscii](https://github.com/rastapasta/mapscii) (`telnet mapscii.me`), built in Rust with [ratatui](https://ratatui.rs). The map is drawn with braille characters, and labels are placed so they don't overlap.

```
       ⠉⠑⠢⢄⡀      ⢰⠁   ⢀⠎  ⠉⠒⢤⣎⠉⠑⠢⢄⡈⠉⠒⠤⣉⠑⠢⢍⡑⠢⢄⡀   ■ Aux chimères de Notre-Dame
           ⠈⠑⠢⢄⡀ ⢠⠃   ⢀⠎ ⡠⠔⠑⠒⠪⣴⡁   ⠈⠑⠢⢄⡀⠉⠒⠤⣈⠑⠢⢌⣑⠢⠤⣀⡸⡲⢄
               ⠈⠑⠣⠤⣀ ⢀⣎⠔⠊  ⢠⠒⠒⠁⠈⠑⠢⢄⡀   ⠈⠑⠢⠤⣀⠉⠒⠤⣀⡉⠒⠤⣱⠁ ⡱⠢⣀ ◆ Crêperie du Cloître
  Parvis Notre-Dame - Place Jean-Paul II    ⠉⠒⠤⣀⠈⠑⢢⢇⡉⡲⠥⣀⡑⠢⢄⡀
                   ⢀⠎    ⢀⠇             ⠉⠒⠤⣀    ⠉⠲⠧⣀⡸⠑⠢⢄⡈⠑⠢⢌⡑⠢⢄⡀         † Archevêché de Paris
        ★ Point zéro des Routes de France   ⠉⠒⠤⣀⡀   ⠉⠒⠢⢄⡈⠑⠢⢄⣈⠑⠢⢌⣑⠒⠤⣀
                 ⢀⠎    ⢀⠎       ★ Tours de Notre-Dame   ⠈⠑⠢⢄⡀⠉⠒⠤⣀⠉⠒⠤⣉⠒⠤⣀
                ⢀⠎    ⢀⠎                            ⠈⠑⠢⢄⡀   ⠈⠑⠢⢄⡀⠉⠒⠤⣀⠉⠒⠤⢍⡒⠤⣀
               ⢀⠎    ⢀⠎                                 ⠈⠑⠒⠤⣀   ⠈⠉⠒⠤⣀⠉⠒⠤⣀⡈⠑⠢⢍⡑⠢⢄⡀ ◆ La Rosace
               ⡎    ⢀⠎ ★ Galerie des Rois                    ⠉⠑⠒⠤⣀   ⠉⠒⠤⣀⠈⠑⠢⢄⡈⠑⠢⠬⣑⠢⢄⡀   ■ Au Jongleur de Notre Dame
```

*Île de la Cité, Paris, at zoom 17. In the terminal, water, parks and buildings are coloured and labels are colour-coded by category.*

## Features

- Map data from OpenStreetMap: vector tiles from [OpenFreeMap](https://openfreemap.org), no API key needed
- Smooth zoom from the whole world (zoom 0) to street level (zoom 19.5), centred on the mouse cursor
- Points of interest (shops, restaurants, cafés, bars, museums, pharmacies, …) with a symbol and colour per category, from zoom 15
- Street names from zoom 15.5, house numbers from zoom 18
- Filter points of interest by category
- Nearby places panel listing named places on screen, nearest first
- Place search with [Nominatim](https://nominatim.org), or jump to `lat, lon` coordinates
- Tiles download in the background, and a lower-zoom version is shown until the detailed one arrives
- Downloaded tiles are cached on disk, so areas you've already seen work offline

## Installation

Requires a [Rust toolchain](https://rustup.rs) (edition 2024, Rust 1.88 or newer).

```sh
cargo install --git https://github.com/coko7/osomty
```

Or from a clone:

```sh
git clone https://github.com/coko7/osomty
cd osomty
cargo run --release
```

The map uses 24-bit colours and braille characters. Use a terminal with truecolor support and a font that has braille glyphs.

## Usage

```sh
osomty                      # world view
osomty Paris                # search a place and jump to the first result
osomty "Café de Flore"
osomty 48.8530 2.3499       # latitude longitude (zoom 16)
osomty 48.8530 2.3499 18    # latitude longitude zoom
```

### Map keys

| Key | Action |
| --- | --- |
| `←` `↑` `↓` `→` / `h` `j` `k` `l` | Pan |
| `+` `-` / `a` `z` | Zoom in / out |
| Mouse drag / wheel | Pan / zoom around the cursor |
| `/` or `s` | Search |
| `p` | Show / hide the nearby places panel |
| `Tab` | Focus the nearby places panel |
| `f` / `F` | Next / previous category filter |
| `Esc` | Clear the marker |
| `r` | Back to the world view |
| `?` | Help |
| `q` / `Ctrl+C` | Quit |

### Nearby places panel

| Key | Action |
| --- | --- |
| `↑` `↓` / `j` `k` | Select a place (highlighted on the map) |
| `Enter` | Go to the selected place |
| `PgUp` `PgDn` / `g` | Scroll / back to top |
| `Tab` / `Esc` | Back to the map |

### Search

| Key | Action |
| --- | --- |
| `Enter` | Search; press again to go to the selected result |
| `↑` `↓` / `Tab` | Select a result |
| `Ctrl+W` / `Ctrl+Backspace` / `Alt+Backspace` | Delete the previous word |
| `Ctrl+Delete` / `Alt+D` | Delete the next word |
| `Ctrl+←` `Ctrl+→` / `Alt+B` `Alt+F` | Move by word |
| `Home` `End` / `Ctrl+A` `Ctrl+E` | Start / end of the line |
| `Ctrl+U` / `Ctrl+K` | Delete to the start / end of the line |
| `Esc` | Close |

### Place categories

| Symbol | Category | Symbol | Category |
| --- | --- | --- | --- |
| `◆` red | Food | `▪` | Transport |
| `◆` yellow | Drinks | `★` | Culture |
| `■` | Shops | `▲` | Education |
| `+` | Health | `†` | Worship |
| `$` | Money | `•` | Other |
| `⌂` | Lodging | | |

## Data, caching and network

- **Map tiles:** [OpenFreeMap](https://openfreemap.org) planet tiles, using the [OpenMapTiles](https://openmaptiles.org/schema/) schema. The most detailed tiles are at zoom 14, and higher zooms enlarge them.
- **Search:** the public [Nominatim](https://nominatim.openstreetmap.org) instance. Each search is one request. Please follow its [usage policy](https://operations.osmfoundation.org/policies/nominatim/).
- **Cache:** tiles are stored in your system cache directory (`~/.cache/osomty` on Linux, `~/Library/Caches/osomty` on macOS, `%LOCALAPPDATA%\osomty` on Windows). Each OpenFreeMap release gets its own folder, and old folders are not cleaned up automatically, so it is safe to delete them.
- **Proxy:** `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` are respected.

## Attribution

Map data © [OpenStreetMap contributors](https://www.openstreetmap.org/copyright), available under the Open Database License. Tiles served by [OpenFreeMap](https://openfreemap.org). The attribution is shown in the app's status bar.

## License

[MIT](LICENSE)
