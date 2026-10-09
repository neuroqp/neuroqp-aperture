use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeLevel {
    pub options: String,
    pub width: u32,
    pub height: u32,
}

fn field<'a>(header: &'a str, key: &str) -> Option<&'a str> {
    header.lines().find_map(|line| {
        let (name, value) = line.split_once(": ")?;
        (name == key).then_some(value.trim())
    })
}

fn number(header: &str, key: &str) -> Option<u32> {
    field(header, key)?.parse().ok()
}

fn level(header: &str, options: String, tiled_fallback: bool) -> Option<NativeLevel> {
    let width = number(header, "width")?;
    let height = number(header, "height")?;
    if width == 0
        || height == 0
        || !match (number(header, "tile-width"), number(header, "tile-height")) {
            (Some(width), Some(height)) => width > 0 && height > 0,
            (None, None) => tiled_fallback,
            _ => false,
        }
    {
        return None;
    }
    Some(NativeLevel {
        options,
        width,
        height,
    })
}

fn reduced(base: &NativeLevel, candidate: &NativeLevel) -> bool {
    (1..=28).any(|shift| {
        let factor = 1_u32 << shift;
        candidate.width >= 1
            && candidate.height >= 1
            && candidate.width < base.width
            && candidate.height < base.height
            && (base.width / factor..=base.width.div_ceil(factor)).contains(&candidate.width)
            && (base.height / factor..=base.height.div_ceil(factor)).contains(&candidate.height)
    })
}

pub fn valid_cached_levels(levels: &[NativeLevel], width: u32, height: u32) -> bool {
    let Some(base) = levels.first() else {
        return false;
    };
    levels.len() >= 2
        && base.width == width
        && base.height == height
        && base.options == "[page=0,n=1,access=random]"
        && levels.windows(2).all(|pair| {
            pair[0].width >= pair[1].width
                && pair[0].height >= pair[1].height
                && (pair[0].width != pair[1].width || pair[0].height != pair[1].height)
        })
        && levels.iter().skip(1).all(|candidate| {
            let Some(options) = candidate
                .options
                .strip_prefix("[page=")
                .and_then(|s| s.strip_suffix(",access=random]"))
            else {
                return false;
            };
            let Some((page, remainder)) = options.split_once(",n=1") else {
                return false;
            };
            page.parse::<u32>().is_ok_and(|page| page < 256)
                && (remainder.is_empty()
                    || remainder.strip_prefix(",subifd=").is_some_and(|index| {
                        page == "0" && index.parse::<u32>().is_ok_and(|index| index < 28)
                    }))
                && reduced(base, candidate)
        })
}

// libvips before 8.18 omits tile dimensions in vipsheader. libtiff's metadata
// report supplies layout without pixel decoding; never infer tiled layout from
// reduced dimensions alone. Only the first descriptor of each SubIFD chain maps
// to libvips' subifd array index.
fn tiled_directories(info: &str) -> Vec<(u32, Option<u32>)> {
    let mut page = None;
    let mut directory = None;
    let mut tiled = Vec::new();
    for line in info.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("=== TIFF directory ") {
            page = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok());
            directory = page.map(|page| (page, None));
        } else if let Some(rest) = line.strip_prefix("--- SubIFD ") {
            let words: Vec<_> = rest.split_whitespace().collect();
            if words.len() >= 5 && words[1] == "of" && words[2] == "chain" {
                directory = if words[0] == "0" {
                    words[3]
                        .parse()
                        .ok()
                        .and_then(|subifd| page.map(|page| (page, Some(subifd))))
                } else {
                    None
                };
            }
        } else if let Some(rest) = line.strip_prefix("Tile Width: ") {
            if let Some((width, height)) = rest.split_once(" Tile Length: ") {
                if width.parse::<u32>().unwrap_or(0) > 0 && height.parse::<u32>().unwrap_or(0) > 0 {
                    if let Some(directory) = directory {
                        tiled.push(directory);
                    }
                }
            }
        }
    }
    tiled
}

// TIFF page numbers carry no scene/channel meaning. Only pages explicitly marked
// reduced resolution are eligible; SubIFDs already describe child resolutions.
fn reduced_pages(info: &str) -> Vec<u32> {
    let mut page = None;
    let mut pages = Vec::new();
    for line in info.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("=== TIFF directory ") {
            page = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok());
        } else if line.starts_with("--- SubIFD") {
            page = None;
        } else if line.starts_with("Subfile Type:") && line.contains("reduced-resolution image") {
            if let Some(index) = page.filter(|index| *index > 0) {
                if !pages.contains(&index) {
                    pages.push(index);
                }
            }
        }
    }
    pages
}

/// Returns base plus validated reduced levels, largest first. Empty means use
/// background conversion. Inspection only reads metadata, never source pixels.
pub fn discover(
    header: &str,
    inspect: &dyn Fn(&str) -> Result<String, String>,
    tiffinfo: Option<&str>,
) -> Vec<NativeLevel> {
    discover_levels(header, inspect, tiffinfo, false)
}

/// Validate an overview near the existing 1024-pixel preview size first. Levels wait
/// until first pixels are available; scientific page-selection rules are shared.
pub fn discover_preview(
    header: &str,
    inspect: &dyn Fn(&str) -> Result<String, String>,
    tiffinfo: Option<&str>,
) -> Vec<NativeLevel> {
    discover_levels(header, inspect, tiffinfo, true)
}

fn discover_levels(
    header: &str,
    inspect: &dyn Fn(&str) -> Result<String, String>,
    tiffinfo: Option<&str>,
    preview_only: bool,
) -> Vec<NativeLevel> {
    if field(header, "vips-loader") != Some("tiffload") {
        return Vec::new();
    }
    let tiled = tiled_directories(tiffinfo.unwrap_or(""));
    let Some(base) = level(
        header,
        "[page=0,n=1,access=random]".into(),
        tiled.contains(&(0, None)),
    ) else {
        return Vec::new();
    };
    let mut levels = vec![base.clone()];
    if preview_only && base.width.max(base.height) <= 1024 {
        return levels;
    }
    let subifds = number(header, "n-subifds").unwrap_or(0);
    let mut options: Vec<_> = if (1..=28).contains(&subifds) {
        (0..subifds)
            .map(|index| {
                (
                    format!("[page=0,n=1,subifd={index},access=random]"),
                    (0, Some(index)),
                )
            })
            .collect()
    } else {
        let count = number(header, "n-pages").unwrap_or(1);
        // ponytail: metadata probing is capped at 256 pages; add explicit scene
        // selection before supporting files with larger scientific page sets.
        reduced_pages(tiffinfo.unwrap_or(""))
            .into_iter()
            .filter(|index| *index < count.min(256))
            .map(|index| (format!("[page={index},n=1,access=random]"), (index, None)))
            .collect()
    };
    if preview_only {
        // TIFF pyramids normally list successive halvings. Probe the index near
        // the target size, then fall back to other validated candidates if that
        // convention does not hold. Never trade first-preview detail for speed.
        let mut size = base.width.max(base.height);
        let mut shift: usize = 0;
        while size > 1024 {
            size = size.div_ceil(2);
            shift += 1;
        }
        let preferred = shift.saturating_sub(1);
        if preferred < options.len() {
            options.swap(0, preferred);
        }
    }
    for (options, directory) in options {
        let Ok(candidate_header) = inspect(&options) else {
            continue;
        };
        if ["bands", "format", "interpretation"]
            .into_iter()
            .any(|key| field(header, key) != field(&candidate_header, key))
            || number(header, "orientation").unwrap_or(1)
                != number(&candidate_header, "orientation").unwrap_or(1)
        {
            continue;
        }
        let Some(candidate) = level(&candidate_header, options, tiled.contains(&directory)) else {
            continue;
        };
        if reduced(&base, &candidate)
            && !levels.iter().any(|existing| {
                existing.width == candidate.width && existing.height == candidate.height
            })
        {
            levels.push(candidate);
            if preview_only
                && (512..=1024).contains(
                    &levels
                        .last()
                        .unwrap()
                        .width
                        .max(levels.last().unwrap().height),
                )
            {
                break;
            }
        }
    }
    if levels.len() < 2 {
        return Vec::new();
    }
    levels.sort_by_key(|level| std::cmp::Reverse(level.width));
    levels
}

#[cfg(test)]
mod tests {
    use super::*;
    fn header(width: u32, height: u32, extra: &str) -> String {
        format!("width: {width}\nheight: {height}\nbands: 3\nformat: uchar\ninterpretation: srgb\nvips-loader: tiffload\ntile-width: 16\ntile-height: 16\norientation: 1\n{extra}")
    }

    #[test]
    fn preview_probes_target_sized_overview_without_accepting_unrelated_pages() {
        let base = header(8192, 6144, "n-subifds: 3\n");
        let probes = std::cell::RefCell::new(Vec::new());
        let levels = discover_preview(
            &base,
            &|options| {
                probes.borrow_mut().push(options.to_string());
                Ok(header(1024, 768, ""))
            },
            None,
        );
        assert_eq!(probes.borrow().len(), 1);
        assert!(levels[1].options.contains("subifd=2"));
        assert!(valid_cached_levels(&levels, 8192, 6144));
        let mut corrupt = levels;
        corrupt[1].options = "[page=0,n=-1,access=random]".into();
        assert!(!valid_cached_levels(&corrupt, 8192, 6144));
        assert!(discover_preview(
            &header(8192, 6144, "n-pages: 3\n"),
            &|_| panic!("ordinary pages must not be probed"),
            None
        )
        .is_empty());
    }

    #[test]
    fn native_levels_require_matching_dyadic_geometry_and_pixels() {
        let base = header(129, 97, "n-subifds: 3\n");
        let levels = discover(
            &base,
            &|options| {
                Ok(if options.contains("subifd=0") {
                    header(65, 49, "")
                } else if options.contains("subifd=1") {
                    header(33, 25, "").replace("bands: 3", "bands: 1")
                } else {
                    header(20, 15, "")
                })
            },
            None,
        );
        assert_eq!(levels.len(), 2);
        assert_eq!((levels[1].width, levels[1].height), (65, 49));
        assert!(levels[1].options.contains("subifd=0"));
    }

    #[test]
    fn older_vips_requires_explicit_tiff_layout_metadata() {
        let without_tiles = |width, height, extra| {
            header(width, height, extra).replace("tile-width: 16\ntile-height: 16\n", "")
        };
        let base = without_tiles(129, 97, "n-subifds: 2\n");
        let inspect = |options: &str| {
            Ok(if options.contains("subifd=0") {
                without_tiles(65, 49, "")
            } else {
                without_tiles(33, 25, "")
            })
        };
        let info="=== TIFF directory 0 ===\n  Tile Width: 16 Tile Length: 16\n--- SubIFD image descriptor tag within TIFF directory 0 with array of 2 SubIFD chains ---\n--- SubIFD 0 of chain 0 at offset 0x10 (16):\n  Tile Width: 16 Tile Length: 16\n--- SubIFD 1 of chain 0 at offset 0x20 (32):\n  Tile Width: 16 Tile Length: 16\n--- SubIFD 0 of chain 1 at offset 0x30 (48):\n  Rows/Strip: 25\n";
        assert!(discover(&base, &inspect, None).is_empty());
        let levels = discover(&base, &inspect, Some(info));
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[1].width, 65);
        assert!(discover(
            &base.replace(
                "n-subifds: 2",
                "tile-width: 0\ntile-height: 0\nn-subifds: 2"
            ),
            &inspect,
            Some(info)
        )
        .is_empty());
    }
    #[test]
    fn ordinary_pages_are_not_pyramids_even_when_smaller() {
        let base = header(128, 96, "n-pages: 3\n");
        let inspect = |_: &str| Ok(header(64, 48, ""));
        assert!(discover(&base, &inspect, None).is_empty());
        let info = "=== TIFF directory 0 ===\n--- SubIFD 0 of chain 0\nSubfile Type: reduced-resolution image (1 = 0x1)\n=== TIFF directory 1 ===\nSubfile Type: (0 = 0x0)\n=== TIFF directory 2 ===\nSubfile Type: reduced-resolution image (1 = 0x1)\n";
        let levels = discover(&base, &inspect, Some(info));
        assert_eq!(levels.len(), 2);
        assert!(levels[1].options.contains("page=2"));
        assert!(discover(
            &base.replace("tile-width: 16", "tile-width: 0"),
            &inspect,
            Some(info)
        )
        .is_empty());
    }
}
